//! The Library: everything the user picks, looks at and edits as a file — shaders, app
//! themes, window skins, backdrops and mascots — listed the same way, whatever its kind.
//!
//! Each item is built-in (in the program) or the user's (a file in the appearance folder,
//! `skin::user::dir`). Files that fail to load are listed too, with the reason, so the user
//! sees the error where they look for the item instead of in the log.
//! The window that shows all this is `hub/library.rs`; the mascots' own editing stays in
//! `apps/mascots.rs`, shown in the Library's Mascots tab.

pub mod postfx;
pub mod shaders;

use std::path::{Path, PathBuf};

use crate::skin::{self, backdrop, user};
use crate::tr;

/// The kinds of item, in the order of the Library's tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Shader,
    Theme,
    Skin,
    Backdrop,
    Mascot,
}

impl Kind {
    pub const ALL: [Kind; 5] = [Kind::Shader, Kind::Theme, Kind::Skin, Kind::Backdrop, Kind::Mascot];

    /// The name a manifest's `asset = "…"` and the Library's tabs use.
    pub fn key(self) -> &'static str {
        match self {
            Kind::Shader => "shader",
            Kind::Theme => "theme",
            Kind::Skin => "skin",
            Kind::Backdrop => "backdrop",
            Kind::Mascot => "mascot",
        }
    }

    pub fn from_key(key: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.key() == key)
    }

    /// Folder of the user's items of this kind.
    pub fn dir(self) -> PathBuf {
        let root = user::dir();
        match self {
            Kind::Shader => shaders::dir(),
            Kind::Theme => root.join(user::THEMES),
            Kind::Skin => root.join(user::SKINS),
            Kind::Backdrop => root.join(user::BACKDROPS),
            Kind::Mascot => root.join(crate::mascot::pack::DIR),
        }
    }

    /// File extension of an item of this kind (mascots are folders).
    fn extension(self) -> &'static str {
        match self {
            Kind::Shader => "glsl",
            _ => "toml",
        }
    }
}

/// Where an item comes from (mascots from the store are shown in their own tab).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Builtin,
    User,
}

impl Origin {
    pub fn label(self) -> &'static str {
        match self {
            Origin::Builtin => tr!("built-in"),
            Origin::User => tr!("yours"),
        }
    }
}

/// An item as the Library lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub key: String,
    pub name: String,
    pub origin: Origin,
    /// The item's file (`None` for a built-in one).
    pub path: Option<PathBuf>,
    /// Why the file did not load (`None` = it did).
    pub error: Option<String>,
}

/// The items of `kind`, built-in ones first, then the user's (those that failed to load
/// at the end of their kind's list, with the reason). Mascots come from `apps/mascots.rs`.
pub fn items(kind: Kind) -> Vec<Item> {
    let file = |key: &str| Some(kind.dir().join(format!("{key}.{}", kind.extension())));
    let mut out: Vec<Item> = match kind {
        Kind::Shader => shaders::all()
            .into_iter()
            .map(|s| Item { key: s.key, name: s.name, origin: s.origin, path: s.path, error: None })
            .collect(),
        Kind::Theme => crate::mascot::themes::all()
            .into_iter()
            .map(|t| {
                let builtin = crate::mascot::themes::BUILTIN.iter().any(|b| b.key == t.key);
                Item {
                    key: t.key.into(),
                    name: t.name().into(),
                    origin: if builtin { Origin::Builtin } else { Origin::User },
                    path: if builtin { None } else { file(t.key) },
                    error: None,
                }
            })
            .collect(),
        Kind::Skin => skin::all()
            .into_iter()
            .map(|s| {
                let builtin = skin::BUILTIN.iter().any(|b| b.key == s.key);
                Item {
                    key: s.key.into(),
                    name: s.name().into(),
                    origin: if builtin { Origin::Builtin } else { Origin::User },
                    path: if builtin { None } else { file(s.key) },
                    error: None,
                }
            })
            .collect(),
        Kind::Backdrop => backdrop::all()
            .into_iter()
            .map(|b| {
                let builtin = backdrop::BUILTIN.iter().any(|x| x.key == b.key);
                Item {
                    key: b.key.into(),
                    name: crate::core::i18n::tr(b.label).into(),
                    origin: if builtin { Origin::Builtin } else { Origin::User },
                    path: if builtin { None } else { file(b.key) },
                    error: None,
                }
            })
            .collect(),
        Kind::Mascot => Vec::new(),
    };
    out.extend(failed(kind));
    out
}

/// The user's files of `kind` that did not load.
fn failed(kind: Kind) -> Vec<Item> {
    let broken = |key: String, path: PathBuf, reason: String| Item {
        name: key.clone(),
        key,
        origin: Origin::User,
        path: Some(path),
        error: Some(reason),
    };
    match kind {
        Kind::Shader => shaders::skipped().into_iter().map(|s| broken(s.key, s.path, s.reason)).collect(),
        Kind::Mascot => Vec::new(),
        _ => {
            let folder = match kind {
                Kind::Theme => user::THEMES,
                Kind::Skin => user::SKINS,
                _ => user::BACKDROPS,
            };
            user::skipped()
                .into_iter()
                .filter_map(|s| {
                    let name = s.file.strip_prefix(folder)?.strip_prefix('/')?;
                    let key = name.strip_suffix(".toml").unwrap_or(name).to_string();
                    Some(broken(key, user::dir().join(&s.file), s.reason))
                })
                .collect()
        }
    }
}

/// The display name of an item, for lists outside the Library (a setting's summary).
pub fn name_of(kind: Kind, key: &str) -> String {
    items(kind).into_iter().find(|i| i.key == key).map_or_else(|| key.to_string(), |i| i.name)
}

/// Copies `key` into the user's folder under a free key, so it can be edited: a built-in
/// theme or skin becomes a file that starts from it (`base`), a shader is copied whole.
/// Returns the new key. Built-in backdrops have no file form and can't be copied.
pub fn duplicate(kind: Kind, key: &str) -> Result<String, String> {
    let item = items(kind).into_iter().find(|i| i.key == key && i.error.is_none()).ok_or_else(|| tr!("{} not found", key))?;
    let dir = kind.dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let taken: Vec<String> = items(kind).into_iter().map(|i| i.key).collect();
    let new = shaders::free_key(key, &taken);
    let copy_name = |name: &str| -> String {
        let n = tr!("{} (copy)", name);
        n.chars().take(40).collect()
    };
    match kind {
        Kind::Shader => {
            let s = shaders::find(key).ok_or_else(|| tr!("{} not found", key))?;
            write_new(&dir.join(format!("{new}.glsl")), &s.glsl)?;
            let mut meta = toml::Table::new();
            meta.insert("name".into(), copy_name(&s.name).into());
            for (field, value) in [("author", &s.author), ("license", &s.license), ("description", &s.description)] {
                if !value.is_empty() {
                    meta.insert(field.into(), value.clone().into());
                }
            }
            // a user shader's own adjustments (labels, ranges) go along
            if let Some(old) = s.meta_path.as_deref().and_then(|p| std::fs::read_to_string(p).ok())
                && let Ok(old) = old.parse::<toml::Table>()
                && let Some(params) = old.get("params")
            {
                meta.insert("params".into(), params.clone());
            }
            write_new(&dir.join(format!("{new}.toml")), &toml::to_string(&meta).map_err(|e| e.to_string())?)?;
        }
        Kind::Theme | Kind::Skin => {
            let text = match &item.path {
                // the user's own file: copied as it is, under the new name
                Some(path) => {
                    let mut t: toml::Table = std::fs::read_to_string(path).map_err(|e| e.to_string())?.parse().map_err(|e: toml::de::Error| e.message().to_string())?;
                    t.insert("label".into(), copy_name(&item.name).into());
                    toml::to_string(&t).map_err(|e| e.to_string())?
                }
                None => format!(
                    "# {}\nbase = \"{key}\"\nlabel = \"{}\"\n",
                    tr!("Starts from \"{}\": add here only what changes (see DESIGN.md).", item.name),
                    copy_name(&item.name).replace('"', "'")
                ),
            };
            write_new(&dir.join(format!("{new}.toml")), &text)?;
        }
        Kind::Backdrop => {
            let path = item.path.ok_or_else(|| tr!("A built-in backdrop can't be copied; start from the examples in examples/appearance/backdrops."))?;
            let mut t: toml::Table = std::fs::read_to_string(&path).map_err(|e| e.to_string())?.parse().map_err(|e: toml::de::Error| e.message().to_string())?;
            t.insert("label".into(), copy_name(&item.name).into());
            write_new(&dir.join(format!("{new}.toml")), &toml::to_string(&t).map_err(|e| e.to_string())?)?;
        }
        Kind::Mascot => return Err(tr!("Mascots are copied in their own tab.").into()),
    }
    Ok(new)
}

/// Removes the user's item (its file, the shader's `.toml` or folder too). Built-in
/// items can't be removed.
pub fn remove(kind: Kind, key: &str) -> Result<(), String> {
    let item = items(kind).into_iter().find(|i| i.key == key).ok_or_else(|| tr!("{} not found", key))?;
    let path = item.path.filter(|_| item.origin == Origin::User).ok_or_else(|| tr!("A built-in item can't be removed."))?;
    // a shader kept in its own folder goes whole
    let folder = path.parent().filter(|p| kind == Kind::Shader && path.file_name().is_some_and(|n| n == "shader.glsl") && p != &kind.dir());
    match folder {
        Some(dir) => std::fs::remove_dir_all(dir).map_err(|e| e.to_string())?,
        None => {
            std::fs::remove_file(&path).map_err(|e| e.to_string())?;
            let meta = path.with_extension("toml");
            if kind == Kind::Shader && meta.is_file() {
                std::fs::remove_file(meta).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

/// Copies files chosen by the user into the folder of `kind` (a shader's `.toml` with the
/// same name goes along). Returns the keys added; a file whose key is taken or invalid
/// stops the import with the reason.
pub fn import(kind: Kind, files: &[PathBuf]) -> Result<Vec<String>, String> {
    let dir = kind.dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let taken: Vec<String> = items(kind).into_iter().map(|i| i.key).collect();
    let mut added = Vec::new();
    for file in files {
        let key = file.file_stem().map(|s| s.to_string_lossy().to_lowercase().replace(' ', "-")).unwrap_or_default();
        shaders::check_key(&key)?;
        if taken.contains(&key) || added.contains(&key) {
            return Err(tr!("there is already one called \"{}\": rename the file and try again", key));
        }
        let target = dir.join(format!("{key}.{}", kind.extension()));
        std::fs::copy(file, &target).map_err(|e| format!("{}: {e}", file.display()))?;
        let meta = file.with_extension("toml");
        if kind == Kind::Shader && meta.is_file() {
            std::fs::copy(&meta, target.with_extension("toml")).map_err(|e| format!("{}: {e}", meta.display()))?;
        }
        added.push(key);
    }
    Ok(added)
}

/// Writes a file that must not exist yet (a copy never replaces anything).
fn write_new(path: &Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(text.as_bytes()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip_their_keys() {
        for k in Kind::ALL {
            assert_eq!(Kind::from_key(k.key()), Some(k));
        }
        assert_eq!(Kind::from_key("font"), None);
    }

    #[test]
    fn built_in_shaders_are_listed() {
        let keys: Vec<String> = items(Kind::Shader).into_iter().map(|i| i.key).collect();
        assert!(keys.starts_with(&["crt".to_string(), "bloom".to_string()]), "{keys:?}");
    }
}
