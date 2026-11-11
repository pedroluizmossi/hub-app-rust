//! MPRIS pelo D-Bus: quem está tocando, o quê, e os comandos do player.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedValue;

use super::{MediaSnapshot, usable};

/// Prefixo dos serviços que falam MPRIS.
const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT: &str = "org.mpris.MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const PROPS: &str = "org.freedesktop.DBus.Properties";
/// Depois de falhar, espera isto antes de tentar o barramento de novo.
const RETRY: Duration = Duration::from_secs(10);
/// Prazo de cada chamada D-Bus. O zbus não tem prazo por padrão: um player travado
/// (navegador congelado, processo parado) prenderia a thread do provedor para sempre.
const CALL_TIMEOUT: Duration = Duration::from_secs(2);

/// Conexão com o barramento da sessão, com prazo nas chamadas.
fn connect() -> zbus::Result<Connection> {
    zbus::blocking::connection::Builder::session()?.method_timeout(CALL_TIMEOUT).build()
}

#[derive(Default)]
pub struct MediaProvider {
    conn: Option<Connection>,
    /// Player em uso (mantém a escolha entre leituras).
    player: String,
    last: Option<MediaSnapshot>,
    retry_at: Option<Instant>,
}


impl MediaProvider {
    /// Chamado a cada segundo enquanto o mini-app está ligado. `Some` quando algo mudou.
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
                return self.fail("barramento indisponível");
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
        let conn = self.conn.as_ref().expect("conexão recém-aberta").clone();
        match self.poll(&conn) {
            Ok(snap) => snap,
            Err(e) => {
                // conexão caiu (o D-Bus reiniciou, o player fechou no meio da leitura)
                self.conn = None;
                self.retry_at = Some(Instant::now() + RETRY);
                self.fail(&e.to_string())
            }
        }
    }

    fn fail(&mut self, motivo: &str) -> MediaSnapshot {
        self.player.clear();
        MediaSnapshot { ok: false, error: first_line(motivo), ..Default::default() }
    }

    fn poll(&mut self, conn: &Connection) -> anyhow::Result<MediaSnapshot> {
        let names = players(conn)?;
        if names.is_empty() {
            self.player.clear();
            return Ok(MediaSnapshot { ok: true, ..Default::default() });
        }

        // Dois players podem descrever a mesma música (Firefox e o do navegador, por
        // exemplo). Fica o que toca e que sabe duração/posição; empate mantém o atual.
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

/// Serviços MPRIS no barramento, em ordem estável.
fn players(conn: &Connection) -> anyhow::Result<Vec<String>> {
    let bus = Proxy::new(conn, "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus")?;
    let names: Vec<String> = bus.call("ListNames", &())?;
    let mut out: Vec<String> = names.into_iter().filter(|n| n.starts_with(PREFIX)).collect();
    out.sort();
    Ok(out)
}

/// Todas as propriedades de uma interface de um player.
fn props(conn: &Connection, service: &str, iface: &str) -> anyhow::Result<HashMap<String, OwnedValue>> {
    let p = Proxy::new(conn, service, PATH, PROPS)?;
    Ok(p.call("GetAll", &(iface,))?)
}

/// Posição atual em microssegundos (só existe no player, e não vem no `GetAll`).
fn position(conn: &Connection, service: &str) -> anyhow::Result<i64> {
    let p = Proxy::new(conn, service, PATH, PLAYER)?;
    Ok(p.get_property::<i64>("Position")?)
}

/// Propriedades -> snapshot. Separado do D-Bus para poder ser testado.
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

/// Executa um comando no player. Verbos: play-pause, next, prev, back, fwd, raise.
/// `seek_us` é o pulo dos botões de avançar/voltar.
pub fn send(service: &str, verb: &str, seek_us: i64) -> anyhow::Result<()> {
    if !valid_service(service) {
        anyhow::bail!("player inválido");
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

/// Nome de serviço que veio do barramento: só aceita o formato do MPRIS.
pub fn valid_service(service: &str) -> bool {
    service.starts_with(PREFIX)
        && service.len() <= 255
        && service.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn text(v: Option<&OwnedValue>) -> String {
    v.and_then(|v| String::try_from(v.clone()).ok()).unwrap_or_default()
}

/// Lista de nomes (`as`, como manda a especificação) ou um texto só, que alguns players
/// mandam no lugar.
fn names(v: Option<&OwnedValue>) -> String {
    let Some(v) = v else { return String::new() };
    match Vec::<String>::try_from(v.clone()) {
        Ok(list) => list.join(", "),
        Err(_) => text(Some(v)),
    }
}

/// Duração em microssegundos: `x` (i64) pela especificação, mas há players que mandam `t`
/// (u64) ou `i` (i32).
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

/// Microssegundos -> segundos (negativo vira zero).
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
    fn le_metadados_e_limites() {
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
    fn player_sem_metadados_nao_quebra() {
        let root = HashMap::new();
        let s = parse("org.mpris.MediaPlayer2.x", &root, &HashMap::new(), None);
        assert!(s.ok);
        assert!(!s.playing());
        assert_eq!(s.title, "");
        assert_eq!(s.length, 0);
        assert_eq!(s.summary(), "CD Player: org.mpris.MediaPlayer2.x aberto");
    }

    #[test]
    fn snapshot_inalterado_e_republicado_quando_solicitado() {
        let mut provider = MediaProvider::default();
        let snap = MediaSnapshot { ok: true, state: "Paused".into(), ..Default::default() };
        assert_eq!(provider.publish(snap.clone(), false), Some(snap.clone()));
        assert_eq!(provider.publish(snap.clone(), false), None);
        assert_eq!(provider.publish(snap.clone(), true), Some(snap));
    }

    #[test]
    fn posicao_pausada_e_mantida_no_snapshot() {
        let s = parse("org.mpris.MediaPlayer2.x", &HashMap::new(), &player_props("Paused"), Some(42_000_000));
        assert!(s.paused());
        assert_eq!(s.position, 42);
    }

    #[test]
    fn posicao_negativa_e_zero_e_tempo() {
        assert_eq!(seconds(-5), 0);
        assert_eq!(seconds(1_999_999), 1);
        assert_eq!(mmss(0), "0:00");
        assert_eq!(mmss(7), "0:07");
        assert_eq!(mmss(187), "3:07");
        assert_eq!(mmss(3723), "1:02:03");
    }

    #[test]
    fn escolhe_o_player_mais_util() {
        let base = MediaSnapshot { ok: true, title: "x".into(), ..Default::default() };
        let tocando_sem_duracao = MediaSnapshot { player: "a".into(), state: "Playing".into(), ..base.clone() };
        let pausado_com_duracao = MediaSnapshot { player: "b".into(), state: "Paused".into(), length: 100, ..base.clone() };
        let tocando_com_duracao =
            MediaSnapshot { player: "b".into(), state: "Playing".into(), length: 100, ..base.clone() };
        // tocar vale mais que saber a duração; entre dois que tocam, vence o que sabe duração
        assert!(usable(&tocando_sem_duracao, "a") > usable(&pausado_com_duracao, "a"));
        assert!(usable(&tocando_com_duracao, "a") > usable(&tocando_sem_duracao, "a"));
        // empate total: fica o que já estava em uso
        assert!(usable(&tocando_com_duracao, "b") > usable(&tocando_com_duracao, "a"));
    }

    #[test]
    fn so_aceita_servico_do_mpris() {
        assert!(valid_service("org.mpris.MediaPlayer2.firefox.instance_1_1202"));
        assert!(valid_service("org.mpris.MediaPlayer2.plasma-browser-integration"));
        assert!(!valid_service("org.mpris.MediaPlayer2.x; rm -rf /"));
        assert!(!valid_service("org.example.Player"));
        assert!(!valid_service(""));
    }

    #[test]
    fn aceita_duracao_sem_sinal_e_artista_como_texto() {
        let player = map(&[(
            "Metadata",
            metadata(&[("xesam:artist", Value::from("Tonico")), ("mpris:length", Value::from(187_000_000u64))]),
        )]);
        let snap = parse("org.mpris.MediaPlayer2.x", &HashMap::new(), &player, None);
        assert_eq!(snap.artist, "Tonico");
        assert_eq!(snap.length, 187);
    }

}
