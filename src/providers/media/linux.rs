//! MPRIS over D-Bus: who is playing, what, and the player commands.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use super::{ART_MAX_BYTES, Art, ArtCache, MediaSnapshot, Score, file_url_path, usable};
use crate::tr;

/// Where the players come from, for the label in the CD Player display.
pub const SOURCE: &str = "MPRIS";
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
    /// Cover of the current track (read once per `mpris:artUrl`).
    art: ArtCache,
    /// Download of an HTTP(S) cover running on its own thread: the URL and the slot where
    /// the thread leaves the bytes (`Some(None)`: the download failed).
    art_job: Option<(String, ArtSlot)>,
}

type ArtSlot = Arc<Mutex<Option<Option<Vec<u8>>>>>;


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
        let mut best: Option<(Score, MediaSnapshot, String)> = None;
        for name in &names {
            let Ok(player) = props(conn, name, PLAYER) else { continue };
            let root = props(conn, name, ROOT).unwrap_or_default();
            let position = position(conn, name).ok();
            let snap = parse(name, &root, &player, position);
            let score = usable(&snap, &self.player);
            if best.as_ref().is_none_or(|(s, _, _)| score > *s) {
                best = Some((score, snap, text(metadata(&player).get("mpris:artUrl"))));
            }
        }
        match best {
            Some((_, mut snap, art_url)) => {
                self.player = snap.player.clone();
                // a late cover comes with a new URL (a new key): one read per URL, so a
                // download that fails (offline) does not hold the readings again
                snap.art = if is_http(&art_url) {
                    self.http_art(&art_url, download_art)
                } else {
                    self.art.get(&art_url, 1, || file_art(&art_url))
                };
                Ok(snap)
            }
            None => {
                self.player.clear();
                Ok(MediaSnapshot { ok: true, ..Default::default() })
            }
        }
    }
}

impl MediaProvider {
    /// The cover at an HTTP(S) URL, downloaded on its own thread so a slow or offline
    /// network doesn't hold the readings: `None` until the bytes arrive, and then the
    /// snapshot changes on a later tick. One download per URL.
    fn http_art(&mut self, url: &str, fetch: fn(&str) -> Option<Vec<u8>>) -> Option<Art> {
        match &self.art_job {
            Some((job, slot)) if job == url => {
                let done = slot.lock().ok().and_then(|mut s| s.take());
                match done {
                    Some(bytes) => self.art.get(url, 1, || bytes),
                    // still downloading, or already read: whatever the cache has
                    None => self.art.get(url, 0, || None),
                }
            }
            _ => {
                let slot = ArtSlot::default();
                let (thread_slot, thread_url) = (slot.clone(), url.to_string());
                let spawned = std::thread::Builder::new().name("media-art".into()).spawn(move || {
                    let bytes = fetch(&thread_url);
                    if let Ok(mut s) = thread_slot.lock() {
                        *s = Some(bytes);
                    }
                });
                if let Err(e) = spawned {
                    log::warn!("media: could not start the cover download: {e}");
                }
                self.art_job = Some((url.to_string(), slot));
                self.art.get(url, 0, || None)
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
/// A plain `Properties.Get`: a proxy's `get_property` would set up a property cache (a
/// match rule and a `GetAll`) on every call, since the proxy lives only for this reading.
fn position(conn: &Connection, service: &str) -> anyhow::Result<i64> {
    let p = Proxy::new(conn, service, PATH, PROPS)?;
    let value: OwnedValue = p.call("Get", &(PLAYER, "Position"))?;
    Ok(i64::try_from(value)?)
}

/// Properties -> snapshot. Kept apart from D-Bus so it can be tested.
pub fn parse(
    service: &str,
    root: &HashMap<String, OwnedValue>,
    player: &HashMap<String, OwnedValue>,
    position_us: Option<i64>,
) -> MediaSnapshot {
    let meta = metadata(player);
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
        can_raise: flag(root, "CanRaise"),
        // the cover is read by the provider (`ArtCache`), not here
        art: None,
    }
}

/// The track's `Metadata` (empty if the player has none).
fn metadata(player: &HashMap<String, OwnedValue>) -> HashMap<String, OwnedValue> {
    player
        .get("Metadata")
        .and_then(|v| HashMap::<String, OwnedValue>::try_from(v.clone()).ok())
        .unwrap_or_default()
}

/// Deadline to download a cover (Spotify gives an `https://` URL).
const ART_TIMEOUT: Duration = Duration::from_secs(4);

fn is_http(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

/// Bytes of the cover at a `file://` URL (`mpris:artUrl` of browsers and local players),
/// capped at `ART_MAX_BYTES`; anything else is ignored.
fn file_art(url: &str) -> Option<Vec<u8>> {
    let path = file_url_path(url)?;
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > ART_MAX_BYTES {
        return None;
    }
    std::fs::read(&path).ok()
}

/// One HTTP client for every cover (building one loads the system certificates).
fn art_client() -> Option<&'static reqwest::blocking::Client> {
    static CLIENT: OnceLock<Option<reqwest::blocking::Client>> = OnceLock::new();
    CLIENT.get_or_init(|| reqwest::blocking::Client::builder().timeout(ART_TIMEOUT).build().ok()).as_ref()
}

/// Bytes of the cover at an HTTP(S) URL (Spotify), capped at `ART_MAX_BYTES`.
fn download_art(url: &str) -> Option<Vec<u8>> {
    let resp = art_client()?.get(url).send().and_then(|r| r.error_for_status()).ok()?;
    if resp.content_length().is_some_and(|n| n > ART_MAX_BYTES) {
        return None;
    }
    // without a declared length (chunked), never more than the limit is read: one byte past
    // it says the cover is too big
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(resp, ART_MAX_BYTES + 1), &mut bytes).ok()?;
    (bytes.len() as u64 <= ART_MAX_BYTES).then_some(bytes)
}

/// Jumps to `fraction` (0..1) of the track (a click on the progress bar): `SetPosition`
/// with the track id, as the spec asks; a player without `mpris:trackid` gets a relative
/// `Seek` from the current position instead.
pub fn seek_to(service: &str, fraction: f64) -> anyhow::Result<()> {
    if !valid_service(service) {
        anyhow::bail!("{}", tr!("invalid player"));
    }
    let conn = connect()?;
    let meta = metadata(&props(&conn, service, PLAYER)?);
    let length = micros(meta.get("mpris:length"));
    anyhow::ensure!(length > 0, "{}", tr!("the track has no known length"));
    let target = (length as f64 * fraction.clamp(0.0, 1.0)) as i64;
    let player = Proxy::new(&conn, service, PATH, PLAYER)?;
    // the id is an object path per the spec; some players send it as a string
    let track = meta.get("mpris:trackid").and_then(|v| {
        OwnedObjectPath::try_from(v.clone()).ok().or_else(|| OwnedObjectPath::try_from(text(Some(v))).ok())
    });
    match track {
        Some(track) => {
            player.call_method("SetPosition", &(track, target))?;
        }
        None => {
            let now = position(&conn, service)?;
            player.call_method("Seek", &(target - now,))?;
        }
    }
    Ok(())
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

    fn cover(_url: &str) -> Option<Vec<u8>> {
        let img = image::RgbImage::from_pixel(8, 8, image::Rgb([255, 0, 0]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).ok()?;
        Some(out.into_inner())
    }

    #[test]
    fn http_cover_arrives_on_a_later_tick() {
        let mut p = MediaProvider::default();
        let url = "https://example.invalid/a.png";
        // the first tick only starts the download
        assert!(p.http_art(url, cover).is_none());
        let deadline = Instant::now() + Duration::from_secs(5);
        let art = loop {
            if let Some(art) = p.http_art(url, cover) {
                break Some(art);
            }
            if Instant::now() > deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(art.is_some());
        // already read: stays cached, no new download
        assert!(p.http_art(url, |_| panic!("downloaded twice")).is_some());
        // a failed download gives no cover and isn't retried
        let other = "https://example.invalid/b.png";
        assert!(p.http_art(other, |_| None).is_none());
        std::thread::sleep(Duration::from_millis(100));
        assert!(p.http_art(other, |_| None).is_none());
        assert!(p.http_art(other, |_| panic!("downloaded twice")).is_none());
    }

    #[test]
    fn reads_metadata_and_capabilities() {
        let root = map(&[("Identity", Value::from("firefox")), ("CanRaise", Value::from(true))]);
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
        assert!(s.can_raise);
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
        // no `CanRaise`: the "open" button stays hidden
        assert!(!s.can_raise);
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
