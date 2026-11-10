//! Executor de comandos com allowlist por mini-app.
//!
//! O mini-app só roda os programas declarados em `permissions.exec` do manifesto, e os
//! argumentos vão direto para o processo, sem shell — nada de interpolar strings.

use std::process::{Command, Stdio};

#[derive(Debug, Clone)]
pub struct Exec {
    app: String,
    allowed: Vec<String>,
}

impl Exec {
    pub fn new(app: &str, allowed: &[String]) -> Self {
        Exec { app: app.to_string(), allowed: allowed.to_vec() }
    }

    fn check(&self, program: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.allowed.iter().any(|p| p == program),
            "o mini-app {} não tem permissão para executar \"{program}\"",
            self.app
        );
        Ok(())
    }

    /// Roda em segundo plano (thread própria) e chama `done` com a saída combinada.
    pub fn run_async<F>(&self, program: &str, args: &[&str], done: F)
    where
        F: FnOnce(anyhow::Result<String>) + Send + 'static,
    {
        if let Err(e) = self.check(program) {
            done(Err(e));
            return;
        }
        let program = program.to_string();
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        std::thread::spawn(move || done(run(&program, &args)));
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

/// Roda um programa e devolve stdout (ou stderr no erro). Uso interno do host.
pub fn run(program: &str, args: &[String]) -> anyhow::Result<String> {
    let out = Command::new(program).args(args).stdin(Stdio::null()).output()?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        anyhow::bail!(if err.is_empty() { format!("{program} saiu com {}", out.status) } else { err })
    }
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
}
