//! Interface language. Source strings are English; other languages come from gettext
//! `.po` catalogs.
//!
//! - **Hub (compiled UI and Rust):** `i18n/<lang>/*.po`. `ui/build.rs` merges the fragments
//!   into one catalog that is bundled for Slint's `@tr()` and exported as
//!   `miniutils_ui::PT_BR_CATALOG` for the Rust side ([`tr!`](crate::tr)).
//! - **Plugins and external apps:** `i18n/<lang>.po` in the app folder. It translates the
//!   manifest texts ([`Catalog::localize_manifest`]) and the `@tr("…")` of the interpreted
//!   view ([`Catalog::localize_slint`]); external apps get the language in `start` and
//!   translate their own messages.
//!
//! The test `catalog_covers_every_string` checks that every `tr!`/`@tr` of the hub has a
//! pt-BR entry and that the catalog has no stale entries.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

/// A language the interface can be shown in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    En,
    PtBr,
}

impl Language {
    /// Values of the `language` config option, in the order the settings screen lists them
    /// (index 0 is "auto").
    pub const KEYS: [&str; 3] = ["auto", "en", "pt-BR"];

    /// Locale tag: name of the catalog folder (`i18n/pt-BR`) and of the Slint bundle.
    pub fn tag(self) -> &'static str {
        match self {
            Language::En => "en",
            Language::PtBr => "pt-BR",
        }
    }

    /// Language from a locale tag such as `pt-BR`, `pt_BR.UTF-8` or `en-US`. Any other
    /// Portuguese counts as pt-BR (the only Portuguese catalog); the rest is English.
    pub fn from_locale(locale: &str) -> Language {
        let base = locale.split(['-', '_', '.', '@']).next().unwrap_or("");
        if base.eq_ignore_ascii_case("pt") { Language::PtBr } else { Language::En }
    }

    /// Resolves the `language` option: `"auto"` (or anything unknown) follows the system.
    pub fn from_setting(setting: &str) -> Language {
        match setting {
            "en" => Language::En,
            "pt-BR" => Language::PtBr,
            _ => sys_locale::get_locale().map_or(Language::En, |l| Language::from_locale(&l)),
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// Language the interface is shown in now.
pub fn current() -> Language {
    if CURRENT.load(Ordering::Relaxed) == 1 { Language::PtBr } else { Language::En }
}

/// Switches the language: Rust strings from now on, and the compiled Slint UI at once
/// (`select_bundled_translation` needs a component to exist; before the window is created
/// it fails and is retried by [`apply_to_ui`]).
pub fn set(lang: Language) {
    CURRENT.store(if lang == Language::PtBr { 1 } else { 0 }, Ordering::Relaxed);
    apply_to_ui();
}

/// Makes the compiled UI follow [`current`]. Call after creating the first window.
pub fn apply_to_ui() {
    if let Err(e) = slint::select_bundled_translation(current().tag()) {
        log::debug!("i18n: UI translation not selected yet: {e}");
    }
}

/// Translations of one language: msgid (English) → msgstr.
#[derive(Debug, Default, Clone)]
pub struct Catalog(HashMap<String, String>);

impl Catalog {
    /// Reads a gettext `.po` (msgid/msgstr, with continuation lines). Entries with an
    /// empty msgstr, plurals and the header are ignored; `msgctxt` is not supported. Entries
    /// flagged `#, fuzzy` (what `msgmerge` leaves when the English text changed) are skipped,
    /// like `msgfmt` does: the English text shows instead of a stale translation.
    pub fn parse(po: &str) -> Catalog {
        let mut map = HashMap::new();
        let mut id: Option<String> = None;
        let mut text: Option<String> = None;
        let mut target = 0; // 1 = continuing the msgid, 2 = the msgstr
        // the flags of the next entry (they come before its msgid) and of the current one
        let (mut next_fuzzy, mut fuzzy) = (false, false);
        let mut flush = |id: &mut Option<String>, text: &mut Option<String>, fuzzy: bool| {
            if let (Some(i), Some(t)) = (id.take(), text.take())
                && !i.is_empty()
                && !t.is_empty()
                && !fuzzy
            {
                map.insert(i, t);
            }
        };
        for line in po.lines().map(str::trim) {
            if let Some(flags) = line.strip_prefix("#,") {
                if flags.split(',').any(|f| f.trim() == "fuzzy") {
                    next_fuzzy = true;
                }
            } else if let Some(rest) = line.strip_prefix("msgid ") {
                flush(&mut id, &mut text, fuzzy);
                fuzzy = std::mem::take(&mut next_fuzzy);
                id = Some(unquote(rest));
                target = 1;
            } else if let Some(rest) = line.strip_prefix("msgstr ") {
                text = Some(unquote(rest));
                target = 2;
            } else if line.starts_with('"') {
                let piece = unquote(line);
                match target {
                    1 => id.get_or_insert_default().push_str(&piece),
                    2 => text.get_or_insert_default().push_str(&piece),
                    _ => {}
                }
            } else if line.is_empty() || line.starts_with('#') {
                // comment or entry separator
            } else {
                // msgid_plural, msgstr[n], msgctxt: not used by this project
                target = 0;
            }
        }
        flush(&mut id, &mut text, fuzzy);
        Catalog(map)
    }

    /// Catalog of `lang` in the folder of a plugin or external app (`i18n/<tag>.po`).
    /// Empty when the app has none (its English texts are shown).
    pub fn of_app(dir: &std::path::Path, lang: Language) -> Catalog {
        if lang == Language::En {
            return Catalog::default();
        }
        let path = dir.join("i18n").join(format!("{}.po", lang.tag()));
        match std::fs::read_to_string(&path) {
            Ok(text) => Catalog::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Catalog::default(),
            Err(e) => {
                log::warn!("i18n: {}: {e}", path.display());
                Catalog::default()
            }
        }
    }

    pub fn get(&self, msgid: &str) -> Option<&str> {
        self.0.get(msgid).map(String::as_str)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[cfg(test)]
    pub fn msgids(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    /// Translates the `@tr("…")` of an interpreted `.slint` (plugin or external app view):
    /// the literal is replaced by its translation, so the interpreter, which has no
    /// bundled translations, already sees the translated text. Arguments and `{}`
    /// placeholders are kept, so `@tr` still formats them.
    pub fn localize_slint(&self, source: &str) -> String {
        if self.is_empty() {
            return source.to_string();
        }
        let mut out = String::with_capacity(source.len());
        let mut rest = source;
        while let Some(i) = rest.find("@tr(") {
            let (head, tail) = rest.split_at(i + "@tr(".len());
            out.push_str(head);
            rest = tail;
            let lead = rest.len() - rest.trim_start().len();
            out.push_str(&rest[..lead]);
            rest = &rest[lead..];
            let Some((literal, after)) = split_slint_string(rest) else { continue };
            // `@tr("context" => "text")`: contexts are not used; leave it untouched
            if after.trim_start().starts_with("=>") {
                out.push_str(&rest[..rest.len() - after.len()]);
                rest = after;
                continue;
            }
            match self.get(&unescape(literal)) {
                Some(t) => {
                    out.push('"');
                    out.push_str(&escape_slint(t));
                    out.push('"');
                }
                None => out.push_str(&rest[..rest.len() - after.len()]),
            }
            rest = after;
        }
        out.push_str(rest);
        out
    }

    /// Translates the texts of a manifest: name, description and setting labels. Choice
    /// options are values, not texts, and stay as they are.
    pub fn localize_manifest(&self, m: &mut super::manifest::Manifest) {
        if self.is_empty() {
            return;
        }
        let tr = |s: &mut String| {
            if let Some(t) = self.get(s) {
                *s = t.to_string();
            }
        };
        tr(&mut m.name);
        tr(&mut m.description);
        for s in &mut m.settings {
            tr(&mut s.label);
            tr(&mut s.help);
            tr(&mut s.group);
            // only labels written in the manifest: a plain option's label is its saved value
            for (value, label) in s.options.iter().zip(&mut s.option_labels) {
                if label != value {
                    tr(label);
                }
            }
        }
    }
}

/// The hub's own catalog for `lang` (empty for English).
pub fn hub_catalog(lang: Language) -> &'static Catalog {
    static EMPTY: OnceLock<Catalog> = OnceLock::new();
    static PT_BR: OnceLock<Catalog> = OnceLock::new();
    match lang {
        Language::En => EMPTY.get_or_init(Catalog::default),
        Language::PtBr => PT_BR.get_or_init(|| Catalog::parse(miniutils_ui::PT_BR_CATALOG)),
    }
}

/// Translates a hub string to the current language. Prefer the [`tr!`](crate::tr) macro.
pub fn tr(msgid: &'static str) -> &'static str {
    hub_catalog(current()).get(msgid).unwrap_or(msgid)
}

/// Fills `{}` (in order) and `{0}`, `{1}`… placeholders, like Slint's `@tr`. `{{` and `}}`
/// are literal braces.
pub fn format(template: &str, args: &[&dyn Display]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut next = 0;
    let mut chars = template.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '{' if chars.peek().is_some_and(|&(_, c)| c == '{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek().is_some_and(|&(_, c)| c == '}') => {
                chars.next();
                out.push('}');
            }
            '{' => {
                let Some(close) = template[i..].find('}') else {
                    out.push_str(&template[i..]);
                    break;
                };
                let inner = &template[i + 1..i + close];
                let idx = if inner.is_empty() {
                    next += 1;
                    Some(next - 1)
                } else {
                    inner.parse::<usize>().ok()
                };
                match idx.and_then(|n| args.get(n)) {
                    Some(a) => out.push_str(&a.to_string()),
                    None => out.push_str(&template[i..=i + close]),
                }
                while chars.peek().is_some_and(|&(j, _)| j <= i + close) {
                    chars.next();
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// Translates a hub string: `tr!("Settings")` gives `&'static str`;
/// `tr!("Updated {} apps", n)` fills the placeholders and gives `String`.
/// The first argument must be a string literal (the catalog test reads it from the source).
#[macro_export]
macro_rules! tr {
    ($msgid:literal) => {
        $crate::core::i18n::tr($msgid)
    };
    ($msgid:literal, $($arg:expr),+ $(,)?) => {
        $crate::core::i18n::format($crate::core::i18n::tr($msgid), &[$(&$arg as &dyn ::std::fmt::Display),+])
    };
}

/// Marks a string for the catalog without translating it here: for tables of labels
/// (`const ACCENTS = [("cyan", tr_noop!("Cyan"))]`) that are translated where shown, with
/// [`tr`]`(label)`. The catalog test reads these literals like the ones of [`tr!`](crate::tr).
#[macro_export]
macro_rules! tr_noop {
    ($msgid:literal) => {
        $msgid
    };
}

/// Content of a `"…"` literal (still escaped) at the start of `s`, and what follows it.
fn split_slint_string(s: &str) -> Option<(&str, &str)> {
    let body = s.strip_prefix('"')?;
    let mut escaped = false;
    for (i, c) in body.char_indices() {
        match c {
            '\\' if !escaped => escaped = true,
            '"' if !escaped => return Some((&body[..i], &body[i + 1..])),
            _ => escaped = false,
        }
    }
    None
}

/// Reads one `"…"` of a .po line.
fn unquote(s: &str) -> String {
    let s = s.trim();
    let inner = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(s);
    unescape(inner)
}

/// C/Rust/Slint escapes common to all three: `\n`, `\t`, `\"`, `\\`.
pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(o) => {
                out.push('\\');
                out.push(o);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn escape_slint(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PO: &str = r#"
# header
msgid ""
msgstr ""
"Content-Type: text/plain; charset=UTF-8\n"

#: src/hub/settings.rs
msgid "Settings"
msgstr "Configurações"

msgid ""
"Two "
"lines"
msgstr "Duas "
"linhas"

msgid "Not translated"
msgstr ""

msgid "Say \"hi\"\n"
msgstr "Diga \"oi\"\n"

#: src/x.rs
#, fuzzy, c-format
msgid "Changed in English"
msgstr "Tradução antiga"

msgid "After the fuzzy one"
msgstr "Depois da incerta"
"#;

    #[test]
    fn parses_po_entries() {
        let c = Catalog::parse(PO);
        assert_eq!(c.get("Settings"), Some("Configurações"));
        assert_eq!(c.get("Two lines"), Some("Duas linhas"));
        assert_eq!(c.get("Not translated"), None);
        assert_eq!(c.get("Say \"hi\"\n"), Some("Diga \"oi\"\n"));
        // fuzzy: stale translation, not used; the flag does not leak to the next entry
        assert_eq!(c.get("Changed in English"), None);
        assert_eq!(c.get("After the fuzzy one"), Some("Depois da incerta"));
        assert_eq!(c.len(), 4, "the header is not an entry");
    }

    #[test]
    fn formats_positional_and_indexed_placeholders() {
        assert_eq!(format("{} of {}", &[&1, &3]), "1 of 3");
        assert_eq!(format("{1} before {0}", &[&"a", &"b"]), "b before a");
        assert_eq!(format("{{literal}} {}", &[&5]), "{literal} 5");
        assert_eq!(format("missing {}", &[]), "missing {}");
        assert_eq!(format("ação {}", &[&"ok"]), "ação ok");
    }

    #[test]
    fn locale_tags_map_to_languages() {
        assert_eq!(Language::from_locale("pt-BR"), Language::PtBr);
        assert_eq!(Language::from_locale("pt_BR.UTF-8"), Language::PtBr);
        assert_eq!(Language::from_locale("pt-PT"), Language::PtBr);
        assert_eq!(Language::from_locale("en-US"), Language::En);
        assert_eq!(Language::from_locale("de_DE"), Language::En);
        assert_eq!(Language::from_setting("en"), Language::En);
        assert_eq!(Language::from_setting("pt-BR"), Language::PtBr);
    }

    #[test]
    fn localizes_tr_literals_in_slint() {
        let c = Catalog::parse("msgid \"Notes\"\nmsgstr \"Notas\"\nmsgid \"{} left\"\nmsgstr \"faltam {}\"\nmsgid \"Quote\"\nmsgstr \"Diz \\\"oi\\\"\"\n");
        let src = r#"Text { text: @tr("Notes"); } Text { text: @tr( "{} left", n); } Text { text: @tr("Other"); } Text { text: @tr("Quote"); } Text { text: @tr("ctx" => "Notes"); }"#;
        let out = c.localize_slint(src);
        assert_eq!(
            out,
            r#"Text { text: @tr("Notas"); } Text { text: @tr( "faltam {}", n); } Text { text: @tr("Other"); } Text { text: @tr("Diz \"oi\""); } Text { text: @tr("ctx" => "Notes"); }"#
        );
    }

    /// Every `tr!("…")` in `src/` and `@tr("…")` in the compiled `.slint` files, plus the
    /// texts of the built-in manifests, has a pt-BR translation; and every catalog entry is
    /// still used somewhere.
    #[test]
    fn catalog_covers_every_string() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut used = std::collections::BTreeSet::new();
        let mut files = Vec::new();
        collect(&root.join("src"), &mut files);
        collect(&root.join("ui"), &mut files);
        collect(&root.join("miniapps"), &mut files);
        for f in &files {
            let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !matches!(ext, "rs" | "slint" | "toml") {
                continue;
            }
            let text = std::fs::read_to_string(f).unwrap();
            match ext {
                "rs" if f.ends_with("core/i18n.rs") => {}
                "rs" => {
                    literals_after(&text, "tr!(", &mut used);
                    literals_after(&text, "tr_noop!(", &mut used);
                }
                "slint" => literals_after(&text, "@tr(", &mut used),
                "toml" if f.file_name().is_some_and(|n| n == "manifest.toml") => {
                    let m: toml::Table = toml::from_str(&text).unwrap();
                    for k in ["name", "description"] {
                        if let Some(s) = m.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                            used.insert(s.to_string());
                        }
                    }
                    for s in m.get("settings").and_then(|v| v.as_array()).into_iter().flatten() {
                        for k in ["label", "help", "group"] {
                            if let Some(l) = s.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                                used.insert(l.to_string());
                            }
                        }
                        for o in s.get("options").and_then(|v| v.as_array()).into_iter().flatten() {
                            if let Some(l) = o.get("label").and_then(|v| v.as_str()) {
                                used.insert(l.to_string());
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        let cat = hub_catalog(Language::PtBr);
        let missing: Vec<_> = used.iter().filter(|s| cat.get(s).is_none()).collect();
        let stale: Vec<_> = cat.msgids().filter(|s| !used.contains(*s)).collect();
        assert!(missing.is_empty(), "no pt-BR translation in i18n/pt-BR/*.po for: {missing:#?}");
        assert!(stale.is_empty(), "i18n/pt-BR/*.po has entries no longer used: {stale:#?}");
    }

    fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                if !p.ends_with("target") {
                    collect(&p, out);
                }
            } else {
                out.push(p);
            }
        }
    }

    /// The string literal right after each `marker` (skipping `@tr("ctx" => …)` contexts).
    fn literals_after(text: &str, marker: &str, out: &mut std::collections::BTreeSet<String>) {
        for (i, _) in text.match_indices(marker) {
            // `tr!(` but not `include_str!(` / `my_tr!(`
            if text[..i].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_') {
                continue;
            }
            let rest = text[i + marker.len()..].trim_start();
            if let Some((lit, after)) = split_slint_string(rest)
                && !after.trim_start().starts_with("=>")
            {
                out.insert(unescape(lit));
            }
        }
    }
}
