//! Conversa entre mascotes: o que os apps contam e quem pode falar.
//!
//! Um app não fala com outro. Ele só registra fatos sobre si ("a CPU esquentou", "o
//! postgres caiu", "me cutucaram") e diz quais mascotes tem na tela. Quem transforma isso
//! em diálogo é o diretor do hub (`src/hub/chatter.rs`).
//!
//! As falas vêm de arquivos TOML: cada app pode ter o seu (`miniapps/<app>/lines.toml`,
//! ver `MiniApp::lines`) e o hub tem as genéricas (`src/hub/lines.toml`), usadas quando o
//! app não declarou aquele caso. Formato:
//!
//! ```toml
//! [trouble]               # o que a mascote deste app diz ao contar um alarme
//! pt = ["{s} caiu! (o_o)"]
//! en = ["{s} went down! (o_o)"]
//! de = ["{s} ist abgestürzt! (o_o)"]   # opcional
//!
//! [trouble.answer]        # o que as outras respondem a esse alarme (opcional)
//! pt = ["{s} foi tirar um cochilo? zzz"]
//! en = ["did {s} take a nap? zzz"]
//!
//! [reply.strain]          # o que a mascote deste app responde quando outra fica agitada
//! about = ["CPU", "RAM"]  # só para estes assuntos (vazio = qualquer um)
//! pt = ["foi mal... acho que fui eu u.u"]
//! en = ["sorry... i think that was me u.u"]
//! ```
//!
//! Casos: `trouble`, `strain`, `recovered`, `poked` e `idle` (papo à toa). Lacunas:
//! `{a}` quem conta, `{b}` quem responde, `{s}` o assunto (vazio vira o nome de quem conta),
//! `{v}` o número do fato. Variante com `{v}` só sai quando o fato tem número, e com `{b}`
//! só quando alguém responde. Tom fofo, com algumas falas bravas (de palavrão); kaomoji em
//! ASCII (a fonte pode não ter emoji); até ~60 caracteres (duas linhas da caixa de diálogo).

use serde::Deserialize;

use crate::mascot::Mood;
use crate::mascot::sprites::Sprite;
use crate::mascot::themes::Theme;

/// Uma mascote que pode falar: nome mostrado na caixa de diálogo e como desenhá-la.
#[derive(Clone)]
pub struct Voice {
    /// Id do app onde a mascote aparece (a fala sai na caixa desse app).
    pub app: String,
    /// "Docker", "CPU", "Claude".
    pub name: String,
    pub sprite: &'static Sprite,
    pub theme: &'static Theme,
    pub mood: Mood,
}

impl Voice {
    /// Id da mascote na tela ("cyber.vitals:CPU"): o Rust passa à view, que o põe no
    /// `Mascot` para a caixa de diálogo achar quem fala e quem ouve.
    pub fn id(&self) -> String {
        Voice::id_of(&self.app, &self.name)
    }

    pub fn id_of(app: &str, name: &str) -> String {
        format!("{app}:{name}")
    }

    /// Mesma mascote (o diretor não põe uma mascote para responder a si mesma).
    pub fn same(&self, other: &Voice) -> bool {
        self.app == other.app && self.name == other.name
    }
}

/// O que aconteceu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FactKind {
    /// Entrou em alarme (container caiu, limite estourou, temperatura crítica).
    Trouble,
    /// Ficou agitado (uso alto, esquentando).
    Strain,
    /// Ainda não passou do limite, mas vai passar se o ritmo continuar. O número, se
    /// houver, são os minutos que faltam.
    Forecast,
    /// Voltou ao normal depois de agitado ou em alarme.
    Recovered,
    /// O usuário cutucou a mascote.
    Poked,
    /// Papo à toa: tudo calmo há um tempo (quem puxa é o diretor, não um app).
    Idle,
}

impl FactKind {
    pub const ALL: [FactKind; 6] =
        [FactKind::Trouble, FactKind::Strain, FactKind::Forecast, FactKind::Recovered, FactKind::Poked, FactKind::Idle];

    /// Nome da seção no `lines.toml`.
    pub fn key(self) -> &'static str {
        match self {
            FactKind::Trouble => "trouble",
            FactKind::Strain => "strain",
            FactKind::Forecast => "forecast",
            FactKind::Recovered => "recovered",
            FactKind::Poked => "poked",
            FactKind::Idle => "idle",
        }
    }
}

/// Um fato contado por um app.
#[derive(Clone)]
pub struct Fact {
    pub kind: FactKind,
    /// Quem conta (a mascote do assunto).
    pub voice: Voice,
    /// Do que se trata: "CPU", "postgres", "Claude". Vazio = a própria mascote.
    pub subject: String,
    /// Número para a frase ("92" em "CPU em 92%"), quando houver.
    pub value: Option<f32>,
}

/// Fato de mudança de humor: piorou para alarme ou agitado, ou voltou ao normal.
/// `None` quando a mudança não merece comentário (ex.: dormiu, acordou).
pub fn mood_fact(before: Mood, now: Mood, voice: Voice, subject: &str, value: Option<f32>) -> Option<Fact> {
    let bad = |m: Mood| m >= Mood::Busy;
    let kind = if now == Mood::Alarm && before != Mood::Alarm {
        FactKind::Trouble
    } else if now == Mood::Busy && !bad(before) {
        FactKind::Strain
    } else if bad(before) && !bad(now) {
        FactKind::Recovered
    } else {
        return None;
    };
    Some(Fact { kind, voice, subject: subject.to_string(), value })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Pt,
    En,
    De,
}

impl Lang {
    pub const ALL: [Lang; 3] = [Lang::Pt, Lang::En, Lang::De];
}

/// Variantes de uma fala nos três idiomas (alemão opcional).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Say {
    #[serde(default)]
    pub pt: Vec<String>,
    #[serde(default)]
    pub en: Vec<String>,
    #[serde(default)]
    pub de: Vec<String>,
    /// Só em `reply.*`: assuntos a que esta resposta se aplica (vazio = qualquer um).
    #[serde(default)]
    pub about: Vec<String>,
}

impl Say {
    pub fn variants(&self, lang: Lang) -> &[String] {
        match lang {
            Lang::Pt => &self.pt,
            Lang::En => &self.en,
            Lang::De => &self.de,
        }
    }

    /// Serve para o assunto `subject` (ver `about`).
    pub fn fits(&self, subject: &str) -> bool {
        self.about.is_empty() || self.about.iter().any(|a| a == subject)
    }
}

/// Uma seção de caso: o que a mascote do app diz e, opcionalmente, o que os outros respondem.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    #[serde(default)]
    pub pt: Vec<String>,
    #[serde(default)]
    pub en: Vec<String>,
    #[serde(default)]
    pub de: Vec<String>,
    pub answer: Option<Say>,
}

impl Entry {
    pub fn say(&self) -> Say {
        Say { pt: self.pt.clone(), en: self.en.clone(), de: self.de.clone(), about: Vec::new() }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Replies {
    pub trouble: Option<Say>,
    pub strain: Option<Say>,
    pub forecast: Option<Say>,
    pub recovered: Option<Say>,
    pub poked: Option<Say>,
    pub idle: Option<Say>,
}

/// As falas de um app (ou as genéricas do hub).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lines {
    pub trouble: Option<Entry>,
    pub strain: Option<Entry>,
    pub forecast: Option<Entry>,
    pub recovered: Option<Entry>,
    pub poked: Option<Entry>,
    pub idle: Option<Entry>,
    #[serde(default)]
    pub reply: Replies,
}

impl Lines {
    /// Lê um `lines.toml`. Seção ou campo desconhecido é erro (pega erro de digitação).
    pub fn parse(src: &str) -> Result<Lines, String> {
        if src.trim().is_empty() {
            return Ok(Lines::default());
        }
        toml::from_str(src).map_err(|e| e.to_string())
    }

    /// O que a mascote deste app diz ao contar um fato do tipo `kind`.
    pub fn own(&self, kind: FactKind) -> Option<&Entry> {
        match kind {
            FactKind::Trouble => self.trouble.as_ref(),
            FactKind::Strain => self.strain.as_ref(),
            FactKind::Forecast => self.forecast.as_ref(),
            FactKind::Recovered => self.recovered.as_ref(),
            FactKind::Poked => self.poked.as_ref(),
            FactKind::Idle => self.idle.as_ref(),
        }
    }

    /// O que a mascote deste app responde quando outra conta um fato do tipo `kind`.
    pub fn reply(&self, kind: FactKind) -> Option<&Say> {
        let r = &self.reply;
        match kind {
            FactKind::Trouble => r.trouble.as_ref(),
            FactKind::Strain => r.strain.as_ref(),
            FactKind::Forecast => r.forecast.as_ref(),
            FactKind::Recovered => r.recovered.as_ref(),
            FactKind::Poked => r.poked.as_ref(),
            FactKind::Idle => r.idle.as_ref(),
        }
    }

    /// Todas as falas, para conferir (nome da seção, fala).
    pub fn all(&self) -> Vec<(String, Say)> {
        let mut out = Vec::new();
        for k in FactKind::ALL {
            if let Some(e) = self.own(k) {
                out.push((k.key().to_string(), e.say()));
                if let Some(a) = &e.answer {
                    out.push((format!("{}.answer", k.key()), a.clone()));
                }
            }
            if let Some(r) = self.reply(k) {
                out.push((format!("reply.{}", k.key()), r.clone()));
            }
        }
        out
    }

    /// Problemas de um `lines.toml`: falta português ou inglês, lacuna desconhecida, fala
    /// longa demais. Vazio = tudo certo (usado pelos testes de cada app).
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (section, say) in self.all() {
            if say.pt.is_empty() || say.en.is_empty() {
                out.push(format!("[{section}] precisa de pt e en"));
            }
            for lang in Lang::ALL {
                for t in say.variants(lang) {
                    let rest = t.replace("{a}", "").replace("{b}", "").replace("{s}", "").replace("{v}", "");
                    if rest.contains('{') || rest.contains('}') {
                        out.push(format!("[{section}] lacuna desconhecida em {t:?}"));
                    }
                    if t.chars().count() > 60 {
                        out.push(format!("[{section}] fala longa demais: {t:?}"));
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_secoes_respostas_e_assuntos() {
        let l = Lines::parse(
            r#"
            [trouble]
            pt = ["{s} caiu!"]
            en = ["{s} went down!"]
            [trouble.answer]
            pt = ["cochilo?"]
            en = ["nap?"]
            [reply.strain]
            about = ["CPU"]
            pt = ["fui eu"]
            en = ["it was me"]
            "#,
        )
        .unwrap();
        assert_eq!(l.own(FactKind::Trouble).unwrap().pt, ["{s} caiu!"]);
        assert!(l.own(FactKind::Trouble).unwrap().answer.is_some());
        assert!(l.reply(FactKind::Strain).unwrap().fits("CPU"));
        assert!(!l.reply(FactKind::Strain).unwrap().fits("DISCO"));
        assert!(l.problems().is_empty());
    }

    #[test]
    fn erro_de_digitacao_nao_passa() {
        assert!(Lines::parse("[truble]\npt = [\"x\"]").is_err());
        assert!(Lines::parse("[trouble]\npt = [\"x\"]\nes = [\"y\"]").is_err());
        let falta_en = Lines::parse("[poked]\npt = [\"oi\"]").unwrap();
        assert_eq!(falta_en.problems().len(), 1);
    }
}
