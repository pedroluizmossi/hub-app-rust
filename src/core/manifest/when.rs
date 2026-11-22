//! The `when` of a setting: the setting only shows on Settings › Apps while the condition
//! holds. Grammar (spaces around the operator are optional):
//!
//! - `<ident>`: the value is "set" (a `true` bool, a non-empty text, a number other than 0);
//! - `!<ident>`: the opposite;
//! - `<ident> == '<text>'` / `<ident> != '<text>'`: the value written as text (`true`, `42`,
//!   `0.5`, the option or the text itself) is / is not `<text>`. Double quotes work too.
//!
//! `<ident>` is another setting of the same app or a hub fact ([`HUB_RENDERER`]).

/// Hub fact: how the windows are drawn this run, `"software"` or `"opengl"`.
pub const HUB_RENDERER: &str = "hub.renderer";

/// A parsed `when`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct When {
    /// Setting key (or hub fact) the condition reads.
    pub key: String,
    pub test: Test,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Test {
    Set,
    Unset,
    Is(String),
    IsNot(String),
}

impl When {
    /// `None` if the text does not follow the grammar.
    pub fn parse(text: &str) -> Option<When> {
        let text = text.trim();
        let op = [("==", true), ("!=", false)]
            .into_iter()
            .filter_map(|(op, is)| text.find(op).map(|at| (at, op.len(), is)))
            .min_by_key(|(at, ..)| *at);
        if let Some((at, len, is)) = op {
            let key = ident(&text[..at])?;
            let quoted = text[at + len..].trim();
            let quote = quoted.chars().next().filter(|q| *q == '\'' || *q == '"')?;
            let inner = quoted[1..].strip_suffix(quote)?;
            if inner.contains(quote) {
                return None;
            }
            let test = if is { Test::Is(inner.into()) } else { Test::IsNot(inner.into()) };
            return Some(When { key, test });
        }
        match text.strip_prefix('!') {
            Some(rest) => Some(When { key: ident(rest)?, test: Test::Unset }),
            None => Some(When { key: ident(text)?, test: Test::Set }),
        }
    }

    /// Whether the condition holds for `value`, the current value of [`When::key`].
    pub fn holds(&self, value: &toml::Value) -> bool {
        match &self.test {
            Test::Set => is_set(value),
            Test::Unset => !is_set(value),
            Test::Is(t) => as_text(value) == *t,
            Test::IsNot(t) => as_text(value) != *t,
        }
    }
}

/// A setting key or hub fact: letters, digits, `_`, `-` and `.`.
fn ident(text: &str) -> Option<String> {
    let text = text.trim();
    let valid = !text.is_empty() && text.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'));
    valid.then(|| text.to_string())
}

fn is_set(value: &toml::Value) -> bool {
    match value {
        toml::Value::Boolean(b) => *b,
        toml::Value::String(s) => !s.is_empty(),
        toml::Value::Integer(i) => *i != 0,
        toml::Value::Float(f) => *f != 0.0,
        toml::Value::Array(a) => !a.is_empty(),
        toml::Value::Table(t) => !t.is_empty(),
        toml::Value::Datetime(_) => true,
    }
}

fn as_text(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn when(key: &str, test: Test) -> Option<When> {
        Some(When { key: key.into(), test })
    }

    #[test]
    fn parses_the_four_forms() {
        assert_eq!(When::parse("notify"), when("notify", Test::Set));
        assert_eq!(When::parse(" ! notify "), when("notify", Test::Unset));
        assert_eq!(When::parse("sort == 'cpu'"), when("sort", Test::Is("cpu".into())));
        assert_eq!(When::parse("sort!='cpu'"), when("sort", Test::IsNot("cpu".into())));
        assert_eq!(When::parse("hub.renderer == \"opengl\""), when(HUB_RENDERER, Test::Is("opengl".into())));
        // the operator inside the quoted text is part of the text
        assert_eq!(When::parse("a == 'b==c'"), when("a", Test::Is("b==c".into())));
        assert_eq!(When::parse("a == ''"), when("a", Test::Is(String::new())));
    }

    #[test]
    fn refuses_bad_syntax() {
        for bad in ["", "!", "a b", "a == cpu", "a == 'cpu", "a == 'c'u'", "== 'x'", "a = 'x'", "!!a", "a && b", "a == 'x' 'y'"] {
            assert_eq!(When::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn evaluates_against_values() {
        let set = When::parse("k").unwrap();
        let unset = When::parse("!k").unwrap();
        for (v, on) in [
            (toml::Value::Boolean(true), true),
            (toml::Value::Boolean(false), false),
            (toml::Value::String("x".into()), true),
            (toml::Value::String(String::new()), false),
            (toml::Value::Integer(0), false),
            (toml::Value::Integer(3), true),
            (toml::Value::Float(0.0), false),
            (toml::Value::Float(0.5), true),
        ] {
            assert_eq!(set.holds(&v), on, "{v}");
            assert_eq!(unset.holds(&v), !on, "{v}");
        }
        let cpu = When::parse("sort == 'cpu'").unwrap();
        assert!(cpu.holds(&toml::Value::String("cpu".into())));
        assert!(!cpu.holds(&toml::Value::String("ram".into())));
        assert!(When::parse("sort != 'cpu'").unwrap().holds(&toml::Value::String("ram".into())));
        // other types compare as written
        assert!(When::parse("on == 'true'").unwrap().holds(&toml::Value::Boolean(true)));
        assert!(When::parse("n == '42'").unwrap().holds(&toml::Value::Integer(42)));
        assert!(When::parse("f == '0.5'").unwrap().holds(&toml::Value::Float(0.5)));
    }
}
