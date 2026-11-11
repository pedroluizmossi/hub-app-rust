//! Executor de comandos com allowlist por mini-app.
//!
//! O mini-app só roda os programas declarados em `permissions.exec` do manifesto, e os
//! argumentos vão direto para o processo, sem shell — nada de interpolar strings.

use std::ffi::OsStr;
use std::fmt;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Limite de uma ação pedida pelo usuário (`docker stop` espera o container sair;
/// `compose restart` numa stack grande demora).
pub const ACTION_TIMEOUT: Duration = Duration::from_secs(180);

/// Por que um comando não devolveu saída.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecError {
    /// O mini-app não declarou o programa em `permissions.exec`.
    Denied(String),
    /// O programa não está instalado (não está no PATH).
    NotFound(String),
    /// Não terminou no prazo e foi morto.
    Timeout { program: String, after: Duration },
    /// Saiu com erro: a mensagem é o stderr (ou o código de saída).
    Failed(String),
}

impl ExecError {
    pub fn not_found(&self) -> bool {
        matches!(self, ExecError::NotFound(_))
    }

    /// Primeira linha da mensagem, curta o bastante para a tela.
    pub fn brief(&self) -> String {
        self.to_string().lines().next().unwrap_or_default().chars().take(160).collect()
    }
}

impl fmt::Display for ExecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecError::Denied(msg) | ExecError::Failed(msg) => f.write_str(msg),
            ExecError::NotFound(program) => write!(f, "{program} não está instalado"),
            ExecError::Timeout { program, after } => {
                write!(f, "{program} não respondeu em {} s", after.as_secs())
            }
        }
    }
}

impl std::error::Error for ExecError {}

#[derive(Debug, Clone)]
pub struct Exec {
    app: String,
    allowed: Vec<String>,
}

impl Exec {
    pub fn new(app: &str, allowed: &[String]) -> Self {
        Exec { app: app.to_string(), allowed: allowed.to_vec() }
    }

    fn check(&self, program: &str) -> Result<(), ExecError> {
        if self.allowed.iter().any(|p| p == program) {
            Ok(())
        } else {
            Err(ExecError::Denied(format!(
                "o mini-app {} não tem permissão para executar \"{program}\"",
                self.app
            )))
        }
    }

    /// Roda em segundo plano (thread própria) e chama `done` com o stdout, ou com o erro.
    /// O comando é morto se passar de `ACTION_TIMEOUT`.
    pub fn run_async<F>(&self, program: &str, args: &[&str], done: F)
    where
        F: FnOnce(Result<String, ExecError>) + Send + 'static,
    {
        if let Err(e) = self.check(program) {
            done(Err(e));
            return;
        }
        let program = program.to_string();
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        std::thread::spawn(move || done(run_timeout(&program, &args, ACTION_TIMEOUT)));
    }

    /// Abre um terminal rodando `argv` (o programa precisa estar na allowlist).
    pub fn open_terminal(&self, title: &str, argv: &[&str]) -> anyhow::Result<()> {
        let program = argv.first().ok_or_else(|| anyhow::anyhow!("comando vazio"))?;
        self.check(program)?;
        let (term, prefix) = terminal().ok_or_else(|| anyhow::anyhow!("nenhum terminal encontrado (defina $TERMINAL)"))?;
        let mut cmd = Command::new(&term);
        if term.ends_with("konsole") {
            cmd.args(["-p", &format!("tabtitle={title}")]);
        } else if term.ends_with("xfce4-terminal") || term.ends_with("gnome-terminal") {
            cmd.args(["--title", title]);
        }
        cmd.args(prefix).args(argv).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        cmd.spawn()?;
        Ok(())
    }
}

/// O que um comando escreveu, tenha saído bem ou não.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    /// "exit status: 1", para a mensagem de erro quando o stderr vem vazio.
    status: String,
}

/// Roda um programa e devolve o stdout; se passar de `limit`, mata e devolve `Timeout`.
/// Uso interno do host (provedores) e base do `Exec::run_async`.
pub fn run_timeout<S: AsRef<OsStr>>(program: &str, args: &[S], limit: Duration) -> Result<String, ExecError> {
    let out = output_timeout(program, args, limit)?;
    if out.success {
        Ok(out.stdout)
    } else {
        let err = out.stderr.trim();
        Err(ExecError::Failed(if err.is_empty() { format!("{program} saiu com {}", out.status) } else { err.to_string() }))
    }
}

/// Como `run_timeout`, mas devolve a saída mesmo quando o programa sai com erro (o
/// `docker inspect` de vários ids falha se um sumiu, e os outros vêm no stdout).
///
/// O filho ganha um grupo de processos próprio: ao estourar o prazo o grupo inteiro
/// morre, e nenhum neto fica segurando os pipes (o que prenderia as leitoras).
pub fn output_timeout<S: AsRef<OsStr>>(program: &str, args: &[S], limit: Duration) -> Result<Output, ExecError> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ExecError::NotFound(program.to_string()),
            _ => ExecError::Failed(format!("{program}: {e}")),
        })?;

    // Uma leitora por pipe (senão um pipe cheio trava o filho); cada uma avisa quando
    // o seu fecha, e é esse aviso que esperamos com prazo, sem ficar acordando à toa.
    let (tx, rx) = mpsc::channel::<(bool, Vec<u8>)>();
    let mut readers = 0;
    let pipes: [(bool, Option<Box<dyn Read + Send>>); 2] = [
        (true, child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>)),
        (false, child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>)),
    ];
    for (is_stdout, pipe) in pipes {
        let Some(mut pipe) = pipe else { continue };
        let tx = tx.clone();
        readers += 1;
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            let _ = tx.send((is_stdout, buf));
        });
    }
    drop(tx);

    let deadline = Instant::now() + limit;
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let mut timed_out = false;
    for _ in 0..readers {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok((true, buf)) => stdout = buf,
            Ok((false, buf)) => stderr = buf,
            Err(_) => {
                timed_out = true;
                break;
            }
        }
    }
    // pipes fechados: o filho está saindo (ou já saiu); o resto do prazo é para ele terminar
    let status = loop {
        if timed_out {
            break None;
        }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => break None,
        }
    };
    let Some(status) = status else {
        kill_group(&mut child);
        return Err(ExecError::Timeout { program: program.to_string(), after: limit });
    };
    Ok(Output {
        success: status.success(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        status: status.to_string(),
    })
}

/// Mata o filho e o grupo dele, e recolhe o processo.
fn kill_group(child: &mut std::process::Child) {
    // SAFETY: `kill` só envia um sinal; o pid é o do filho que ainda não recolhemos, que é
    // líder do próprio grupo (`process_group(0)`).
    unsafe {
        libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Abre um endereço http(s) no navegador padrão (serviço do host, fora da allowlist).
pub fn open_url(url: &str) {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return;
    }
    let _ = Command::new("xdg-open").arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}

pub fn which(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|d| d.join(program).is_file()))
        .unwrap_or(false)
}

/// Terminal disponível e o argumento que separa o comando a executar.
fn terminal() -> Option<(String, &'static [&'static str])> {
    if let Ok(t) = std::env::var("TERMINAL") {
        if which(&t) || std::path::Path::new(&t).is_file() {
            return Some((t, &["-e"]));
        }
    }
    const KNOWN: &[(&str, &[&str])] = &[
        ("konsole", &["-e"]),
        ("gnome-terminal", &["--"]),
        ("kgx", &["--"]),
        ("ptyxis", &["--"]),
        ("xfce4-terminal", &["-x"]),
        ("mate-terminal", &["-x"]),
        ("tilix", &["-e"]),
        ("alacritty", &["-e"]),
        ("kitty", &[]),
        ("foot", &[]),
        ("wezterm", &["start", "--"]),
        ("xterm", &["-e"]),
        ("x-terminal-emulator", &["-e"]),
    ];
    KNOWN.iter().find(|(t, _)| which(t)).map(|(t, p)| (t.to_string(), *p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bloqueia_programa_fora_da_allowlist() {
        let e = Exec::new("teste", &["docker".into()]);
        assert!(e.check("docker").is_ok());
        assert!(e.check("rm").is_err());
        assert!(e.open_terminal("x", &["bash"]).is_err());
    }

    #[test]
    fn devolve_saida_erro_e_programa_ausente() {
        let limit = Duration::from_secs(5);
        assert_eq!(run_timeout("sh", &["-c", "echo oi"], limit).unwrap(), "oi\n");
        assert_eq!(
            run_timeout("sh", &["-c", "echo ruim >&2; exit 3"], limit),
            Err(ExecError::Failed("ruim".into()))
        );
        let ausente = run_timeout("miniutils-programa-que-nao-existe", &[""; 0], limit).unwrap_err();
        assert!(ausente.not_found());
        assert_eq!(ausente.to_string(), "miniutils-programa-que-nao-existe não está instalado");
        // saída com erro, mas o stdout vem junto
        let out = output_timeout("sh", &["-c", "echo parte; exit 1"], limit).unwrap();
        assert!(!out.success);
        assert_eq!(out.stdout, "parte\n");
    }

    #[test]
    fn mata_o_que_passa_do_prazo() {
        let inicio = Instant::now();
        // o `sleep` é neto (filho do sh): morre junto, senão seguraria o pipe por 30 s
        let r = run_timeout("sh", &["-c", "sleep 30; echo tarde"], Duration::from_millis(200));
        assert!(matches!(r, Err(ExecError::Timeout { .. })), "{r:?}");
        assert!(inicio.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn saida_grande_nao_trava() {
        // mais que o buffer de um pipe (64 KiB) nos dois canais
        let out = run_timeout("sh", &["-c", "head -c 300000 /dev/zero | tr '\\0' a; head -c 300000 /dev/zero | tr '\\0' b >&2"], Duration::from_secs(10)).unwrap();
        assert_eq!(out.len(), 300000);
    }
}
