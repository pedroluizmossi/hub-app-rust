//! MiniUtils — mini-app hub for the desktop (Linux and Windows). See ARCHITECTURE.md.

// On Windows, the release build does not open a console window along with the app (see `console`).
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

/// Old Portuguese names of the command-line flags, still accepted (not shown in `--help`).
/// The store and `--preview` options have their own aliases (`store.rs`, `mascot/preview.rs`).
const LEGACY_FLAGS: [(&str, &str); 3] =
    [("--pastas", "--dirs"), ("--previa", "--preview"), ("--importar-mascote", "--import-mascot")];

/// The current name of a flag: an old Portuguese name becomes the English one.
fn canonical_flag(arg: &str) -> &str {
    LEGACY_FLAGS.iter().find(|(old, _)| *old == arg).map_or(arg, |(_, new)| new)
}

/// Width of the option column in [`help`].
const HELP_COLUMN: usize = 24;

/// Text of `--help`, in the interface language (without the store part, which is
/// `store::USAGE`). Every option here must appear in the README (see the test
/// `readme_mentions_every_option`).
fn help() -> String {
    let mut out = String::new();
    out.push_str(tr!("usage:"));
    out.push('\n');
    for line in [
        tr!("miniutils [--hidden] [--tab <name>] [--plugin-dev <folder>]"),
        tr!("miniutils --preview <mascot> [animation] [--output file.gif]"),
        tr!("miniutils --import-mascot <folder>"),
        "miniutils --dirs",
        "miniutils --version",
    ] {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');
    for (flag, desc) in [
        ("--hidden", tr!("starts in the tray only (for autostart)")),
        (
            tr!("--tab <name>"),
            tr!("opens on a panel page (by its name or the name of an app on it,\ne.g. General, docker), on apps (settings) or on store"),
        ),
        (
            tr!("--plugin-dev <folder>"),
            tr!("loads the plugin in the folder (enabled, on the 1st page) and reloads it\non save; view and script errors go to the terminal"),
        ),
        ("--preview", tr!("GIF of a mascot animation in every theme")),
        (
            "--import-mascot",
            tr!("copies the PNG sheets (Name_Animation_N.png) in the folder and its\nsubfolders to the mascots folder, one mascot per character"),
        ),
        ("--dirs", tr!("where the local apps, the themes/skins/backdrops/mascots,\nthe configuration and the logs are")),
        ("--version", tr!("shows the version (also in Settings › System)")),
    ] {
        out.push_str("  ");
        out.push_str(flag);
        let mut lines = desc.lines();
        let width = flag.chars().count() + 2;
        if width < HELP_COLUMN {
            out.push_str(&" ".repeat(HELP_COLUMN - width));
            out.push_str(lines.next().unwrap_or(""));
        }
        for l in lines {
            out.push('\n');
            out.push_str(&" ".repeat(HELP_COLUMN));
            out.push_str(l);
        }
        out.push('\n');
    }
    out.truncate(out.trim_end().len());
    out
}

/// Answer to `--dirs`: the real paths on this system.
fn dirs_text() -> String {
    let logs = core::os::log_dir().map_or_else(|| tr!("(no folder)").to_string(), |d| d.display().to_string());
    let lines = [
        (tr!("local apps:"), core::plugins::dir().display().to_string(), tr!("(one app per folder, named after the id in manifest.toml)").to_string()),
        (tr!("app data:"), core::appdata::root().display().to_string(), tr!("(one folder per app: what it stores; see [data] in APPS.md)").to_string()),
        (
            tr!("appearance:"),
            skin::user::dir().display().to_string(),
            tr!(
                "({}/, {}/ and {}/, one .toml each, examples in the repository;\n{}/, one folder of PNG sheets per mascot: --import-mascot)",
                skin::user::THEMES,
                skin::user::SKINS,
                skin::user::BACKDROPS,
                mascot::pack::DIR
            ),
        ),
        (tr!("configuration:"), core::config::Config::path().display().to_string(), String::new()),
        (tr!("logs:"), logs, String::new()),
    ];
    let column = lines.iter().map(|(label, _, _)| label.chars().count()).max().unwrap_or(0) + 1;
    let mut out = String::new();
    for (label, path, note) in lines {
        out.push_str(label);
        out.push_str(&" ".repeat(column - label.chars().count()));
        out.push_str(&path);
        out.push('\n');
        for l in note.lines() {
            out.push_str(&" ".repeat(column));
            out.push_str(l);
            out.push('\n');
        }
    }
    out.truncate(out.trim_end().len());
    out
}

fn main() -> anyhow::Result<()> {
    // live-preview (development only): the interface is interpreted at run time, and the
    // ComponentContainer of hub.slint is still an experimental feature (see ui/build.rs)
    #[cfg(feature = "live-preview")]
    // SAFETY: first line of `main`, before any other thread exists
    unsafe {
        std::env::set_var("SLINT_ENABLE_EXPERIMENTAL_FEATURES", "1")
    };
    #[cfg(windows)]
    console::attach();
    // command-line output follows the system language; the window uses the config (Hub::new)
    core::i18n::set(core::i18n::Language::from_setting("auto"));
    let args: Vec<String> = std::env::args().skip(1).map(|a| canonical_flag(&a).to_string()).collect();
    // plugin store (command line): opens no window or tray
    if let Some(code) = store::run_cli(&args) {
        std::process::exit(code);
    }
    // where things are (for someone adding an app by hand, or an agent about to create one)
    if args.first().is_some_and(|a| a == "--dirs") {
        println!("{}", dirs_text());
        return Ok(());
    }
    // GIF preview of an animation: opens no window or tray
    if args.first().is_some_and(|a| a == "--preview") {
        match mascot::preview::run(&args[1..]) {
            Ok(path) => println!("{}", path.display()),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        }
        return Ok(());
    }
    // imported mascot (PNG sheets): copies it to the mascots folder; opens no window
    if args.first().is_some_and(|a| a == "--import-mascot") {
        let Some(src) = args.get(1) else {
            eprintln!("{}", tr!("--import-mascot <folder>: missing the folder with the PNG sheets"));
            std::process::exit(2);
        };
        let root = skin::user::dir().join(mascot::pack::DIR);
        match mascot::pack::import(std::path::Path::new(src), &root) {
            Ok(lines) => {
                for l in lines {
                    println!("{l}");
                }
                println!("{}", tr!("in {}; with MiniUtils open: miniutils --reload", root.display()));
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
        println!("{}\n\n{}", help(), store::USAGE);
        return Ok(());
    }
    let tab = args.iter().position(|a| a == "--tab").and_then(|i| args.get(i + 1)).cloned();
    let dev = match args.iter().position(|a| a == "--plugin-dev") {
        Some(i) => {
            let Some(dir) = args.get(i + 1) else {
                eprintln!("{}", tr!("--plugin-dev <folder>: missing the plugin folder"));
                std::process::exit(2);
            };
            let dir = std::fs::canonicalize(dir).unwrap_or_else(|e| {
                eprintln!("--plugin-dev {dir}: {e}");
                std::process::exit(2);
            });
            if let Err(e) = core::plugins::Plugin::load(&dir) {
                // stays open: the plugin comes in once the manifest is fixed
                eprintln!("plugin-dev: {e}");
            }
            core::plugins::set_dev(dir);
            true
        }
        None => false,
    };

    // is there an instance already? it gets the request and this one exits, with no window or tray
    let socket = instance::socket_path();
    let mut claim_error = None;
    let server = match instance::claim(&socket, &instance::show_message(tab.as_deref())) {
        Ok(instance::Role::Second) if dev => {
            eprintln!(
                "{}",
                tr!("--plugin-dev: close the MiniUtils that is already open (the plugin under development runs in an instance of its own)")
            );
            std::process::exit(1);
        }
        Ok(instance::Role::Second) => return Ok(()),
        Ok(instance::Role::First(server)) => Some(server),
        Err(e) => {
            claim_error = Some(e);
            None
        }
    };
    // only the instance that stays open logs (the second one exits right above)
    let log_file = core::logging::init(dev);
    log::info!(
        "MiniUtils {} starting; log at {}",
        core::about::version_line(),
        log_file.as_deref().map_or("(no file)".into(), |p| p.display().to_string())
    );
    if let Some(e) = claim_error {
        log::warn!("no single instance ({e})");
    }
    autostart::refresh();
    // app data still in the old place (plugins/.data, todo.json in the config folder)
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
    // the config is saved on a thread: finish whatever is pending before exiting
    core::config::flush_saves();
    match &result {
        Ok(()) => log::info!("exited"),
        Err(e) => log::error!("exited with an error: {e:#}"),
    }
    result
}

/// Text output on Windows for an app without a console.
#[cfg(windows)]
mod console {
    use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE};
    use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};
    use windows::core::w;

    /// A windowed app (`windows_subsystem = "windows"`) starts without a console: `--help`,
    /// the store and `--preview` would show nothing. Here it attaches to the console of its
    /// caller (if it was a terminal). Redirected output (`> file`, pipe) already works and
    /// is left alone.
    pub fn attach() {
        // SAFETY: plain console calls, at the start of `main`, before anything is written.
        unsafe {
            if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
                return; // opened from the menu or Explorer: there is no terminal
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

    /// Does the output already point somewhere (file, pipe or console)?
    unsafe fn usable(which: STD_HANDLE) -> bool {
        // SAFETY: only queries the handle; does not use it.
        unsafe { GetStdHandle(which) }.is_ok_and(|h: HANDLE| !h.is_invalid() && !h.0.is_null())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--help` and the README's option list have drifted apart before: every option in
    /// `--help` must appear in the README.
    #[test]
    fn readme_mentions_every_option() {
        let readme = include_str!("../README.md");
        let text = help();
        let flags: Vec<&str> = text
            .split(|c: char| c.is_whitespace() || c == '[' || c == ']')
            .filter(|w| w.starts_with("--"))
            .collect();
        assert!(flags.len() >= 5, "no options found in the help: {flags:?}");
        let missing: Vec<&&str> = flags.iter().filter(|f| !readme.contains(**f)).collect();
        assert!(missing.is_empty(), "--help options missing from the README: {missing:?}");
    }

    /// The old Portuguese flags still work, but the help only shows the English ones.
    #[test]
    fn legacy_portuguese_flags_are_hidden_aliases() {
        assert_eq!(canonical_flag("--pastas"), "--dirs");
        assert_eq!(canonical_flag("--previa"), "--preview");
        assert_eq!(canonical_flag("--importar-mascote"), "--import-mascot");
        assert_eq!(canonical_flag("--hidden"), "--hidden");
        assert_eq!(canonical_flag("neko"), "neko");
        let text = help();
        for (old, new) in LEGACY_FLAGS {
            assert!(!text.contains(old), "{old} shown in --help");
            assert!(text.contains(new), "{new} missing from --help");
        }
    }
}
