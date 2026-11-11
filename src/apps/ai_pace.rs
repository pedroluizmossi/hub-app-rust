//! Previsão de quando um limite de uso acaba, pelo ritmo das últimas leituras.
//!
//! O % usado é um contador: só sobe até a janela renovar, e não tem ruído de medição. O
//! ritmo num intervalo é a diferença entre as pontas dividida pelo tempo. Como o uso vem em
//! rajadas, o ritmo é medido em dois intervalos (o "burn rate" de duas janelas): o longo dá
//! a previsão mostrada, que não pula a cada leitura; o curto diz se o gasto continua agora.
//! Só os dois juntos agitam a mascote: rajada isolada ou uso que já parou não contam.
use std::collections::VecDeque;

/// Intervalo curto: "está gastando agora?".
const SHORT: i64 = 20 * 60;
/// Intervalo longo: "é um padrão ou foi uma rajada?". Também é quanto histórico se guarda.
const LONG: i64 = 60 * 60;
/// Histórico mínimo para cada ritmo valer (abaixo disso, duas leituras viram alarme falso).
const SHORT_MIN: i64 = 5 * 60;
const LONG_MIN: i64 = 15 * 60;
/// Queda no % usado (em pontos) que indica renovação da janela.
const DROP: f64 = 1.0;
/// Mudança no horário de renovação (s) que indica outra janela.
const RESET_SHIFT: i64 = 10 * 60;

/// Menor variação (em pontos) que ocupa a altura toda do gráfico: abaixo disso a linha
/// fica proporcionalmente mais baixa, para meio ponto não parecer um salto.
const SPARK_RANGE: f64 = 5.0;

/// O limite acaba antes de renovar, se o ritmo continuar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Forecast {
    /// Quando chega a 100% (segundos desde a época), pelo ritmo longo.
    pub ends_at: i64,
    /// O ritmo curto também aponta esgotamento antes da renovação.
    pub strained: bool,
}

/// Leituras recentes de uma janela de uso: (hora, % usado), da mais antiga à mais nova.
#[derive(Default)]
pub struct History {
    samples: VecDeque<(i64, f64)>,
    resets_at: i64,
}

impl History {
    /// Guarda uma leitura feita em `at`. Recomeça do zero quando a janela renovou.
    pub fn record(&mut self, at: i64, used: f64, resets_at: i64) {
        let renewed = self.samples.back().is_some_and(|&(_, last)| used < last - DROP)
            || (self.resets_at - resets_at).abs() > RESET_SHIFT;
        if renewed { self.samples.clear(); }
        self.resets_at = resets_at;
        if self.samples.back().is_some_and(|&(t, _)| at <= t) { return; }
        self.samples.push_back((at, used));
        while self.samples.front().is_some_and(|&(t, _)| at - t > LONG) { self.samples.pop_front(); }
    }

    /// Gráfico da última hora como caminho SVG num quadro 1000×100: a leitura mais nova
    /// fica na borda direita e a altura acompanha o quanto o uso subiu. Vazio com menos
    /// de duas leituras.
    pub fn spark(&self) -> String {
        let (Some(&(end, _)), true) = (self.samples.back(), self.samples.len() >= 2) else { return String::new() };
        let low = self.samples.iter().map(|&(_, u)| u).fold(f64::INFINITY, f64::min);
        let high = self.samples.iter().map(|&(_, u)| u).fold(low, f64::max);
        let range = (high - low).max(SPARK_RANGE);
        self.samples.iter().enumerate().map(|(i, &(t, u))| {
            let x = 1000.0 - (end - t) as f64 * 1000.0 / LONG as f64;
            let y = 98.0 - (u - low) / range * 96.0;
            format!("{} {x:.1} {y:.1}", if i == 0 { "M" } else { "L" })
        }).collect::<Vec<_>>().join(" ")
    }

    /// Ritmo (pontos por segundo) desde a leitura mais antiga dentro de `span`. Se as
    /// leituras são mais espaçadas que `span`, vale a anterior à última.
    fn rate(&self, span: i64, min: i64) -> Option<f64> {
        let &(now, used) = self.samples.back()?;
        let older = self.samples.len() - 1;
        let &(t, u) = self.samples.iter().take(older).find(|&&(t, _)| now - t <= span)
            .or_else(|| older.checked_sub(1).and_then(|i| self.samples.get(i)))?;
        (now - t >= min).then(|| (used - u) / (now - t) as f64)
    }

    /// Previsão vista em `now`. `None` se, nesse ritmo, a janela renova antes de acabar
    /// (ou se ainda não há histórico para dizer).
    pub fn forecast(&self, now: i64) -> Option<Forecast> {
        let &(at, used) = self.samples.back()?;
        let ends = |rate: f64| (rate > 0.0 && used < 100.0)
            .then(|| at.saturating_add(((100.0 - used) / rate) as i64));
        let ends_at = self.rate(LONG, LONG_MIN).and_then(ends).filter(|t| *t > now && *t < self.resets_at)?;
        let strained = self.rate(SHORT, SHORT_MIN).and_then(ends).is_some_and(|t| t < self.resets_at);
        Some(Forecast { ends_at, strained })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60;

    /// Histórico com uma leitura a cada `step` minutos; `used(minuto)` dá o % em cada uma.
    fn history(minutes: i64, step: i64, resets_at: i64, used: impl Fn(i64) -> f64) -> History {
        let mut h = History::default();
        for m in (0..=minutes).step_by(step as usize) { h.record(m * MIN, used(m), resets_at); }
        h
    }

    #[test]
    fn ritmo_constante_acaba_antes_de_renovar() {
        // 40% → 70% em 60 min (0,5 ponto/min): faltam 30 pontos, mais 60 min.
        let h = history(60, 3, 180 * MIN, |m| 40.0 + m as f64 * 0.5);
        assert_eq!(h.forecast(60 * MIN), Some(Forecast { ends_at: 120 * MIN, strained: true }));
    }

    #[test]
    fn renova_antes_de_acabar_nao_preve() {
        let h = history(60, 3, 90 * MIN, |m| 40.0 + m as f64 * 0.5);
        assert_eq!(h.forecast(60 * MIN), None);
    }

    #[test]
    fn uso_que_parou_preve_mas_nao_agita() {
        // Subiu 30 pontos em 30 min e parou: o ritmo longo ainda aponta, o curto é zero.
        let h = history(60, 3, 180 * MIN, |m| 40.0 + m.min(30) as f64);
        let f = h.forecast(60 * MIN).unwrap();
        assert!(!f.strained);
        assert_eq!(f.ends_at, 120 * MIN);
    }

    #[test]
    fn rajada_isolada_nao_preve() {
        // Parado 45 min e depois 6 pontos em 15 min: o ritmo curto esgotaria em ~2 h, mas
        // na hora inteira foram só 6 pontos (o longo levaria mais de 8 h; renova em 5 h).
        let h = history(60, 3, 360 * MIN, |m| 50.0 + (m - 45).max(0) as f64 * 0.4);
        assert_eq!(h.forecast(60 * MIN), None);
    }

    #[test]
    fn pouco_historico_nao_preve() {
        let h = history(12, 3, 180 * MIN, |m| 40.0 + m as f64 * 2.0);
        assert_eq!(h.forecast(12 * MIN), None);
    }

    #[test]
    fn renovacao_zera_o_historico() {
        let mut h = history(60, 3, 180 * MIN, |m| 40.0 + m as f64 * 0.5);
        h.record(63 * MIN, 2.0, 480 * MIN);
        assert_eq!(h.forecast(63 * MIN), None);
        assert_eq!(h.samples.len(), 1);
    }

    #[test]
    fn leitura_repetida_nao_conta_duas_vezes() {
        let mut h = history(60, 3, 180 * MIN, |m| 40.0 + m as f64 * 0.5);
        let n = h.samples.len();
        h.record(60 * MIN, 70.0, 180 * MIN);
        assert_eq!(h.samples.len(), n);
    }

    #[test]
    fn leituras_espacadas_usam_o_ultimo_intervalo() {
        // Uma leitura a cada 30 min: não há leitura dentro do intervalo curto.
        let h = history(60, 30, 180 * MIN, |m| 40.0 + m as f64 * 0.5);
        assert_eq!(h.forecast(60 * MIN), Some(Forecast { ends_at: 120 * MIN, strained: true }));
    }

    #[test]
    fn grafico_da_ultima_hora() {
        // 30 min de leituras: ocupa a metade direita; 10 pontos de subida, a altura toda.
        let h = history(30, 15, 0, |m| 40.0 + m as f64 / 3.0);
        assert_eq!(h.spark(), "M 500.0 98.0 L 750.0 50.0 L 1000.0 2.0");
        // 1 ponto de subida: um quinto da altura
        let h = history(60, 60, 0, |m| 40.0 + m as f64 / 60.0);
        assert_eq!(h.spark(), "M 0.0 98.0 L 1000.0 78.8");
        assert_eq!(history(0, 3, 0, |_| 40.0).spark(), "");
    }

    #[test]
    fn previsao_vencida_some() {
        let h = history(60, 3, 180 * MIN, |m| 40.0 + m as f64 * 0.5);
        assert_eq!(h.forecast(121 * MIN), None);
    }
}
