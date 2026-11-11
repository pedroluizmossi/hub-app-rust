//! Executor de comandos com allowlist por mini-app.
//!
//! O mini-app só roda os programas declarados em `permissions.exec` do manifesto, e os
//! argumentos vão direto para o processo, sem shell — nada de interpolar strings.

use std::ffi::OsStr;
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
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

    /// Roda e espera (com prazo), na thread de quem chama: para quem já está numa thread
    /// própria (o script de um plugin). Nunca na thread da UI.
    pub fn run_blocking(&self, program: &str, args: &[&str], limit: Duration) -> Result<String, ExecError> {
        self.check(program)?;
        run_timeout(program, args, limit)
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
        self.open_terminal_in(title, None, argv)
    }

    /// Como `open_terminal`, começando na pasta `dir` (`None` = a pasta pessoal).
    pub fn open_terminal_in(&self, title: &str, dir: Option<&Path>, argv: &[&str]) -> anyhow::Result<()> {
        let program = argv.first().ok_or_else(|| anyhow::anyhow!("comando vazio"))?;
        self.check(program)?;
        // caminho completo: o terminal pode não ter as pastas do usuário no PATH
        let path = locate(program).ok_or_else(|| ExecError::NotFound(program.to_string()))?;
        let dir = dir.filter(|d| d.is_dir());
        let mut cmd = terminal_command(title, dir, Some((&path, &argv[1..])))?;
        // sem pasta pedida, começa na pessoal, não na pasta de onde o app foi aberto
        match dir {
            Some(dir) => {
                cmd.current_dir(dir);
            }
            None => {
                if let Some(home) = dirs::home_dir() {
                    cmd.current_dir(home);
                }
            }
        }
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        spawn_detached(&mut cmd)?;
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
/// O filho ganha um grupo de processos próprio (no Windows, a árvore dele): ao estourar o
/// prazo o grupo inteiro morre, e nenhum neto fica segurando os pipes (o que prenderia as
/// leitoras).
pub fn output_timeout<S: AsRef<OsStr>>(program: &str, args: &[S], limit: Duration) -> Result<Output, ExecError> {
    let mut cmd = command(program);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd
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

/// `Command` para um programa que roda em segundo plano (provedores, ações dos apps).
///
/// No Windows faz duas coisas que o `Command::new` não faz: acha os programas que são
/// `.cmd`/`.bat` (as CLIs instaladas pelo npm: `codex`, `claude`, `opencode`), que só
/// rodam pelo caminho completo, e não abre uma janela de console a cada execução.
pub fn command(program: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = match find(program) {
            Some(path) => Command::new(path),
            None => Command::new(program),
        };
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd
    }
    #[cfg(not(windows))]
    Command::new(program)
}

/// `CREATE_NO_WINDOW`: programa de console sem janela (senão ela pisca na tela).
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Mata o filho e o grupo dele, e recolhe o processo.
fn kill_group(child: &mut std::process::Child) {
    // SAFETY: `kill` só envia um sinal; o pid é o do filho que ainda não recolhemos, que é
    // líder do próprio grupo (`process_group(0)`).
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
    }
    // o Windows não tem grupo para sinalizar: o `taskkill /T` encerra o filho e os netos
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Abre um endereço http(s) no navegador padrão (serviço do host, fora da allowlist).
pub fn open_url(url: &str) {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return;
    }
    shell_open(OsStr::new(url));
}

/// Abre uma pasta no gerenciador de arquivos padrão (serviço do host, fora da allowlist).
/// Só pastas que existem: nada de abrir arquivo ou endereço por este caminho.
pub fn open_dir(dir: &Path) -> bool {
    dir.is_absolute() && dir.is_dir() && shell_open(dir.as_os_str())
}

/// Entrega um endereço ou pasta ao programa padrão do sistema.
#[cfg(not(windows))]
fn shell_open(target: &OsStr) -> bool {
    spawn_detached(Command::new("xdg-open").arg(target).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())).is_ok()
}

/// Entrega um endereço ou pasta ao programa padrão do sistema. Pelo `ShellExecute`, e não
/// por `cmd /c start`: assim nada do endereço é interpretado como comando.
#[cfg(windows)]
fn shell_open(target: &OsStr) -> bool {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, PCWSTR, w};
    let target = HSTRING::from(target);
    // SAFETY: as strings vivem até a chamada voltar; o resultado é só um código (> 32 = abriu).
    let code = unsafe { ShellExecuteW(None, w!("open"), &target, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
    code.0 as isize > 32
}

/// Lança um programa que segue sozinho (navegador, terminal, gerenciador de arquivos).
/// Uma thread espera ele sair: sem o `wait`, o processo vira zumbi até o app fechar, e o
/// app fica dias na bandeja.
fn spawn_detached(cmd: &mut Command) -> std::io::Result<()> {
    let mut child = cmd.spawn()?;
    let _ = std::thread::Builder::new().name("miniutils-reap".into()).spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Abre um terminal na pasta `dir`, com o shell do usuário (serviço do host, fora da
/// allowlist: não roda nenhum programa escolhido pelo mini-app).
pub fn open_terminal_at(dir: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(dir.is_absolute() && dir.is_dir(), "pasta não encontrada: {}", dir.display());
    let mut cmd = terminal_command("", Some(dir), None)?;
    cmd.current_dir(dir).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    spawn_detached(&mut cmd)?;
    Ok(())
}

/// Monta o comando que abre um terminal: em `dir` (se pedida) e rodando `run` (programa já
/// com o caminho completo, e os argumentos) ou, sem `run`, o shell do usuário.
#[cfg(not(windows))]
fn terminal_command(title: &str, dir: Option<&Path>, run: Option<(&Path, &[&str])>) -> anyhow::Result<Command> {
    let (term, prefix) = terminal().ok_or_else(|| anyhow::anyhow!("nenhum terminal encontrado (defina $TERMINAL)"))?;
    let mut cmd = Command::new(&term);
    if !title.is_empty() {
        if term.ends_with("konsole") {
            cmd.args(["-p", &format!("tabtitle={title}")]);
        } else if term.ends_with("xfce4-terminal") || term.ends_with("gnome-terminal") {
            cmd.args(["--title", title]);
        }
    }
    // o konsole pode reaproveitar um processo já aberto, com outra pasta: pede a pasta nele
    if let (Some(dir), true) = (dir, term.ends_with("konsole")) {
        cmd.arg("--workdir").arg(dir);
    }
    if let Some((path, args)) = run {
        cmd.args(prefix).arg(path).args(args);
    }
    Ok(cmd)
}

/// Monta o comando que abre um terminal no Windows: o Windows Terminal (`wt`) quando
/// existe; senão o console clássico (`conhost`). Os dois recebem o programa e os
/// argumentos direto, sem passar pelo `cmd` — nada de interpolar strings.
#[cfg(windows)]
fn terminal_command(title: &str, dir: Option<&Path>, run: Option<(&Path, &[&str])>) -> anyhow::Result<Command> {
    let (path, args): (PathBuf, Vec<String>) = match run {
        Some((path, args)) => (path.to_path_buf(), args.iter().map(|a| a.to_string()).collect()),
        None => (default_shell(), Vec::new()),
    };
    // um `.cmd`/`.bat` é sempre executado pelo `cmd`, que interpretaria estes caracteres
    if is_batch(&path) && args.iter().any(|a| a.contains(CMD_SPECIAL)) {
        anyhow::bail!("argumento com caractere especial do cmd: não abro {} assim", path.display());
    }
    let home = dirs::home_dir();
    let dir = dir.or(home.as_deref());
    let mut cmd = match find("wt") {
        Some(wt) => {
            let mut cmd = Command::new(wt);
            cmd.arg("new-tab");
            if !title.is_empty() {
                cmd.args(["--title", title]);
            }
            if let Some(dir) = dir {
                cmd.arg("-d").arg(dir);
            }
            // para o `wt`, `;` separa comandos: escapado, vira texto
            cmd.arg(&path).args(args.iter().map(|a| a.replace(';', "\\;")));
            cmd
        }
        None => {
            let mut cmd = Command::new("conhost.exe");
            cmd.arg(&path).args(&args);
            cmd
        }
    };
    if let Some(dir) = dir {
        cmd.current_dir(dir);
    }
    Ok(cmd)
}

/// Caracteres que o `cmd` interpreta numa linha de comando.
#[cfg(windows)]
const CMD_SPECIAL: &[char] = &['&', '|', '<', '>', '^', '%', '"', '!', '(', ')', '\n', '\r'];

#[cfg(windows)]
fn is_batch(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
}

/// Shell para o "abrir terminal aqui": PowerShell novo, o que vem no Windows, ou o `cmd`.
#[cfg(windows)]
fn default_shell() -> PathBuf {
    find("pwsh")
        .or_else(|| find("powershell"))
        .or_else(|| std::env::var_os("COMSPEC").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("cmd.exe"))
}

pub fn which(program: &str) -> bool {
    find(program).is_some()
}

/// Caminho de um programa no PATH.
pub fn find(program: &str) -> Option<PathBuf> {
    find_in(path_dirs(), program)
}

fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default()
}

fn find_in(dirs: Vec<PathBuf>, program: &str) -> Option<PathBuf> {
    let names = file_names(program);
    dirs.into_iter().find_map(|d| names.iter().map(|n| d.join(n)).find(|p| p.is_file()))
}

/// Nomes de arquivo que um programa pode ter: no Linux, o próprio nome.
#[cfg(not(windows))]
fn file_names(program: &str) -> Vec<String> {
    vec![program.to_string()]
}

/// Nomes de arquivo que um programa pode ter: no Windows, o nome com cada extensão
/// executável do `PATHEXT` (`docker` → `docker.exe`, `codex` → `codex.cmd`).
#[cfg(windows)]
fn file_names(program: &str) -> Vec<String> {
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    windows_file_names(program, &pathext)
}

/// Separado do ambiente para poder ser testado em qualquer sistema.
#[cfg(any(windows, test))]
fn windows_file_names(program: &str, pathext: &str) -> Vec<String> {
    let mut names = Vec::new();
    // já veio com extensão (`wt.exe`): esse nome primeiro
    if Path::new(program).extension().is_some() {
        names.push(program.to_string());
    }
    names.extend(pathext.split(';').filter(|e| e.starts_with('.') && e.len() > 1).map(|e| format!("{program}{}", e.to_ascii_lowercase())));
    names
}

/// Caminho de um programa: no PATH ou nas pastas onde instaladores de CLI costumam pôr
/// binários do usuário. Aberto pelo menu do sistema, o app pode não ter essas pastas no
/// PATH (elas entram pelo `.bashrc`/`.zshrc`), e o terminal não acharia o programa.
fn locate(program: &str) -> Option<PathBuf> {
    let mut dirs = path_dirs();
    if let Some(home) = dirs::home_dir() {
        for d in USER_BIN_DIRS {
            dirs.push(home.join(d));
        }
    }
    find_in(dirs, program)
}

#[cfg(not(windows))]
const USER_BIN_DIRS: &[&str] = &[".local/bin", ".opencode/bin", ".cargo/bin", ".bun/bin", ".npm-global/bin", ".volta/bin"];
#[cfg(windows)]
const USER_BIN_DIRS: &[&str] =
    &["AppData/Roaming/npm", ".local/bin", ".opencode/bin", ".cargo/bin", ".bun/bin", "AppData/Local/Volta/bin", "scoop/shims"];

/// Terminais conhecidos e o argumento que separa o comando a executar.
#[cfg(any(not(windows), test))]
const KNOWN_TERMINALS: &[(&str, &[&str])] = &[
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

/// Argumento que separa o comando para o terminal `term` (nome ou caminho). Terminal fora
/// da lista recebe `-e`, o mais comum.
#[cfg(any(not(windows), test))]
fn terminal_prefix(term: &str) -> &'static [&'static str] {
    let name = Path::new(term).file_name().and_then(|n| n.to_str()).unwrap_or(term);
    KNOWN_TERMINALS.iter().find(|(t, _)| *t == name).map_or(&["-e"], |(_, p)| *p)
}

/// Terminal disponível e o argumento que separa o comando a executar.
#[cfg(not(windows))]
fn terminal() -> Option<(String, &'static [&'static str])> {
    if let Ok(t) = std::env::var("TERMINAL") {
        if which(&t) || Path::new(&t).is_file() {
            let prefix = terminal_prefix(&t);
            return Some((t, prefix));
        }
    }
    KNOWN_TERMINALS.iter().find(|(t, _)| which(t)).map(|(t, p)| (t.to_string(), *p))
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
    #[cfg(unix)]
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
    #[cfg(unix)]
    fn mata_o_que_passa_do_prazo() {
        let inicio = Instant::now();
        // o `sleep` é neto (filho do sh): morre junto, senão seguraria o pipe por 30 s
        let r = run_timeout("sh", &["-c", "sleep 30; echo tarde"], Duration::from_millis(200));
        assert!(matches!(r, Err(ExecError::Timeout { .. })), "{r:?}");
        assert!(inicio.elapsed() < Duration::from_secs(5));
    }

    #[test]
    #[cfg(windows)]
    fn windows_devolve_saida_erro_e_programa_ausente() {
        let limit = Duration::from_secs(10);
        assert_eq!(run_timeout("cmd", &["/c", "echo oi"], limit).unwrap().trim(), "oi");
        let out = output_timeout("cmd", &["/c", "echo parte& exit 1"], limit).unwrap();
        assert!(!out.success);
        assert_eq!(out.stdout.trim(), "parte");
        assert!(run_timeout("miniutils-programa-que-nao-existe", &[""; 0], limit).unwrap_err().not_found());
    }

    #[test]
    #[cfg(windows)]
    fn windows_mata_a_arvore_que_passa_do_prazo() {
        let inicio = Instant::now();
        // o `ping` é neto (filho do cmd): morre junto, senão seguraria o pipe por 30 s
        let r = run_timeout("cmd", &["/c", "ping -n 30 127.0.0.1"], Duration::from_millis(500));
        assert!(matches!(r, Err(ExecError::Timeout { .. })), "{r:?}");
        assert!(inicio.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn nomes_de_programa_no_windows() {
        let ext = ".COM;.EXE;.BAT;.CMD";
        assert_eq!(windows_file_names("docker", ext), ["docker.com", "docker.exe", "docker.bat", "docker.cmd"]);
        // com extensão, o próprio nome vem primeiro
        assert_eq!(windows_file_names("wt.exe", ".EXE")[0], "wt.exe");
        // nome com ponto que não é extensão executável ainda ganha as do PATHEXT
        assert!(windows_file_names("python3.12", ext).contains(&"python3.12.exe".to_string()));
        assert_eq!(windows_file_names("x", ";;"), Vec::<String>::new());
    }

    #[test]
    fn terminal_de_fora_usa_o_separador_certo() {
        assert_eq!(terminal_prefix("kitty"), &[] as &[&str]);
        assert_eq!(terminal_prefix("/usr/bin/gnome-terminal"), &["--"]);
        assert_eq!(terminal_prefix("wezterm"), &["start", "--"]);
        assert_eq!(terminal_prefix("st"), &["-e"]);
    }

    #[test]
    #[cfg(unix)]
    fn saida_grande_nao_trava() {
        // mais que o buffer de um pipe (64 KiB) nos dois canais
        let out = run_timeout("sh", &["-c", "head -c 300000 /dev/zero | tr '\\0' a; head -c 300000 /dev/zero | tr '\\0' b >&2"], Duration::from_secs(10)).unwrap();
        assert_eq!(out.len(), 300000);
    }
}
