//! Mini-app CD Player: o player MPRIS ativo, com disco girando, faixa e comandos.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::ComponentHandle;

use crate::core::bus::{Event, Topic};
use crate::core::manifest::Manifest;
use crate::core::miniapp::{Host, MiniApp, Report, TrayMedia};
use crate::mascot::{Mood, disc, render};
use crate::providers::media::{self, MediaSnapshot};
use crate::ui::{CdPlayerApp, HubWindow};

/// Quadro de giro entre leituras (o `tick` da UI avança).
const SPIN_LIMIT: u8 = disc::STEPS;
/// Quanto tempo a falha de um comando fica no topo.
const ERROR_TTL: Duration = Duration::from_secs(8);

pub struct CdPlayerMini {
    host: Rc<Host>,
    manifest: Manifest,
    snap: MediaSnapshot,
    mood: Mood,
    /// Último quadro do disco entregue à UI.
    disc: disc::Cache,
    /// Quadro atual da volta.
    spin: u8,
    /// Pulo dos botões de avançar/voltar, em segundos (da config).
    seek_step: u32,
    /// Faixa já anunciada (não notifica de novo o que já tocava ao ligar o app).
    announced: Option<String>,
    baseline: bool,
    /// Falha do último comando, mostrada no topo por `ERROR_TTL`.
    error: String,
    error_at: Option<Instant>,
}

impl CdPlayerMini {
    pub fn create(host: Rc<Host>, ui: &HubWindow) -> Rc<RefCell<Self>> {
        let manifest = Manifest::parse(include_str!("../../miniapps/cdplayer/manifest.toml"))
            .expect("manifesto do CD Player inválido");
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
    }

    /// Comando do player numa thread: o D-Bus pode demorar e a UI não espera.
    fn command(&self, verb: &str) {
        if !media::valid_service(&self.snap.player) {
            return;
        }
        let step = self.host.config.borrow().int(&self.manifest, "seek").clamp(1, 300) as i64 * 1_000_000;
        let (player, poker, reporter) =
            (self.snap.player.clone(), self.host.bus.poker(Topic::Media), self.host.reporter(&self.manifest));
        let verb = verb.to_string();
        let failed = reporter.clone();
        let spawned = std::thread::Builder::new().name("mpris-comando".into()).spawn(move || {
            let result = media::send(&player, &verb, step);
            poker.poke();
            // a falha chega ao app por `on_report`
            reporter.result(&verb, &result);
        });
        if let Err(e) = spawned {
            failed.error(format!("sem thread para o comando: {e}"));
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
                self.host.notifier.borrow_mut().send("cd:faixa", &format!("▶ {}", snap.title), &body, false);
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
            // antes da primeira leitura o snapshot ainda não tem erro: é "sem disco", não falha
            return if self.snap.error.is_empty() { "SEM DISCO" } else { "SEM BARRAMENTO" };
        }
        if !self.snap.has_player() {
            "SEM DISCO"
        } else if self.snap.playing() {
            "TOCANDO"
        } else if self.snap.paused() {
            "PAUSADO"
        } else {
            "PARADO"
        }
    }

    /// Posição limitada ao tamanho da faixa (alguns players erram para mais).
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
        let erro = if self.error.is_empty() { self.snap.error.clone() } else { self.error.clone() };
        g.set_error_text(erro.into());
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
        g.set_progress(self.progress());
        g.set_time_text(self.time_text().into());
        g.set_seek_label(self.seek_step.to_string().into());
        g.set_mood(self.mood.as_str().into());
        // o disco só é redesenhado quando o quadro, o tema ou o estado mudam
        let lit = self.snap.ok && self.snap.has_player();
        self.disc.update(theme, self.spin(), lit);
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
        // um comando que deu certo aparece na próxima leitura do player
        if let Report::Error(msg) = report {
            self.error = msg.clone();
            self.error_at = Some(Instant::now());
            self.push(ui);
        }
    }

    fn tick(&mut self, _frame: u32, ui: &HubWindow) {
        // a falha de um comando some sozinha
        if self.error_at.is_some_and(|t| t.elapsed() >= ERROR_TTL) {
            self.error.clear();
            self.error_at = None;
            self.push(ui);
        }
        if !self.snap.playing() {
            return;
        }
        self.spin = self.spin.wrapping_add(1) % SPIN_LIMIT;
        let theme = self.host.theme_for(&self.manifest);
        let lit = self.snap.ok && self.snap.has_player();
        if self.disc.update(theme, self.spin, lit) {
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
            can_play_pause: self.snap.has_player()
                && if self.snap.playing() { self.snap.can_pause } else { self.snap.can_play },
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

    /// Host de verdade, sem tocar em disco nem abrir threads.
    fn test_host() -> Rc<Host> {
        Rc::new(Host::new(crate::core::config::Config::default(), crate::core::bus::Bus::default()))
    }

    #[test]
    fn estados_e_humor() {
        let mut a = app();
        assert_eq!(a.state_text(), "SEM DISCO");
        assert_eq!(a.compute_mood(), Mood::Sleep);
        assert_eq!(a.progress(), 0.0);

        a.snap = MediaSnapshot { ok: true, state: "Playing".into(), player: "org.mpris.MediaPlayer2.x".into(), ..Default::default() };
        assert_eq!(a.state_text(), "TOCANDO");
        assert_eq!(a.compute_mood(), Mood::Happy);

        a.snap.state = "Paused".into();
        assert_eq!(a.state_text(), "PAUSADO");
        assert_eq!(a.compute_mood(), Mood::Idle);

        a.snap.ok = false;
        a.snap.error = "barramento caiu".into();
        assert_eq!(a.state_text(), "SEM BARRAMENTO");
        assert_eq!(a.compute_mood(), Mood::Sleep);
        // sem erro ainda é a primeira leitura: sem disco, não falha
        a.snap.error.clear();
        assert_eq!(a.state_text(), "SEM DISCO");
    }

    #[test]
    fn tempo_e_progresso() {
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
        // posição além do fim (player errado) não enche a barra
        a.snap.position = 300;
        assert_eq!(a.progress(), 1.0);
        // sem duração conhecida
        a.snap.length = 0;
        assert_eq!(a.progress(), 0.0);
        assert_eq!(a.time_text(), "5:00");
    }

    #[test]
    fn primeira_faixa_vira_referencia_e_leituras_nao_apagam_erro() {
        let mut a = app();
        a.host.config.borrow_mut().set("cyber.cd", "notify_track", toml::Value::Boolean(true));
        let snap = MediaSnapshot {
            ok: true,
            player: "org.mpris.MediaPlayer2.x".into(),
            state: "Playing".into(),
            title: "Faixa atual".into(),
            ..Default::default()
        };
        a.error = "comando falhou".into();
        let at = Instant::now();
        a.error_at = Some(at);
        a.ingest(snap.clone());
        assert_eq!(a.announced.as_deref(), Some("org.mpris.MediaPlayer2.x|Faixa atual"));
        a.ingest(MediaSnapshot { position: 1, ..snap });
        assert_eq!(a.announced.as_deref(), Some("org.mpris.MediaPlayer2.x|Faixa atual"));
        assert_eq!(a.error, "comando falhou");
        assert_eq!(a.error_at, Some(at));
    }

    #[test]
    fn menu_da_bandeja_respeita_configuracao_e_capacidades() {
        let mut a = app();
        assert!(a.tray_media().is_none());
        a.host.config.borrow_mut().set("cyber.cd", "tray_controls", toml::Value::Boolean(true));
        a.snap = MediaSnapshot {
            ok: true,
            player: "org.mpris.MediaPlayer2.x".into(),
            title: "Música".into(),
            artist: "Artista".into(),
            state: "Playing".into(),
            can_pause: true,
            can_next: true,
            ..Default::default()
        };
        let media = a.tray_media().unwrap();
        assert_eq!(media.summary, "CD Player: Música — Artista");
        assert!(media.playing && media.can_play_pause && media.can_next);
        assert!(!media.can_previous);
        a.snap.state = "Paused".into();
        assert!(!a.tray_media().unwrap().can_play_pause);
    }
}
