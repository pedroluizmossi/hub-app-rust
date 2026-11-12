//! Controles de mídia do sistema no Windows (`GlobalSystemMediaTransportControls`): as
//! mesmas sessões que aparecem no painel de volume — Spotify, navegadores, players.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session, GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};

use super::{MediaSnapshot, friendly_app, usable};

/// Depois de falhar, espera isto antes de pedir o serviço de novo.
const RETRY: Duration = Duration::from_secs(10);
/// O Windows conta o tempo em passos de 100 ns.
const TICKS_PER_SEC: i64 = 10_000_000;
/// Segundos entre 1601 (início do relógio do Windows) e 1970 (início do Unix).
const EPOCH_GAP_SECS: i64 = 11_644_473_600;

#[derive(Default)]
pub struct MediaProvider {
    manager: Option<Manager>,
    /// Player em uso (mantém a escolha entre leituras).
    player: String,
    last: Option<MediaSnapshot>,
    retry_at: Option<Instant>,
}

impl MediaProvider {
    /// Chamado a cada segundo enquanto o mini-app está ligado. `Some` quando algo mudou.
    pub fn collect(&mut self, force: bool) -> Option<MediaSnapshot> {
        let snap = self.read();
        let changed = self.last.as_ref() != Some(&snap);
        self.last = Some(snap.clone());
        (force || changed).then_some(snap)
    }

    fn read(&mut self) -> MediaSnapshot {
        if self.manager.is_none() {
            if self.retry_at.is_some_and(|t| Instant::now() < t) {
                return self.fail("serviço de mídia indisponível");
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
        let manager = self.manager.clone().expect("serviço recém-aberto");
        match self.poll(&manager) {
            Ok(snap) => snap,
            Err(e) => {
                self.manager = None;
                self.retry_at = Some(Instant::now() + RETRY);
                self.fail(&e.message())
            }
        }
    }

    fn fail(&mut self, motivo: &str) -> MediaSnapshot {
        self.player.clear();
        MediaSnapshot { ok: false, error: motivo.lines().next().unwrap_or_default().to_string(), ..Default::default() }
    }

    fn poll(&mut self, manager: &Manager) -> windows::core::Result<MediaSnapshot> {
        // Vários apps podem ter sessão aberta. Fica o que toca e que sabe duração/posição;
        // empate mantém o atual.
        let mut best: Option<((u8, u8, u8, u8), MediaSnapshot)> = None;
        for session in manager.GetSessions()? {
            let Ok(snap) = snapshot(&session) else { continue };
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

/// O gerenciador de sessões. A chamada espera a resposta: nunca na thread da UI.
fn manager() -> windows::core::Result<Manager> {
    Manager::RequestAsync()?.join()
}

/// Agora, no relógio do Windows (passos de 100 ns desde 1601).
fn now_ticks() -> i64 {
    let unix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    (unix.as_secs() as i64 + EPOCH_GAP_SECS) * TICKS_PER_SEC + unix.subsec_nanos() as i64 / 100
}

/// Início, fim e posição atual da faixa, em passos de 100 ns.
///
/// Os apps só informam a posição de vez em quando: tocando, soma o tempo que passou desde
/// a última informação.
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
    // título e artista podem faltar (app que só abriu a sessão): ainda assim é um player
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
    })
}

/// Executa um comando no player. Verbos: play-pause, next, prev, back, fwd, raise.
/// `seek_us` é o pulo dos botões de avançar/voltar, em microssegundos.
pub fn send(service: &str, verb: &str, seek_us: i64) -> anyhow::Result<()> {
    if !valid_service(service) {
        anyhow::bail!("player inválido");
    }
    let mut sessions = manager()?.GetSessions()?.into_iter();
    let session = sessions
        .find(|s| s.SourceAppUserModelId().is_ok_and(|id| id == service))
        .ok_or_else(|| anyhow::anyhow!("o player fechou"))?;
    let accepted = match verb {
        "play-pause" => session.TryTogglePlayPauseAsync()?.join()?,
        "next" => session.TrySkipNextAsync()?.join()?,
        "prev" => session.TrySkipPreviousAsync()?.join()?,
        "back" | "fwd" => {
            let playing = session.GetPlaybackInfo()?.PlaybackStatus()? == Status::Playing;
            let (start, end, position) = timeline(&session, playing)?;
            // microssegundos -> passos de 100 ns
            let step = seek_us.abs().saturating_mul(10);
            let target = if verb == "back" { position - step } else { position + step };
            let target = if end > start { target.clamp(start, end) } else { target.max(0) };
            session.TryChangePlaybackPositionAsync(target)?.join()?
        }
        // o Windows não tem como trazer a janela de outro app para a frente por aqui
        _ => true,
    };
    anyhow::ensure!(accepted, "o player recusou o comando");
    Ok(())
}

/// Identificador de sessão que veio do sistema: qualquer texto não vazio e curto.
pub fn valid_service(service: &str) -> bool {
    !service.is_empty() && service.len() <= 255
}
