//! MPRIS over D-Bus: who is playing, what, and the player commands.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedValue;

use super::{MediaSnapshot, usable};
use crate::tr;

/// Prefix of the services that speak MPRIS.
const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT: &str = "org.mpris.MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const PROPS: &str = "org.freedesktop.DBus.Properties";
/// After a failure, wait this long before trying the bus again.
const RETRY: Duration = Duration::from_secs(10);
/// Deadline of each D-Bus call. zbus has no deadline by default: a stuck player (frozen
/// browser, stopped process) would hold the provider thread forever.
const CALL_TIMEOUT: Duration = Duration::from_secs(2);

/// Connection to the session bus, with a deadline on calls.
fn connect() -> zbus::Result<Connection> {
    zbus::blocking::connection::Builder::session()?.method_timeout(CALL_TIMEOUT).build()
}

#[derive(Default)]
pub struct MediaProvider {
    conn: Option<Connection>,
    /// Player in use (keeps the choice between readings).
    player: String,
    last: Option<MediaSnapshot>,
    retry_at: Option<Instant>,
}


impl MediaProvider {
    /// Called every second while the mini-app is enabled. `Some` when something changed.
    pub fn collect(&mut self, force: bool) -> Option<MediaSnapshot> {
        let snap = self.read();
        self.publish(snap, force)
    }

    fn publish(&mut self, snap: MediaSnapshot, force: bool) -> Option<MediaSnapshot> {
        let changed = self.last.as_ref() != Some(&snap);
        self.last = Some(snap.clone());
        (force || changed).then_some(snap)
    }

    fn read(&mut self) -> MediaSnapshot {
        if self.conn.is_none() {
            if self.retry_at.is_some_and(|t| Instant::now() < t) {
                return self.fail(tr!("media bus unavailable"));
            }
            match connect() {
                Ok(c) => {
                    self.conn = Some(c);
                    self.retry_at = None;
                }
                Err(e) => {
                    self.retry_at = Some(Instant::now() + RETRY);
                    return self.fail(&e.to_string());
                }
            }
        }
        let conn = self.conn.as_ref().expect("connection just opened").clone();
        match self.poll(&conn) {
            Ok(snap) => snap,
            Err(e) => {
                // connection dropped (D-Bus restarted, the player closed mid-read)
                self.conn = None;
                self.retry_at = Some(Instant::now() + RETRY);
                self.fail(&e.to_string())
            }
        }
    }

    fn fail(&mut self, reason: &str) -> MediaSnapshot {
        self.player.clear();
        MediaSnapshot { ok: false, error: first_line(reason), ..Default::default() }
    }

    fn poll(&mut self, conn: &Connection) -> anyhow::Result<MediaSnapshot> {
        let names = players(conn)?;
        if names.is_empty() {
            self.player.clear();
            return Ok(MediaSnapshot { ok: true, ..Default::default() });
        }

        // Two players may describe the same song (Firefox and the browser integration, for
        // example). Keep the one that is playing and knows length/position; a tie keeps the
        // current one.
        let mut best: Option<((u8, u8, u8, u8), MediaSnapshot)> = None;
        for name in &names {
            let Ok(player) = props(conn, name, PLAYER) else { continue };
            let root = props(conn, name, ROOT).unwrap_or_default();
            let position = position(conn, name).ok();
            let snap = parse(name, &root, &player, position);
            let score = usable(&snap, &self.player);
            if best.as_ref().is_none_or(|(s, _)| score > *s) {
                best = Some((score, snap));
            }
        }
        match best {
            Some((_, snap)) => {
                self.player = snap.player.clone();
                Ok(snap)
            }
            None => {
                self.player.clear();
                Ok(MediaSnapshot { ok: true, ..Default::default() })
            }
        }
    }
}

/// MPRIS services on the bus, in a stable order.
fn players(conn: &Connection) -> anyhow::Result<Vec<String>> {
    let bus = Proxy::new(conn, "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus")?;
    let names: Vec<String> = bus.call("ListNames", &())?;
    let mut out: Vec<String> = names.into_iter().filter(|n| n.starts_with(PREFIX)).collect();
    out.sort();
    Ok(out)
}

/// All properties of one interface of a player.
fn props(conn: &Connection, service: &str, iface: &str) -> anyhow::Result<HashMap<String, OwnedValue>> {
    let p = Proxy::new(conn, service, PATH, PROPS)?;
    Ok(p.call("GetAll", &(iface,))?)
}

/// Current position in microseconds (only on the player interface, and not in `GetAll`).
fn position(conn: &Connection, service: &str) -> anyhow::Result<i64> {
    let p = Proxy::new(conn, service, PATH, PLAYER)?;
    Ok(p.get_property::<i64>("Position")?)
}

/// Properties -> snapshot. Kept apart from D-Bus so it can be tested.
pub fn parse(
    service: &str,
    root: &HashMap<String, OwnedValue>,
    player: &HashMap<String, OwnedValue>,
    position_us: Option<i64>,
) -> MediaSnapshot {
    let meta = player
        .get("Metadata")
        .and_then(|v| HashMap::<String, OwnedValue>::try_from(v.clone()).ok())
        .unwrap_or_default();
    MediaSnapshot {
        ok: true,
        error: String::new(),
        player: service.to_string(),
        app: text(root.get("Identity")),
        state: text(player.get("PlaybackStatus")),
        title: text(meta.get("xesam:title")),
        artist: names(meta.get("xesam:artist")),
        album: text(meta.get("xesam:album")),
        length: seconds(micros(meta.get("mpris:length"))),
        position: seconds(position_us.unwrap_or(0)),
        can_play: flag(player, "CanPlay"),
        can_pause: flag(player, "CanPause"),
        can_previous: flag(player, "CanGoPrevious"),
        can_next: flag(player, "CanGoNext"),
        can_seek: flag(player, "CanSeek"),
    }
}

/// Runs a command on the player. Verbs: play-pause, next, prev, back, fwd, raise.
/// `seek_us` is the jump of the back/forward buttons.
pub fn send(service: &str, verb: &str, seek_us: i64) -> anyhow::Result<()> {
    if !valid_service(service) {
        anyhow::bail!("{}", tr!("invalid player"));
    }
    let conn = connect()?;
    if verb == "raise" {
        let root = Proxy::new(&conn, service, PATH, ROOT)?;
        root.call_method("Raise", &())?;
        return Ok(());
    }
    let player = Proxy::new(&conn, service, PATH, PLAYER)?;
    match verb {
        "play-pause" => {
            player.call_method("PlayPause", &())?;
        }
        "next" => {
            player.call_method("Next", &())?;
        }
        "prev" => {
            player.call_method("Previous", &())?;
        }
        "back" => {
            player.call_method("Seek", &(-seek_us.abs(),))?;
        }
        "fwd" => {
            player.call_method("Seek", &(seek_us.abs(),))?;
        }
        _ => {}
    }
    Ok(())
}

/// Service name that came from the bus: only the MPRIS format is accepted.
pub fn valid_service(service: &str) -> bool {
    service.starts_with(PREFIX)
        && service.len() <= 255
        && service.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn text(v: Option<&OwnedValue>) -> String {
    v.and_then(|v| String::try_from(v.clone()).ok()).unwrap_or_default()
}

/// List of names (`as`, as the spec says) or a single string, which some players send
/// instead.
fn names(v: Option<&OwnedValue>) -> String {
    let Some(v) = v else { return String::new() };
    match Vec::<String>::try_from(v.clone()) {
        Ok(list) => list.join(", "),
        Err(_) => text(Some(v)),
    }
}

/// Length in microseconds: `x` (i64) per the spec, but some players send `t` (u64) or
/// `i` (i32).
fn micros(v: Option<&OwnedValue>) -> i64 {
    let Some(v) = v else { return 0 };
    i64::try_from(v)
        .ok()
        .or_else(|| u64::try_from(v).ok().map(|n| n.min(i64::MAX as u64) as i64))
        .or_else(|| i32::try_from(v).ok().map(i64::from))
        .unwrap_or(0)
}

fn flag(props: &HashMap<String, OwnedValue>, key: &str) -> bool {
    props.get(key).and_then(|v| bool::try_from(v).ok()).unwrap_or(false)
}

/// Microseconds -> seconds (negative becomes zero).
fn seconds(us: i64) -> u32 {
    (us.max(0) / 1_000_000) as u32
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::media::mmss;
    use zbus::zvariant::Value;

    fn map(pairs: &[(&str, Value<'static>)]) -> HashMap<String, OwnedValue> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), OwnedValue::try_from(v.clone()).unwrap())).collect()
    }

    fn metadata(pairs: &[(&str, Value<'static>)]) -> Value<'static> {
        Value::from(map(pairs))
    }

    fn player_props(state: &str) -> HashMap<String, OwnedValue> {
        map(&[
            ("PlaybackStatus", Value::from(state.to_string())),
            (
                "Metadata",
                metadata(&[
                    ("xesam:title", Value::from("Céu de Brasília")),
                    ("xesam:artist", Value::from(vec!["Tonico".to_string(), "Tinoco".to_string()])),
                    ("xesam:album", Value::from("Viola")),
                    ("mpris:length", Value::from(187_000_000i64)),
                ]),
            ),
            ("CanGoNext", Value::from(true)),
            ("CanGoPrevious", Value::from(false)),
            ("CanSeek", Value::from(true)),
            ("CanPlay", Value::from(true)),
            ("CanPause", Value::from(true)),
        ])
    }

    #[test]
    fn reads_metadata_and_capabilities() {
        let root = map(&[("Identity", Value::from("firefox"))]);
        let s = parse("org.mpris.MediaPlayer2.firefox", &root, &player_props("Playing"), Some(42_600_000));
        assert_eq!(s.app, "firefox");
        assert_eq!(s.state, "Playing");
        assert!(s.playing());
        assert_eq!(s.title, "Céu de Brasília");
        assert_eq!(s.artist, "Tonico, Tinoco");
        assert_eq!(s.album, "Viola");
        assert_eq!(s.length, 187);
        assert_eq!(s.position, 42);
        assert!(s.can_next && s.can_play && s.can_seek);
        assert!(!s.can_previous);
        assert!(s.has_player());
        assert_eq!(s.summary(), "CD Player: Céu de Brasília — Tonico, Tinoco");
    }

    #[test]
    fn player_without_metadata_does_not_break() {
        let root = HashMap::new();
        let s = parse("org.mpris.MediaPlayer2.x", &root, &HashMap::new(), None);
        assert!(s.ok);
        assert!(!s.playing());
        assert_eq!(s.title, "");
        assert_eq!(s.length, 0);
        assert_eq!(s.summary(), "CD Player: org.mpris.MediaPlayer2.x open");
    }

    #[test]
    fn unchanged_snapshot_is_republished_on_request() {
        let mut provider = MediaProvider::default();
        let snap = MediaSnapshot { ok: true, state: "Paused".into(), ..Default::default() };
        assert_eq!(provider.publish(snap.clone(), false), Some(snap.clone()));
        assert_eq!(provider.publish(snap.clone(), false), None);
        assert_eq!(provider.publish(snap.clone(), true), Some(snap));
    }

    #[test]
    fn paused_position_is_kept_in_snapshot() {
        let s = parse("org.mpris.MediaPlayer2.x", &HashMap::new(), &player_props("Paused"), Some(42_000_000));
        assert!(s.paused());
        assert_eq!(s.position, 42);
    }

    #[test]
    fn negative_position_is_zero_and_time_format() {
        assert_eq!(seconds(-5), 0);
        assert_eq!(seconds(1_999_999), 1);
        assert_eq!(mmss(0), "0:00");
        assert_eq!(mmss(7), "0:07");
        assert_eq!(mmss(187), "3:07");
        assert_eq!(mmss(3723), "1:02:03");
    }

    #[test]
    fn picks_the_most_useful_player() {
        let base = MediaSnapshot { ok: true, title: "x".into(), ..Default::default() };
        let playing_no_length = MediaSnapshot { player: "a".into(), state: "Playing".into(), ..base.clone() };
        let paused_with_length = MediaSnapshot { player: "b".into(), state: "Paused".into(), length: 100, ..base.clone() };
        let playing_with_length =
            MediaSnapshot { player: "b".into(), state: "Playing".into(), length: 100, ..base.clone() };
        // playing beats knowing the length; between two playing, the one that knows the length wins
        assert!(usable(&playing_no_length, "a") > usable(&paused_with_length, "a"));
        assert!(usable(&playing_with_length, "a") > usable(&playing_no_length, "a"));
        // full tie: the one already in use stays
        assert!(usable(&playing_with_length, "b") > usable(&playing_with_length, "a"));
    }

    #[test]
    fn only_accepts_mpris_services() {
        assert!(valid_service("org.mpris.MediaPlayer2.firefox.instance_1_1202"));
        assert!(valid_service("org.mpris.MediaPlayer2.plasma-browser-integration"));
        assert!(!valid_service("org.mpris.MediaPlayer2.x; rm -rf /"));
        assert!(!valid_service("org.example.Player"));
        assert!(!valid_service(""));
    }

    #[test]
    fn accepts_unsigned_length_and_artist_as_string() {
        let player = map(&[(
            "Metadata",
            metadata(&[("xesam:artist", Value::from("Tonico")), ("mpris:length", Value::from(187_000_000u64))]),
        )]);
        let snap = parse("org.mpris.MediaPlayer2.x", &HashMap::new(), &player, None);
        assert_eq!(snap.artist, "Tonico");
        assert_eq!(snap.length, 187);
    }

}
