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

/// Texto do `--help` (sem a parte da loja, que é `store::USAGE`). Cada opção daqui tem de
/// aparecer no README (ver o teste `readme_cita_todas_as_opcoes`).
const HELP: &str = "\
Uso: miniutils [--hidden] [--tab <nome>] [--plugin-dev <pasta>]
       miniutils --previa <mascote> [animação] [--saida arquivo.gif]
       miniutils --importar-mascote <pasta>
       miniutils --pastas
       miniutils --version

  --hidden              inicia só na bandeja (para o autostart)
  --tab <nome>          abre numa página do painel (pelo nome dela ou de um app nela,
                        ex.: Geral, docker), em apps (configurações) ou em store (loja)
  --plugin-dev <pasta>  carrega o plugin da pasta (ligado, na 1ª página) e recarrega ao
                        salvar; erros da tela e do script saem no terminal
  --previa              GIF de uma animação de mascote em todos os temas
  --importar-mascote    copia as folhas PNG (Nome_Animação_N.png) da pasta e das
                        subpastas para a pasta de mascotes, uma mascote por personagem
  --pastas              onde ficam os apps locais, os temas/visuais/fundos/mascotes,
                        a configuração e os registros
  --version             mostra a versão (também em Configurações › Sistema)";

/// Resposta do `--pastas`: os caminhos reais neste sistema.
fn dirs_text() -> String {
    let logs = core::os::log_dir().map_or_else(|| "(sem pasta)".into(), |d| d.display().to_string());
    format!(
        "apps locais:   {}\n               (um app por pasta, com o nome do id do manifest.toml)\ndados dos apps: {}\n               (uma pasta por app: o que ele guarda; ver [data] no APPS.md)\naparência:     {}\n               (temas/, visuais/ e fundos/, um .toml cada, exemplos em examples/aparencia;\n               mascotes/, uma pasta de folhas PNG por mascote: --importar-mascote)\nconfiguração:  {}\nregistros:     {logs}",
        core::plugins::dir().display(),
        core::appdata::root().display(),
        skin::user::dir().display(),
        core::config::Config::path().display(),
    )
}

fn main() -> anyhow::Result<()> {
    // live-preview (só desenvolvimento): a interface é interpretada ao rodar, e o
    // ComponentContainer do hub.slint ainda é recurso experimental (ver ui/build.rs)
    #[cfg(feature = "live-preview")]
    // SAFETY: primeira linha do `main`, antes de qualquer outra thread existir
    unsafe {
        std::env::set_var("SLINT_ENABLE_EXPERIMENTAL_FEATURES", "1")
    };
    #[cfg(windows)]
    console::attach();
    let args: Vec<String> = std::env::args().skip(1).collect();
    // loja de plugins (linha de comando): não abre janela nem bandeja
    if let Some(code) = store::run_cli(&args) {
        std::process::exit(code);
    }
    // onde ficam as coisas (para quem vai pôr um app à mão, ou um agente que vai criá-lo)
    if args.first().is_some_and(|a| a == "--pastas") {
        println!("{}", dirs_text());
        return Ok(());
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
    // mascote importada (folhas PNG): copia para a pasta de mascotes; não abre janela
    if args.first().is_some_and(|a| a == "--importar-mascote") {
        let Some(src) = args.get(1) else {
            eprintln!("--importar-mascote <pasta>: falta a pasta com as folhas PNG");
            std::process::exit(2);
        };
        let root = skin::user::dir().join(mascot::pack::DIR);
        match mascot::pack::import(std::path::Path::new(src), &root) {
            Ok(lines) => {
                for l in lines {
                    println!("{l}");
                }
                println!("em {}; com o MiniUtils aberto: miniutils --recarregar", root.display());
            }
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
        println!("{HELP}\n\n{}", store::USAGE);
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
    // dados dos apps que ainda estão no lugar antigo (plugins/.data, todo.json da config)
    core::appdata::migrate_legacy(true);

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

#[cfg(test)]
mod tests {
    use super::HELP;

    /// O `--help` e a lista de opções do README já divergiram: toda opção do `--help` tem
    /// de aparecer no README.
    #[test]
    fn readme_cita_todas_as_opcoes() {
        let readme = include_str!("../README.md");
        let flags: Vec<&str> = HELP
            .split(|c: char| c.is_whitespace() || c == '[' || c == ']')
            .filter(|w| w.starts_with("--"))
            .collect();
        assert!(flags.len() >= 5, "não achou as opções no HELP: {flags:?}");
        let faltam: Vec<&&str> = flags.iter().filter(|f| !readme.contains(**f)).collect();
        assert!(faltam.is_empty(), "opções do --help que faltam no README: {faltam:?}");
    }
}
