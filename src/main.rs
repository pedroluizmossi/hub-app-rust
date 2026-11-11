//! MiniUtils — hub de mini-apps para o desktop Linux. Ver ARCHITECTURE.md.

mod apps;
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
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "Uso: miniutils [--hidden] [--tab <id>]\n       miniutils --previa <mascote> [animação] [--saida arquivo.gif]\n\n  --hidden     inicia só na bandeja (para o autostart)\n  --tab <id>   abre numa aba (ex.: cyber.vitals, apps)\n  --previa     GIF de uma animação de mascote em todos os temas\n\n{}",
            store::USAGE
        );
        return Ok(());
    }
    let tab = args.iter().position(|a| a == "--tab").and_then(|i| args.get(i + 1)).cloned();

    // já há uma instância? ela recebe o pedido e esta sai, sem abrir janela nem bandeja
    let socket = instance::socket_path();
    let server = match instance::claim(&socket, &instance::show_message(tab.as_deref())) {
        Ok(instance::Role::Second) => return Ok(()),
        Ok(instance::Role::First(server)) => Some(server),
        Err(e) => {
            eprintln!("miniutils: sem instância única ({e})");
            None
        }
    };

    let positionable = popup::select_backend()?;
    let hub = hub::Hub::new(args.iter().any(|a| a == "--hidden"), tab, positionable)?;
    if let Some(server) = &server {
        hub.serve_commands(server);
    }
    slint::run_event_loop_until_quit()?;
    // a configuração é gravada numa thread: termina o que estiver pendente antes de sair
    core::config::flush_saves();
    Ok(())
}
