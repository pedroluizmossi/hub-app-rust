//! MiniUtils — hub de mini-apps para o desktop (Linux e Windows). Ver ARCHITECTURE.md.

// No Windows, a versão final não abre uma janela de console junto do app (ver `console`).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod apps;
mod autostart;
mod core;
mod hub;
mod instance;
mod mascot;
mod popup;
mod providers;
mod skin;
mod store;
mod tray;
mod ui;

fn main() -> anyhow::Result<()> {
    #[cfg(windows)]
    console::attach();
    let args: Vec<String> = std::env::args().skip(1).collect();
    // loja de plugins (linha de comando): não abre janela nem bandeja
    if let Some(code) = store::run_cli(&args) {
        std::process::exit(code);
    }
    // prévia de animação em GIF: não abre janela nem bandeja
    if args.first().is_some_and(|a| a == "--previa") {
        match mascot::preview::run(&args[1..]) {
            Ok(path) => println!("{}", path.display()),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        }
        return Ok(());
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("miniutils {}", core::about::version_line());
        return Ok(());
    }
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "Uso: miniutils [--hidden] [--tab <id>] [--plugin-dev <pasta>]\n       miniutils --previa <mascote> [animação] [--saida arquivo.gif]\n       miniutils --version\n\n  --hidden              inicia só na bandeja (para o autostart)\n  --tab <id>            abre numa aba (ex.: cyber.vitals, apps)\n  --plugin-dev <pasta>  carrega o plugin da pasta (ligado, na 1ª página) e recarrega ao\n                        salvar; erros da tela e do script saem no terminal\n  --previa              GIF de uma animação de mascote em todos os temas\n\n{}",
            store::USAGE
        );
        return Ok(());
    }
    let tab = args.iter().position(|a| a == "--tab").and_then(|i| args.get(i + 1)).cloned();
    let dev = match args.iter().position(|a| a == "--plugin-dev") {
        Some(i) => {
            let Some(dir) = args.get(i + 1) else {
                eprintln!("--plugin-dev <pasta>: falta a pasta do plugin");
                std::process::exit(2);
            };
            let dir = std::fs::canonicalize(dir).unwrap_or_else(|e| {
                eprintln!("--plugin-dev {dir}: {e}");
                std::process::exit(2);
            });
            if let Err(e) = core::plugins::Plugin::load(&dir) {
                // segue aberto: o plugin entra quando o manifesto for corrigido
                eprintln!("plugin-dev: {e}");
            }
            core::plugins::set_dev(dir);
            true
        }
        None => false,
    };

    // já há uma instância? ela recebe o pedido e esta sai, sem abrir janela nem bandeja
    let socket = instance::socket_path();
    let mut claim_error = None;
    let server = match instance::claim(&socket, &instance::show_message(tab.as_deref())) {
        Ok(instance::Role::Second) if dev => {
            eprintln!("--plugin-dev: feche o MiniUtils que já está aberto (o plugin em desenvolvimento roda numa instância só dele)");
            std::process::exit(1);
        }
        Ok(instance::Role::Second) => return Ok(()),
        Ok(instance::Role::First(server)) => Some(server),
        Err(e) => {
            claim_error = Some(e);
            None
        }
    };
    // só a instância que fica aberta registra (a segunda sai logo acima)
    let log_file = core::logging::init(dev);
    log::info!(
        "MiniUtils {} iniciando; registro em {}",
        core::about::version_line(),
        log_file.as_deref().map_or("(sem arquivo)".into(), |p| p.display().to_string())
    );
    if let Some(e) = claim_error {
        log::warn!("sem instância única ({e})");
    }
    autostart::refresh();

    let result = (|| -> anyhow::Result<()> {
        let positionable = popup::select_backend()?;
        let hub = hub::Hub::new(args.iter().any(|a| a == "--hidden"), tab, positionable)?;
        if let Some(server) = &server {
            hub.serve_commands(server);
        }
        slint::run_event_loop_until_quit()?;
        Ok(())
    })();
    // a configuração é gravada numa thread: termina o que estiver pendente antes de sair
    core::config::flush_saves();
    match &result {
        Ok(()) => log::info!("encerrado"),
        Err(e) => log::error!("encerrado por erro: {e:#}"),
    }
    result
}

/// Saída de texto no Windows para um app sem console.
#[cfg(windows)]
mod console {
    use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE};
    use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};
    use windows::core::w;

    /// Um app de janela (`windows_subsystem = "windows"`) nasce sem console: `--help`, a
    /// loja e a `--previa` não mostrariam nada. Aqui ele se liga ao console de quem o
    /// chamou (se foi um terminal). Saída redirecionada (`> arquivo`, pipe) já chega
    /// pronta e não é mexida.
    pub fn attach() {
        // SAFETY: chamadas simples de console, no começo do `main`, antes de qualquer escrita.
        unsafe {
            if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
                return; // aberto pelo menu ou pelo Explorer: não há terminal
            }
            for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
                if !usable(which) {
                    let console = CreateFileW(
                        w!("CONOUT$"),
                        (GENERIC_READ | GENERIC_WRITE).0,
                        FILE_SHARE_READ | FILE_SHARE_WRITE,
                        None,
                        OPEN_EXISTING,
                        FILE_ATTRIBUTE_NORMAL,
                        None,
                    );
                    if let Ok(console) = console {
                        let _ = SetStdHandle(which, console);
                    }
                }
            }
        }
    }

    /// A saída já aponta para algum lugar (arquivo, pipe ou console)?
    unsafe fn usable(which: STD_HANDLE) -> bool {
        // SAFETY: só consulta o identificador; não o usa.
        unsafe { GetStdHandle(which) }.is_ok_and(|h: HANDLE| !h.is_invalid() && !h.0.is_null())
    }
}
