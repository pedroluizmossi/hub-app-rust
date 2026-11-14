//! CD Player mini-app: the active player (MPRIS on Linux, the system media controls on
//! Windows), with a spinning disc, track and controls.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::ComponentHandle;

use crate::core::bus::{Event, Topic};
use crate::core::manifest::Manifest;
use crate::core::miniapp::{Host, MiniApp, Report, TrayMedia};
use crate::mascot::{Mood, disc, render};
use crate::providers::media::{self, MediaSnapshot};
use crate::tr;
use crate::ui::{CdPlayerApp, HubWindow};

/// Spin frames between readings (the UI `tick` advances them).
const SPIN_LIMIT: u8 = disc::STEPS;
/// How long a failed command stays at the top.
const ERROR_TTL: Duration = Duration::from_secs(8);

pub struct CdPlayerMini {
    host: Rc<Host>,
    manifest: Manifest,
    snap: MediaSnapshot,
    mood: Mood,
    /// Last disc frame handed to the UI.
    disc: disc::Cache,
    /// Current frame of the turn.
    spin: u8,
    /// Jump of the forward/back buttons, in seconds (from the config).
    seek_step: u32,
    /// Track already announced (doesn't notify again what was already playing when the app
    /// was turned on).
    announced: Option<String>,
    baseline: bool,
    /// Failure of the last command, shown at the top for `ERROR_TTL`.
    error: String,
    error_at: Option<Instant>,
}

impl CdPlayerMini {
    pub fn create(host: Rc<Host>, ui: &HubWindow) -> Rc<RefCell<Self>> {
        let manifest = Manifest::parse(include_str!("../../miniapps/cdplayer/manifest.toml"))
            .expect("invalid CD Player manifest");
        let app = Rc::new(RefCell::new(CdPlayerMini {
            host,
            manifest,
            snap: MediaSnapshot::default(),
            mood: Mood::Sleep,
            disc: Default::default(),
            spin: 0,
            seek_step: 10,
            announced: None,
            baseline: false,
            error: String::new(),
            error_at: None,
        }));
        Self::bind(&app, ui);
        app
    }

    fn bind(app: &Rc<RefCell<Self>>, ui: &HubWindow) {
        let g = ui.global::<CdPlayerApp>();
        let a = Rc::downgrade(app);
        g.on_command(move |verb| {
            if let Some(a) = a.upgrade() {
                a.borrow().command(&verb);
            }
        });
        let a = Rc::downgrade(app);
        g.on_seek_to(move |fraction| {
            if let Some(a) = a.upgrade() {
                a.borrow().seek_to(fraction);
            }
        });
    }

    /// Player command (a button or the tray menu).
    fn command(&self, verb: &str) {
        let step = self.host.config.borrow().int(&self.manifest, "seek").clamp(1, 300) * 1_000_000;
        let verb = verb.to_string();
        self.run(verb.clone(), move |player| media::send(player, &verb, step));
    }

    /// Click on the progress bar: jump to `fraction` (0..1) of the track.
    fn seek_to(&self, fraction: f32) {
        if !self.snap.can_seek || self.snap.length == 0 {
            return;
        }
        let fraction = f64::from(fraction.clamp(0.0, 1.0));
        self.run("seek-to".into(), move |player| media::seek_to(player, fraction));
    }

    /// Runs `action` on the current player in a thread: the system may take a while and the
    /// UI doesn't wait. Then asks for a fresh reading; a failure reaches the app through
    /// `on_report`, labeled `what`.
    fn run(&self, what: String, action: impl FnOnce(&str) -> anyhow::Result<()> + Send + 'static) {
        if !media::valid_service(&self.snap.player) {
            return;
        }
        let (player, poker, reporter) =
            (self.snap.player.clone(), self.host.bus.poker(Topic::Media), self.host.reporter(&self.manifest));
        let failed = reporter.clone();
        let spawned = std::thread::Builder::new().name("media-command".into()).spawn(move || {
            let result = action(&player);
            poker.poke();
            reporter.result(&what, &result);
        });
        if let Err(e) = spawned {
            failed.error(tr!("No thread for the command: {}", e));
        }
    }

    fn ingest(&mut self, snap: MediaSnapshot) {
        let notify = self.host.config.borrow().bool(&self.manifest, "notify_track");
        if !self.baseline && !snap.title.is_empty() {
            self.announced = Some(format!("{}|{}", snap.player, snap.title));
        }
        if self.baseline && notify && snap.playing() && !snap.title.is_empty() {
            let key = format!("{}|{}", snap.player, snap.title);
            if self.announced.as_deref() != Some(key.as_str()) {
                let body = match (snap.artist.is_empty(), snap.album.is_empty()) {
                    (false, false) => format!("{} · {}", snap.artist, snap.album),
                    (false, true) => snap.artist.clone(),
                    (true, false) => snap.album.clone(),
                    (true, true) => snap.app.clone(),
                };
                self.host.notifier.borrow_mut().send("cd:track", &format!("▶ {}", snap.title), &body, false);
                self.announced = Some(key);
            }
        }
        self.baseline = true;
        self.snap = snap;
        self.mood = self.compute_mood();
    }

    fn compute_mood(&self) -> Mood {
        if !self.snap.ok || !self.snap.has_player() {
            Mood::Sleep
        } else if self.snap.playing() {
            Mood::Happy
        } else {
            Mood::Idle
        }
    }

    fn state_text(&self) -> &'static str {
        if !self.snap.ok {
            // before the first reading the snapshot has no error yet: it's "no disc", not a failure
            return if self.snap.error.is_empty() { tr!("NO DISC") } else { tr!("NO BUS") };
        }
        if !self.snap.has_player() {
            tr!("NO DISC")
        } else if self.snap.playing() {
            tr!("PLAYING")
        } else if self.snap.paused() {
            tr!("PAUSED")
        } else {
            tr!("STOPPED")
        }
    }

    /// Position capped at the track length (some players overshoot).
    fn progress(&self) -> f32 {
        if self.snap.length == 0 {
            return 0.0;
        }
        (self.snap.position.min(self.snap.length) as f32) / self.snap.length as f32
    }

    fn time_text(&self) -> String {
        if self.snap.length > 0 {
            format!("{} / {}", media::mmss(self.snap.position), media::mmss(self.snap.length))
        } else if self.snap.has_player() {
            media::mmss(self.snap.position)
        } else {
            String::new()
        }
    }

    fn spin(&self) -> u8 {
        if self.snap.playing() { self.spin % SPIN_LIMIT } else { 0 }
    }

    fn push(&mut self, ui: &HubWindow) {
        let theme = self.host.theme_for(&self.manifest);
        let g = ui.global::<CdPlayerApp>();
        g.set_theme(render::colors(theme));
        g.set_ok(self.snap.ok);
        let error = if self.error.is_empty() { self.snap.error.clone() } else { self.error.clone() };
        g.set_error_text(error.into());
        g.set_state_text(self.state_text().into());
        g.set_source(if self.snap.app.is_empty() { self.snap.player.clone() } else { self.snap.app.clone() }.into());
        g.set_title(self.snap.title.clone().into());
        g.set_artist(self.snap.artist.clone().into());
        g.set_album(self.snap.album.clone().into());
        g.set_playing(self.snap.playing());
        g.set_has_player(self.snap.has_player());
        g.set_can_prev(self.snap.can_previous);
        g.set_can_next(self.snap.can_next);
        g.set_can_seek(self.snap.can_seek);
        g.set_has_length(self.snap.length > 0);
        g.set_can_raise(self.snap.can_raise);
        g.set_can_play_pause(self.snap.can_play_pause());
        g.set_backend(media::SOURCE.into());
        g.set_progress(self.progress());
        g.set_time_text(self.time_text().into());
        g.set_seek_label(self.seek_step.to_string().into());
        g.set_mood(self.mood.as_str().into());
        // the disc is only redrawn when the frame, the theme or the state change
        let lit = self.snap.ok && self.snap.has_player();
        self.disc.update(theme, self.spin(), lit, self.snap.art.as_ref());
        g.set_disc(self.disc.image());
    }
}

impl MiniApp for CdPlayerMini {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn enable(&mut self, ui: &HubWindow) {
        self.seek_step = self.host.config.borrow().int(&self.manifest, "seek").clamp(1, 300) as u32;
        self.baseline = false;
        self.announced = None;
        self.spin = 0;
        self.mood = self.compute_mood();
        self.host.bus.poke(Topic::Media);
        self.push(ui);
    }

    fn disable(&mut self, _ui: &HubWindow) {
        self.announced = None;
        self.baseline = false;
        self.snap = MediaSnapshot::default();
        self.mood = Mood::Sleep;
        self.error.clear();
        self.error_at = None;
    }

    fn on_event(&mut self, ev: &Event, ui: &HubWindow) {
        let Event::Media(snap) = ev else { return };
        self.ingest(snap.clone());
        self.push(ui);
    }

    fn on_report(&mut self, report: &Report, ui: &HubWindow) {
        // a command that worked shows up in the player's next reading
        if let Report::Error(msg) = report {
            self.error = msg.clone();
            self.error_at = Some(Instant::now());
            self.push(ui);
        }
    }

    fn tick(&mut self, _frame: u32, ui: &HubWindow) {
        // a failed command's message goes away on its own
        if self.error_at.is_some_and(|t| t.elapsed() >= ERROR_TTL) {
            self.error.clear();
            self.error_at = None;
            self.push(ui);
        }
        // with "Reduce motion" the disc stands still (it only shows the state)
        if !self.snap.playing() || crate::mascot::anim::calm() {
            return;
        }
        self.spin = self.spin.wrapping_add(1) % SPIN_LIMIT;
        let theme = self.host.theme_for(&self.manifest);
        let lit = self.snap.ok && self.snap.has_player();
        if self.disc.update(theme, self.spin, lit, self.snap.art.as_ref()) {
            ui.global::<CdPlayerApp>().set_disc(self.disc.image());
        }
    }

    fn settings_changed(&mut self, ui: &HubWindow) {
        self.seek_step = self.host.config.borrow().int(&self.manifest, "seek").clamp(1, 300) as u32;
        self.push(ui);
    }

    fn mood(&self) -> Mood {
        self.mood
    }

    fn mascot_key(&self) -> String {
        self.manifest.icon.clone()
    }

    fn summary(&self) -> String {
        self.snap.summary()
    }

    fn tray_command(&mut self, verb: &str, _ui: &HubWindow) {
        self.command(verb);
    }

    fn tray_media(&self) -> Option<TrayMedia> {
        if !self.host.config.borrow().bool(&self.manifest, "tray_controls") {
            return None;
        }
        Some(TrayMedia {
            summary: self.snap.summary(),
            playing: self.snap.playing(),
            can_play_pause: self.snap.can_play_pause(),
            can_previous: self.snap.can_previous,
            can_next: self.snap.can_next,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> CdPlayerMini {
        CdPlayerMini {
            host: test_host(),
            manifest: Manifest::parse(include_str!("../../miniapps/cdplayer/manifest.toml")).unwrap(),
            snap: MediaSnapshot::default(),
            mood: Mood::Sleep,
            disc: Default::default(),
            spin: 0,
            seek_step: 10,
            announced: None,
            baseline: false,
            error: String::new(),
            error_at: None,
        }
    }

    /// A real host, without touching the disk or starting threads.
    fn test_host() -> Rc<Host> {
        Rc::new(Host::new(crate::core::config::Config::default(), crate::core::bus::Bus::default()))
    }

    #[test]
    fn states_and_mood() {
        let mut a = app();
        assert_eq!(a.state_text(), "NO DISC");
        assert_eq!(a.compute_mood(), Mood::Sleep);
        assert_eq!(a.progress(), 0.0);

        a.snap = MediaSnapshot { ok: true, state: "Playing".into(), player: "org.mpris.MediaPlayer2.x".into(), ..Default::default() };
        assert_eq!(a.state_text(), "PLAYING");
        assert_eq!(a.compute_mood(), Mood::Happy);

        a.snap.state = "Paused".into();
        assert_eq!(a.state_text(), "PAUSED");
        assert_eq!(a.compute_mood(), Mood::Idle);

        a.snap.ok = false;
        a.snap.error = "bus went down".into();
        assert_eq!(a.state_text(), "NO BUS");
        assert_eq!(a.compute_mood(), Mood::Sleep);
        // no error yet means the first reading: no disc, not a failure
        a.snap.error.clear();
        assert_eq!(a.state_text(), "NO DISC");
    }

    #[test]
    fn time_and_progress() {
        let mut a = app();
        a.snap = MediaSnapshot {
            ok: true,
            player: "org.mpris.MediaPlayer2.x".into(),
            state: "Playing".into(),
            position: 42,
            length: 180,
            ..Default::default()
        };
        assert_eq!(a.progress(), 42.0 / 180.0);
        assert_eq!(a.time_text(), "0:42 / 3:00");
        // a position past the end (buggy player) doesn't overfill the bar
        a.snap.position = 300;
        assert_eq!(a.progress(), 1.0);
        // no known length
        a.snap.length = 0;
        assert_eq!(a.progress(), 0.0);
        assert_eq!(a.time_text(), "5:00");
    }

    #[test]
    fn first_track_becomes_the_reference_and_readings_keep_the_error() {
        let mut a = app();
        a.host.config.borrow_mut().set("cyber.cd", "notify_track", toml::Value::Boolean(true));
        let snap = MediaSnapshot {
            ok: true,
            player: "org.mpris.MediaPlayer2.x".into(),
            state: "Playing".into(),
            title: "Current track".into(),
            ..Default::default()
        };
        a.error = "command failed".into();
        let at = Instant::now();
        a.error_at = Some(at);
        a.ingest(snap.clone());
        assert_eq!(a.announced.as_deref(), Some("org.mpris.MediaPlayer2.x|Current track"));
        a.ingest(MediaSnapshot { position: 1, ..snap });
        assert_eq!(a.announced.as_deref(), Some("org.mpris.MediaPlayer2.x|Current track"));
        assert_eq!(a.error, "command failed");
        assert_eq!(a.error_at, Some(at));
    }

    #[test]
    fn tray_menu_follows_config_and_capabilities() {
        let mut a = app();
        assert!(a.tray_media().is_none());
        // no player: play/pause does nothing
        assert!(!a.snap.can_play_pause());
        a.host.config.borrow_mut().set("cyber.cd", "tray_controls", toml::Value::Boolean(true));
        a.snap = MediaSnapshot {
            ok: true,
            player: "org.mpris.MediaPlayer2.x".into(),
            title: "Song".into(),
            artist: "Artist".into(),
            state: "Playing".into(),
            can_pause: true,
            can_next: true,
            ..Default::default()
        };
        let media = a.tray_media().unwrap();
        assert_eq!(media.summary, "CD Player: Song — Artist");
        assert!(media.playing && media.can_play_pause && media.can_next);
        assert!(!media.can_previous);
        a.snap.state = "Paused".into();
        assert!(!a.tray_media().unwrap().can_play_pause);
    }
}
