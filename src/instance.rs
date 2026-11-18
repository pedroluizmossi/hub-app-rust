//! Single instance: the first `miniutils` keeps listening; later ones only send it a
//! command (`show`, `show apps`, `quit`) and exit.
//!
//! The channel differs per system, behind the same interface (`socket_path`, `claim`,
//! `Server::serve`, `send`): a Unix socket on Linux (`linux.rs`), a named pipe on Windows
//! (`windows.rs`).

#[cfg(unix)]
#[path = "instance/linux.rs"]
mod imp;
#[cfg(windows)]
#[path = "instance/windows.rs"]
mod imp;

pub use imp::{Server, claim, socket_path};

/// Tells the running instance (if any) that the plugins changed. `false` = nobody listened.
pub fn notify_plugins_changed() -> bool {
    imp::send(&socket_path(), "plugins")
}

/// Asks the running instance (if any) to quit as if Quit was picked in the tray (saves the
/// window and the config first). `false` = nobody listened.
pub fn notify_quit() -> bool {
    imp::send(&socket_path(), "quit")
}

/// Request from another instance to the one already running.
#[derive(Debug, PartialEq)]
pub enum Command {
    /// Shows the popup; with a tab (`show apps`), opens on it.
    Show(Option<String>),
    /// The installed plugins changed (the store from the command line): re-read the folder.
    ReloadPlugins,
    /// Quit (`miniutils --quit`, used by the installers before replacing the program).
    Quit,
}

/// Reads a received line. An empty or unknown command becomes `None`.
pub fn parse(line: &str) -> Option<Command> {
    let line = line.trim();
    let (word, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    match word {
        // the tab is the rest of the line: a page name may have spaces ("My page")
        "show" => Some(Command::Show(Some(rest.trim()).filter(|t| !t.is_empty()).map(str::to_string))),
        "plugins" => Some(Command::ReloadPlugins),
        "quit" => Some(Command::Quit),
        _ => None,
    }
}

/// The line a second instance sends to the first. `None` with `hidden` (autostart): the
/// running instance stays as it is, so a second autostart entry or a session restore does not
/// open the window at login.
pub fn message(tab: Option<&str>, hidden: bool) -> Option<String> {
    if hidden {
        return None;
    }
    Some(match tab {
        Some(t) => format!("show {t}"),
        None => "show".to_string(),
    })
}

/// Base name of the channel: `miniutils`, or `miniutils-<name>` with `MINIUTILS_INSTANCE`
/// (to run two separate instances, e.g. a test one).
fn stem(instance: Option<&str>) -> String {
    match instance.filter(|n| !n.is_empty()) {
        // the name goes into the path: only letters, digits, '-' (no '/')
        Some(name) => format!("miniutils-{}", clean(name)),
        None => "miniutils".to_string(),
    }
}

fn clean(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect()
}

/// Result of trying to be the main instance.
pub enum Role {
    /// There is already an instance: it got the command; this one must exit.
    Second,
    /// This is the main one: the socket is open (see `Server::serve`).
    First(Server),
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_accepts_show_with_or_without_a_tab() {
        assert_eq!(parse("show\n"), Some(Command::Show(None)));
        assert_eq!(parse("  show apps \n"), Some(Command::Show(Some("apps".into()))));
        assert_eq!(parse("show cyber.vitals"), Some(Command::Show(Some("cyber.vitals".into()))));
        assert_eq!(parse("plugins\n"), Some(Command::ReloadPlugins));
        assert_eq!(parse("quit\n"), Some(Command::Quit));
        // a page name with spaces arrives whole
        assert_eq!(parse("show My page\n"), Some(Command::Show(Some("My page".into()))));
        assert_eq!(parse("show\tMy  page \n"), Some(Command::Show(Some("My  page".into()))));
        assert_eq!(parse("show   \n"), Some(Command::Show(None)));
    }

    #[test]
    fn message_and_channel_name() {
        assert_eq!(message(None, false).as_deref(), Some("show"));
        assert_eq!(message(Some("apps"), false).as_deref(), Some("show apps"));
        assert_eq!(parse(&message(Some("My page"), false).unwrap()), Some(Command::Show(Some("My page".into()))));
        assert_eq!(stem(None), "miniutils");
        assert_eq!(stem(Some("")), "miniutils");
        assert_eq!(stem(Some("a/b")), "miniutils-a_b");
    }

    #[test]
    fn hidden_start_sends_nothing_to_the_running_instance() {
        assert_eq!(message(None, true), None);
        assert_eq!(message(Some("apps"), true), None);
    }

    #[test]
    fn parse_ignores_empty_and_unknown() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("\n"), None);
        assert_eq!(parse("quitx"), None);
        assert_eq!(parse("showx"), None);
        assert_eq!(parse("showx apps"), None);
    }
}
