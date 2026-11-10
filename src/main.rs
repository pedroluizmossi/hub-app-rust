//! CyberHub — hub de mini-apps para o desktop Linux. Ver ARCHITECTURE.md.

mod apps;
mod core;
mod hub;
mod mascot;
mod popup;
mod providers;
mod tray;
mod ui;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "Uso: cyberhub [--hidden] [--tab <id>]\n\n  --hidden     inicia só na bandeja (para o autostart)\n  --tab <id>   abre numa aba (ex.: cyber.vitals, apps)"
        );
        return Ok(());
    }
    let tab = args.iter().position(|a| a == "--tab").and_then(|i| args.get(i + 1)).cloned();
    let positionable = popup::select_backend()?;
    let _hub = hub::Hub::new(args.iter().any(|a| a == "--hidden"), tab, positionable)?;
    slint::run_event_loop_until_quit()?;
    Ok(())
}
