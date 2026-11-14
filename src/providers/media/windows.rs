//! System media controls on Windows (`GlobalSystemMediaTransportControls`): the same
//! sessions that show up in the volume flyout — Spotify, browsers, players.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session, GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};

use windows::Storage::Streams::DataReader;

use super::{ART_MAX_BYTES, ArtCache, MediaSnapshot, Score, friendly_app, usable};
use crate::tr;

/// Where the players come from (System Media Transport Controls), for the label in the
/// CD Player display.
pub const SOURCE: &str = "SMTC";
/// After a failure, wait this long before asking for the service again.
const RETRY: Duration = Duration::from_secs(10);
/// Reads of the same track's cover before giving up (the cover may come late).
const ART_TRIES: u8 = 4;
/// Windows counts time in 100 ns steps.
const TICKS_PER_SEC: i64 = 10_000_000;
/// Seconds between 1601 (start of the Windows clock) and 1970 (start of Unix time).
const EPOCH_GAP_SECS: i64 = 11_644_473_600;

#[derive(Default)]
pub struct MediaProvider {
    manager: Option<Manager>,
    /// Player in use (keeps the choice between readings).
    player: String,
    last: Option<MediaSnapshot>,
    retry_at: Option<Instant>,
    /// Cover of the current track (read once per track).
    art: ArtCache,
}

impl MediaProvider {
    /// Called every second while the mini-app is enabled. `Some` when something changed.
    pub fn collect(&mut self, force: bool) -> Option<MediaSnapshot> {
        let snap = self.read();
        let changed = self.last.as_ref() != Some(&snap);
        self.last = Some(snap.clone());
        (force || changed).then_some(snap)
    }

    fn read(&mut self) -> MediaSnapshot {
        if self.manager.is_none() {
            if self.retry_at.is_some_and(|t| Instant::now() < t) {
                return self.fail(tr!("media service unavailable"));
            }
            match manager() {
                Ok(m) => {
                    self.manager = Some(m);
                    self.retry_at = None;
                }
                Err(e) => {
                    self.retry_at = Some(Instant::now() + RETRY);
                    return self.fail(&e.message());
                }
            }
        }
        let manager = self.manager.clone().expect("service just opened");
        match self.poll(&manager) {
            Ok(snap) => snap,
            Err(e) => {
                self.manager = None;
                self.retry_at = Some(Instant::now() + RETRY);
                self.fail(&e.message())
            }
        }
    }

    fn fail(&mut self, reason: &str) -> MediaSnapshot {
        self.player.clear();
        MediaSnapshot { ok: false, error: reason.lines().next().unwrap_or_default().to_string(), ..Default::default() }
    }

    fn poll(&mut self, manager: &Manager) -> windows::core::Result<MediaSnapshot> {
        // Several apps may have a session open. Keep the one that is playing and knows
        // length/position; a tie keeps the current one.
        let mut best: Option<(Score, MediaSnapshot, Session)> = None;
        for session in manager.GetSessions()? {
            let Ok(snap) = snapshot(&session) else { continue };
            let score = usable(&snap, &self.player);
            if best.as_ref().is_none_or(|(s, _, _)| score > *s) {
                best = Some((score, snap, session));
            }
        }
        match best {
            Some((_, mut snap, session)) => {
                self.player = snap.player.clone();
                // the session has no cover id: the track stands for it
                let key = if snap.title.is_empty() {
                    String::new()
                } else {
                    format!("{}|{}|{}|{}", snap.player, snap.title, snap.artist, snap.album)
                };
                // the app may hand the cover a moment after the title: a few more reads
                snap.art = self.art.get(&key, ART_TRIES, || thumbnail(&session));
                Ok(snap)
            }
            None => {
                self.player.clear();
                Ok(MediaSnapshot { ok: true, ..Default::default() })
            }
        }
    }
}

/// The session manager. The call waits for the answer: never on the UI thread.
fn manager() -> windows::core::Result<Manager> {
    Manager::RequestAsync()?.join()
}

/// Now, on the Windows clock (100 ns steps since 1601).
fn now_ticks() -> i64 {
    let unix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    (unix.as_secs() as i64 + EPOCH_GAP_SECS) * TICKS_PER_SEC + unix.subsec_nanos() as i64 / 100
}

/// Start, end and current position of the track, in 100 ns steps.
///
/// Apps only report the position now and then: while playing, add the time elapsed since
/// the last report.
fn timeline(session: &Session, playing: bool) -> windows::core::Result<(i64, i64, i64)> {
    let t = session.GetTimelineProperties()?;
    let (start, end) = (t.StartTime()?.Duration, t.EndTime()?.Duration);
    let mut position = t.Position()?.Duration;
    let updated = t.LastUpdatedTime()?.UniversalTime;
    if playing && updated > 0 {
        position += (now_ticks() - updated).max(0);
    }
    if end > start {
        position = position.clamp(start, end);
    }
    Ok((start, end, position))
}

fn snapshot(session: &Session) -> windows::core::Result<MediaSnapshot> {
    let id = session.SourceAppUserModelId()?.to_string();
    let info = session.GetPlaybackInfo()?;
    let state = match info.PlaybackStatus()? {
        Status::Playing => "Playing",
        Status::Paused => "Paused",
        _ => "Stopped",
    };
    let controls = info.Controls()?;
    let can = |flag: windows::core::Result<bool>| flag.unwrap_or(false);
    let toggle = can(controls.IsPlayPauseToggleEnabled());
    let (start, end, position) = timeline(session, state == "Playing").unwrap_or_default();
    let seconds = |ticks: i64| (ticks.max(0) / TICKS_PER_SEC) as u32;
    // title and artist may be missing (an app that only opened the session): still a player
    let props = session.TryGetMediaPropertiesAsync().and_then(|op| op.join()).ok();
    let text = |get: &dyn Fn(&windows::Media::Control::GlobalSystemMediaTransportControlsSessionMediaProperties) -> windows::core::Result<windows::core::HSTRING>| {
        props.as_ref().and_then(|p| get(p).ok()).map(|s| s.to_string()).unwrap_or_default()
    };
    Ok(MediaSnapshot {
        ok: true,
        error: String::new(),
        app: friendly_app(&id),
        player: id,
        state: state.to_string(),
        title: text(&|p| p.Title()),
        artist: text(&|p| p.Artist()),
        album: text(&|p| p.AlbumTitle()),
        length: seconds(end - start),
        position: seconds(position - start),
        can_play: toggle || can(controls.IsPlayEnabled()),
        can_pause: toggle || can(controls.IsPauseEnabled()),
        can_previous: can(controls.IsPreviousEnabled()),
        can_next: can(controls.IsNextEnabled()),
        can_seek: can(controls.IsPlaybackPositionEnabled()),
        // Windows has no way to bring another app's window to the front from here
        can_raise: false,
        // the cover is read by the provider (`ArtCache`), only for the chosen session
        art: None,
    })
}

/// Bytes of the session's cover (`Thumbnail`, usually a PNG or JPEG), if the app gives one.
fn thumbnail(session: &Session) -> Option<Vec<u8>> {
    let props = session.TryGetMediaPropertiesAsync().ok()?.join().ok()?;
    let stream = props.Thumbnail().ok()?.OpenReadAsync().ok()?.join().ok()?;
    let size = stream.Size().ok()?;
    if size == 0 || size > ART_MAX_BYTES {
        return None;
    }
    let reader = DataReader::CreateDataReader(&stream).ok()?;
    let read = reader.LoadAsync(size as u32).ok()?.join().ok()?;
    let mut bytes = vec![0u8; read as usize];
    reader.ReadBytes(&mut bytes).ok()?;
    Some(bytes)
}

/// The open session of player `service`.
fn session_of(service: &str) -> anyhow::Result<Session> {
    if !valid_service(service) {
        anyhow::bail!("{}", tr!("invalid player"));
    }
    let mut sessions = manager()?.GetSessions()?.into_iter();
    sessions
        .find(|s| s.SourceAppUserModelId().is_ok_and(|id| id == service))
        .ok_or_else(|| anyhow::anyhow!("{}", tr!("the player closed")))
}

/// Jumps to `fraction` (0..1) of the track (a click on the progress bar).
pub fn seek_to(service: &str, fraction: f64) -> anyhow::Result<()> {
    let session = session_of(service)?;
    let (start, end, _) = timeline(&session, false)?;
    anyhow::ensure!(end > start, "{}", tr!("the track has no known length"));
    let target = start + ((end - start) as f64 * fraction.clamp(0.0, 1.0)) as i64;
    let accepted = session.TryChangePlaybackPositionAsync(target)?.join()?;
    anyhow::ensure!(accepted, "{}", tr!("the player refused the command"));
    Ok(())
}

/// Runs a command on the player. Verbs: play-pause, next, prev, back, fwd, raise.
/// `seek_us` is the jump of the back/forward buttons, in microseconds.
pub fn send(service: &str, verb: &str, seek_us: i64) -> anyhow::Result<()> {
    let session = session_of(service)?;
    let accepted = match verb {
        "play-pause" => session.TryTogglePlayPauseAsync()?.join()?,
        "next" => session.TrySkipNextAsync()?.join()?,
        "prev" => session.TrySkipPreviousAsync()?.join()?,
        "back" | "fwd" => {
            let playing = session.GetPlaybackInfo()?.PlaybackStatus()? == Status::Playing;
            let (start, end, position) = timeline(&session, playing)?;
            // microseconds -> 100 ns steps
            let step = seek_us.abs().saturating_mul(10);
            let target = if verb == "back" { position - step } else { position + step };
            let target = if end > start { target.clamp(start, end) } else { target.max(0) };
            session.TryChangePlaybackPositionAsync(target)?.join()?
        }
        // Windows has no way to bring another app's window to the front from here
        _ => true,
    };
    anyhow::ensure!(accepted, "{}", tr!("the player refused the command"));
    Ok(())
}

/// Session identifier that came from the system: any short, non-empty text.
pub fn valid_service(service: &str) -> bool {
    !service.is_empty() && service.len() <= 255
}
