//! Single instance: the first `miniutils` keeps listening; later ones only send it a
//! command (`show`, `show apps`, `toggle`, `quit`) and exit.
//!
//! The same channel answers anything that wants to know how the hub is doing (a bar, a
//! script): `status` gets one JSON line, `subscribe` gets one now and another on every
//! change (see `StatusFeed`).
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

/// Base name of this instance's channel: `miniutils`, or `miniutils-<name>` with
/// `MINIUTILS_INSTANCE`. Also names whatever else must not be shared between instances
/// (the popup's window rule on Hyprland; nothing on Windows).
#[cfg_attr(windows, allow(dead_code))]
pub fn channel_name() -> String {
    stem(std::env::var("MINIUTILS_INSTANCE").ok().as_deref())
}

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
    /// Hides the popup if it is open; otherwise shows it, as `Show` (a global shortcut).
    Toggle(Option<String>),
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
        "show" => Some(Command::Show(tab(rest))),
        "toggle" => Some(Command::Toggle(tab(rest))),
        "plugins" => Some(Command::ReloadPlugins),
        "quit" => Some(Command::Quit),
        _ => None,
    }
}

/// The tab named after a command word (`None` when there is none).
fn tab(rest: &str) -> Option<String> {
    Some(rest.trim()).filter(|t| !t.is_empty()).map(str::to_string)
}

/// A question about the hub, answered on the same connection (see `StatusFeed`).
// the Windows pipe is one-way: nothing there asks (see `windows.rs`)
#[cfg_attr(windows, allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Query {
    /// One status line.
    Status,
    /// A status line now and another whenever it changes, until the asker hangs up.
    Subscribe,
}

/// Reads a received line as a question. Anything else is `None` (it may be a command).
#[cfg_attr(windows, allow(dead_code))]
pub fn parse_query(line: &str) -> Option<Query> {
    match line.trim() {
        "status" => Some(Query::Status),
        "subscribe" => Some(Query::Subscribe),
        _ => None,
    }
}

/// How the hub is doing, as one line of JSON, for whoever asks on the instance channel.
///
/// The hub publishes the line whenever it changes (on the UI thread); the channel's thread
/// answers `status` with the last one and keeps the `subscribe` connections, which get every
/// new line. A subscriber that stops reading is dropped.
#[derive(Default)]
pub struct StatusFeed {
    state: std::sync::Mutex<FeedState>,
}

#[derive(Default)]
struct FeedState {
    line: String,
    subscribers: Vec<Box<dyn std::io::Write + Send>>,
}

impl StatusFeed {
    /// The new status. Reaches the subscribers only when it differs from the last one.
    pub fn publish(&self, line: String) {
        let Ok(mut state) = self.state.lock() else { return };
        if state.line == line {
            return;
        }
        state.line = line;
        let FeedState { line, subscribers } = &mut *state;
        subscribers.retain_mut(|s| send_line(s, line));
    }

    /// Answers `status`: the last line.
    #[cfg_attr(windows, allow(dead_code))]
    pub fn answer(&self, mut to: impl std::io::Write) {
        if let Ok(state) = self.state.lock() {
            send_line(&mut to, &state.line);
        }
    }

    /// Answers `subscribe`: the last line now, and `to` stays for the next ones.
    #[cfg_attr(windows, allow(dead_code))]
    pub fn subscribe(&self, mut to: Box<dyn std::io::Write + Send>) {
        let Ok(mut state) = self.state.lock() else { return };
        if send_line(&mut to, &state.line) {
            state.subscribers.push(to);
        }
    }
}

/// Writes one line; `false` when the other side is gone (or stuck).
fn send_line(to: &mut impl std::io::Write, line: &str) -> bool {
    to.write_all(line.as_bytes()).and_then(|()| to.write_all(b"\n")).and_then(|()| to.flush()).is_ok()
}

/// The line a second instance sends to the first. `None` with `hidden` (autostart): the
/// running instance stays as it is, so a second autostart entry or a session restore does not
/// open the window at login. With `toggle` it asks to hide the window if it is open.
pub fn message(tab: Option<&str>, hidden: bool, toggle: bool) -> Option<String> {
    if hidden {
        return None;
    }
    let verb = if toggle { "toggle" } else { "show" };
    Some(match tab {
        Some(t) => format!("{verb} {t}"),
        None => verb.to_string(),
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
        // toggle takes a tab the same way
        assert_eq!(parse("toggle\n"), Some(Command::Toggle(None)));
        assert_eq!(parse("toggle My page\n"), Some(Command::Toggle(Some("My page".into()))));
    }

    #[test]
    fn questions_are_not_commands() {
        assert_eq!(parse_query("status\n"), Some(Query::Status));
        assert_eq!(parse_query(" subscribe \n"), Some(Query::Subscribe));
        assert_eq!(parse_query("show"), None);
        assert_eq!(parse_query("status now"), None);
        assert_eq!(parse("status"), None);
        assert_eq!(parse("subscribe"), None);
    }

    /// A writer the test can read back, and break.
    #[derive(Clone, Default)]
    struct Pipe {
        got: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
        broken: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl Pipe {
        fn text(&self) -> String {
            String::from_utf8(self.got.lock().unwrap().clone()).unwrap()
        }
    }

    impl std::io::Write for Pipe {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.broken.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            self.got.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn status_feed_answers_the_last_line_and_follows_changes() {
        let feed = StatusFeed::default();
        feed.publish("{\"shown\":false}".into());
        let once = Pipe::default();
        feed.answer(once.clone());
        assert_eq!(once.text(), "{\"shown\":false}\n");

        let (a, b) = (Pipe::default(), Pipe::default());
        feed.subscribe(Box::new(a.clone()));
        feed.subscribe(Box::new(b.clone()));
        // the same status again is not news
        feed.publish("{\"shown\":false}".into());
        assert_eq!(a.text(), "{\"shown\":false}\n");
        feed.publish("{\"shown\":true}".into());
        assert_eq!(a.text(), "{\"shown\":false}\n{\"shown\":true}\n");
        // a subscriber that went away is dropped; the others keep getting lines
        b.broken.store(true, std::sync::atomic::Ordering::Relaxed);
        feed.publish("{\"shown\":false}".into());
        feed.publish("{\"shown\":true}".into());
        assert_eq!(feed.state.lock().unwrap().subscribers.len(), 1);
        assert!(a.text().ends_with("{\"shown\":false}\n{\"shown\":true}\n"));
        assert_eq!(once.text(), "{\"shown\":false}\n", "`status` gets one line only");
    }

    #[test]
    fn message_and_channel_name() {
        assert_eq!(message(None, false, false).as_deref(), Some("show"));
        assert_eq!(message(Some("apps"), false, false).as_deref(), Some("show apps"));
        assert_eq!(parse(&message(Some("My page"), false, false).unwrap()), Some(Command::Show(Some("My page".into()))));
        assert_eq!(message(None, false, true).as_deref(), Some("toggle"));
        assert_eq!(parse(&message(Some("My page"), false, true).unwrap()), Some(Command::Toggle(Some("My page".into()))));
        assert_eq!(stem(None), "miniutils");
        assert_eq!(stem(Some("")), "miniutils");
        assert_eq!(stem(Some("a/b")), "miniutils-a_b");
    }

    #[test]
    fn hidden_start_sends_nothing_to_the_running_instance() {
        assert_eq!(message(None, true, false), None);
        assert_eq!(message(Some("apps"), true, false), None);
        assert_eq!(message(None, true, true), None);
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
