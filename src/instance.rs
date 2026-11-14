//! Single instance: the first `miniutils` keeps listening; later ones only send it a
//! command (`show`, `show apps`) and exit.
//!
//! The channel differs per system, behind the same interface (`socket_path`, `claim`,
//! `Server::serve`, `notify_plugins_changed`): a Unix socket on Linux (`linux.rs`), a named
//! pipe on Windows (`windows.rs`).

#[cfg(unix)]
#[path = "instance/linux.rs"]
mod imp;
#[cfg(windows)]
#[path = "instance/windows.rs"]
mod imp;

pub use imp::{Server, claim, notify_plugins_changed, socket_path};

/// Request from another instance to the one already running.
#[derive(Debug, PartialEq)]
pub enum Command {
    /// Shows the popup; with a tab (`show apps`), opens on it.
    Show(Option<String>),
    /// The installed plugins changed (the store from the command line): re-read the folder.
    ReloadPlugins,
}

/// Reads a received line. An empty or unknown command becomes `None`.
pub fn parse(line: &str) -> Option<Command> {
    let mut words = line.split_whitespace();
    match words.next()? {
        "show" => Some(Command::Show(words.next().map(str::to_string))),
        "plugins" => Some(Command::ReloadPlugins),
        _ => None,
    }
}

/// The line the second instance sends to the first.
pub fn show_message(tab: Option<&str>) -> String {
    match tab {
        Some(t) => format!("show {t}"),
        None => "show".to_string(),
    }
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
    }

    #[test]
    fn message_and_channel_name() {
        assert_eq!(show_message(None), "show");
        assert_eq!(show_message(Some("apps")), "show apps");
        assert_eq!(stem(None), "miniutils");
        assert_eq!(stem(Some("")), "miniutils");
        assert_eq!(stem(Some("a/b")), "miniutils-a_b");
    }

    #[test]
    fn parse_ignores_empty_and_unknown() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("\n"), None);
        assert_eq!(parse("quit"), None);
        assert_eq!(parse("showx"), None);
    }
}
