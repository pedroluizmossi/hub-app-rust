//! Limites das assinaturas de IA por consultas diretas aos serviços.
//!
//! As consultas não imprimem credenciais nem o conteúdo das respostas. Cada fonte falha
//! independentemente; a última leitura válida é mantida e marcada como desatualizada.
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[path = "ai_usage_direct.rs"]
mod direct;

/// Intervalo entre leituras (s), vindo da config do app ("Atualizar a cada").
pub static POLL_SECS: AtomicU64 = AtomicU64::new(180);
/// Fontes ligadas na config (bit `i` = fonte `i`, na ordem de `SOURCES`). Desligada não é consultada.
pub static ENABLED: AtomicU8 = AtomicU8::new(0b1111);
/// Nomes das fontes, na ordem dos cartões.
pub const SOURCES: [&str; 4] = ["Codex", "Claude", "Kiro", "OpenCode Go"];
/// O Kiro é consultado com menos frequência (nunca menos que isto).
const KIRO_MIN_POLL: Duration = Duration::from_secs(900);
const RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct UsageWindow {
    pub label: String,
    pub used: f64,
    pub detail: String,
    pub resets_at: Option<i64>,
    pub reset_date: String,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct UsageCard {
    pub name: String,
    pub status: String,
    pub plan: String,
    pub windows: Vec<UsageWindow>,
    pub updated_at: Option<i64>,
    pub stale: bool,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct UsageSnapshot {
    pub cards: Vec<UsageCard>,
}

impl Default for UsageSnapshot {
    fn default() -> Self {
        Self { cards: SOURCES.into_iter()
            .map(|name| UsageCard { name: name.into(), status: "Aguardando leitura".into(), ..Default::default() }).collect() }
    }
}

#[derive(Default)]
pub struct UsageProvider {
    cards: Vec<UsageCard>,
    last: [Option<Instant>; 4],
    retry_at: [Option<Instant>; 4],
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

impl UsageProvider {
    pub fn collect(&mut self, force: bool) -> UsageSnapshot {
        if self.cards.is_empty() { self.cards = UsageSnapshot::default().cards; }
        let poll = Duration::from_secs(POLL_SECS.load(Ordering::Relaxed).max(60));
        let enabled = ENABLED.load(Ordering::Relaxed);
        let elapsed = |i: usize, interval: Duration| enabled & (1 << i) != 0
            && self.retry_at[i].is_none_or(|t| Instant::now() >= t)
            && (force || self.last[i].is_none_or(|t| t.elapsed() >= interval));
        let should_read = [elapsed(0, poll), elapsed(1, poll), elapsed(2, poll.max(KIRO_MIN_POLL)), elapsed(3, poll)];
        let (codex, claude, kiro, opencode) = std::thread::scope(|scope| {
            let c = should_read[0].then(|| scope.spawn(direct::codex));
            let a = should_read[1].then(|| scope.spawn(direct::claude));
            let k = should_read[2].then(|| scope.spawn(direct::kiro));
            let o = should_read[3].then(|| scope.spawn(direct::opencode));
            (c.map(|t| t.join().unwrap_or_else(|_| Err("Falha na leitura".into()))),
             a.map(|t| t.join().unwrap_or_else(|_| Err("Falha na leitura".into()))),
             k.map(|t| t.join().unwrap_or_else(|_| Err("Falha na leitura".into()))),
             o.map(|t| t.join().unwrap_or_else(|_| Err("Falha na leitura".into()))))
        });
        for (idx, result) in [(0, codex), (1, claude), (2, kiro), (3, opencode)] {
            if let Some(result) = result {
                self.last[idx] = Some(Instant::now());
                match result {
                    Ok(mut card) => { card.updated_at = Some(now()); self.cards[idx] = card; }
                    Err(status) => {
                        if status.contains("Limite de consultas") { self.retry_at[idx] = Some(Instant::now() + RATE_LIMIT_BACKOFF); }
                        let card = &mut self.cards[idx];
                        // Falha de autenticação pode indicar troca de conta: não exibir
                        // como sendo da nova conta os limites da sessão anterior.
                        if status.contains("Autenticação") || status.contains("Login") || status.contains("login") {
                            card.windows.clear();
                            card.updated_at = None;
                        }
                        card.status = status;
                        card.stale = card.updated_at.is_some();
                    }
                }
            }
        }
        UsageSnapshot { cards: self.cards.clone() }
    }
}
