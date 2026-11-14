//! Mascot chatter: what the apps report and who may speak.
//!
//! An app does not talk to another. It only records facts about itself ("the CPU got
//! hot", "postgres went down", "someone poked me") and says which mascots it has on
//! screen. The hub's director (`src/hub/chatter.rs`) turns that into dialogue.
//!
//! The lines come from TOML files: each app may have its own (`miniapps/<app>/lines.toml`,
//! see `MiniApp::lines`) and the hub has the generic ones (`src/hub/lines.toml`), used when
//! the app did not declare that case. Format:
//!
//! ```toml
//! [trouble]               # what this app's mascot says when reporting an alarm
//! pt = ["{s} caiu! (o_o)"]
//! en = ["{s} went down! (o_o)"]
//! de = ["{s} ist abgestürzt! (o_o)"]   # optional
//!
//! [trouble.answer]        # what the others answer to that alarm (optional)
//! pt = ["{s} foi tirar um cochilo? zzz"]
//! en = ["did {s} take a nap? zzz"]
//!
//! [reply.strain]          # what this app's mascot answers when another gets agitated
//! about = ["CPU", "RAM"]  # only for these subjects (empty = any)
//! pt = ["foi mal... acho que fui eu u.u"]
//! en = ["sorry... i think that was me u.u"]
//! ```
//!
//! Cases: `trouble`, `strain`, `recovered`, `poked` and `idle` (small talk). Blanks: `{a}`
//! who reports, `{b}` who answers, `{s}` the subject (empty becomes the reporter's name),
//! `{v}` the fact's number. A variant with `{v}` is only used when the fact has a number,
//! and one with `{b}` only when someone answers. Cute tone, with a few grumpy (swearing)
//! lines; ASCII kaomoji (the font may lack emoji); up to ~60 characters (two lines of the
//! speech bubble).

use serde::Deserialize;

use crate::mascot::Mood;
use crate::mascot::sprites::Sprite;
use crate::mascot::themes::Theme;

/// A mascot that may speak: name shown in the speech bubble and how to draw it.
#[derive(Clone)]
pub struct Voice {
    /// Id of the app where the mascot appears (the line shows in that app's box).
    pub app: String,
    /// "Docker", "CPU", "Claude".
    pub name: String,
    pub sprite: &'static Sprite,
    pub theme: &'static Theme,
    pub mood: Mood,
}

impl Voice {
    /// Id of the mascot on screen ("cyber.vitals:CPU"): Rust passes it to the view, which
    /// sets it on the `Mascot` so the speech bubble finds who speaks and who listens.
    pub fn id(&self) -> String {
        Voice::id_of(&self.app, &self.name)
    }

    pub fn id_of(app: &str, name: &str) -> String {
        format!("{app}:{name}")
    }

    /// Same mascot (the director never has a mascot answer itself).
    pub fn same(&self, other: &Voice) -> bool {
        self.app == other.app && self.name == other.name
    }
}

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FactKind {
    /// Went into alarm (container down, limit exceeded, critical temperature).
    Trouble,
    /// Got agitated (high usage, heating up).
    Strain,
    /// Not over the limit yet, but will be if the pace keeps up. The number, if any, is the
    /// minutes left.
    Forecast,
    /// Back to normal after being agitated or in alarm.
    Recovered,
    /// The user poked the mascot.
    Poked,
    /// Small talk: all calm for a while (started by the director, not by an app).
    Idle,
}

impl FactKind {
    pub const ALL: [FactKind; 6] =
        [FactKind::Trouble, FactKind::Strain, FactKind::Forecast, FactKind::Recovered, FactKind::Poked, FactKind::Idle];

    /// Name of the section in `lines.toml`.
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

/// A fact reported by an app.
#[derive(Clone)]
pub struct Fact {
    pub kind: FactKind,
    /// Who reports it (the subject's mascot).
    pub voice: Voice,
    /// What it is about: "CPU", "postgres", "Claude". Empty = the mascot itself.
    pub subject: String,
    /// Number for the sentence ("92" in "CPU at 92%"), if any.
    pub value: Option<f32>,
}

/// Fact for a mood change: worsened to alarm or agitated, or back to normal.
/// `None` when the change is not worth a comment (e.g. fell asleep, woke up).
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

/// Variants of a line in the three languages (German optional).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Say {
    #[serde(default)]
    pub pt: Vec<String>,
    #[serde(default)]
    pub en: Vec<String>,
    #[serde(default)]
    pub de: Vec<String>,
    /// Only in `reply.*`: subjects this answer applies to (empty = any).
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

    /// Fits the subject `subject` (see `about`).
    pub fn fits(&self, subject: &str) -> bool {
        self.about.is_empty() || self.about.iter().any(|a| a == subject)
    }
}

/// A case section: what the app's mascot says and, optionally, what the others answer.
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

/// The lines of an app (or the hub's generic ones).
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
    /// Reads a `lines.toml`. An unknown section or field is an error (catches typos).
    pub fn parse(src: &str) -> Result<Lines, String> {
        if src.trim().is_empty() {
            return Ok(Lines::default());
        }
        toml::from_str(src).map_err(|e| e.to_string())
    }

    /// What this app's mascot says when reporting a fact of kind `kind`.
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

    /// What this app's mascot answers when another reports a fact of kind `kind`.
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

    /// Every line, for checking (section name, line).
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

    /// Problems in a `lines.toml`: missing Portuguese or English, unknown blank, line too
    /// long. Empty = all good (used by each app's tests).
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (section, say) in self.all() {
            if say.pt.is_empty() || say.en.is_empty() {
                out.push(format!("[{section}] needs pt and en"));
            }
            for lang in Lang::ALL {
                for t in say.variants(lang) {
                    let rest = t.replace("{a}", "").replace("{b}", "").replace("{s}", "").replace("{v}", "");
                    if rest.contains('{') || rest.contains('}') {
                        out.push(format!("[{section}] unknown blank in {t:?}"));
                    }
                    if t.chars().count() > 60 {
                        out.push(format!("[{section}] line too long: {t:?}"));
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
    fn reads_sections_answers_and_subjects() {
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
    fn typos_are_rejected() {
        assert!(Lines::parse("[truble]\npt = [\"x\"]").is_err());
        assert!(Lines::parse("[trouble]\npt = [\"x\"]\nes = [\"y\"]").is_err());
        let missing_en = Lines::parse("[poked]\npt = [\"oi\"]").unwrap();
        assert_eq!(missing_en.problems().len(), 1);
    }
}
