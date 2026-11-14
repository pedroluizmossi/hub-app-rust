//! Changes to a mascot's own file, from the Mascots app: the name, which animation shows each
//! mood and reaction (PNG sprite), the `[palette]` colors (PNG sprite) and the walking stops
//! (`extras`, both kinds). The file stays the person's: `toml_edit` keeps its comments and
//! order. Every change is checked by reading the mascot again, as the hub will; if it
//! doesn't load, the file goes back to what it was and the reason is returned.
//!
//! Built-in mascots have no file: they can't be changed.

use std::path::{Path, PathBuf};

use toml_edit::{Array, DocumentMut, Item, Table, value};

use super::sprites::Sprite;
use super::{file, pack};
use crate::tr;

/// The file of a mascot that can be changed.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// `mascots/<key>/sprite.toml` of a PNG sprite.
    Sprite(PathBuf),
    /// `sprites/<key>.toml` of a code mascot.
    Code(PathBuf),
}

impl Target {
    fn path(&self) -> &Path {
        match self {
            Target::Sprite(p) | Target::Code(p) => p,
        }
    }
}

/// The file of `sprite` in `appearance` (`None`: a built-in mascot). A PNG sprite without
/// `sprite.toml` gets one written from its sheet names.
pub fn target(sprite: &Sprite, appearance: &Path) -> Result<Option<Target>, String> {
    if pack::art(sprite).is_some() {
        let dir = appearance.join(pack::DIR).join(sprite.key);
        return pack::ensure_spec(&dir).map(|p| Some(Target::Sprite(p)));
    }
    let p = appearance.join(file::DIR).join(format!("{}.toml", sprite.key));
    Ok(p.is_file().then_some(Target::Code(p)))
}

/// Shown name: `name` in a `sprite.toml`, `label` in a code mascot.
pub fn rename(t: &Target, name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 40 {
        return Err(tr!("label: 1 to 40 characters").into());
    }
    let key = match t {
        Target::Sprite(_) => "name",
        Target::Code(_) => "label",
    };
    change(t, |doc| {
        doc[key] = value(name);
        Ok(())
    })
}

/// Which animation shows mood or reaction `which` (`idle`, `celebrate`…) in a PNG sprite;
/// `None` goes back to the usual names.
pub fn set_mood(t: &Target, which: &str, anim: Option<&str>) -> Result<(), String> {
    let Target::Sprite(_) = t else { return Err(tr!("only a PNG sprite chooses its animations here").into()) };
    let section = if ["celebrate", "surprised"].contains(&which) { "reactions" } else { "moods" };
    change(t, |doc| {
        let table = section_mut(doc, section);
        match anim {
            Some(a) => {
                table[which] = value(a);
            }
            None => {
                table.remove(which);
            }
        }
        Ok(())
    })
}

/// The color role of PNG color `hex` (`#rrggbb`) in `[palette]`; `None` removes it (the
/// color keeps its own).
pub fn set_palette(t: &Target, hex: &str, role: Option<char>) -> Result<(), String> {
    let Target::Sprite(_) = t else { return Err(tr!("a code mascot already follows the theme").into()) };
    let color = crate::skin::color::parse_hex(hex).ok_or_else(|| tr!("{}: expected a color \"#RRGGBB\"", hex.to_string()))?;
    change(t, |doc| {
        let table = section_mut(doc, "palette");
        // the same color may be written another way (`#C8001E`, without `#`): one entry only
        let same: Vec<String> =
            table.iter().filter(|(k, _)| crate::skin::color::parse_hex(k) == Some(color)).map(|(k, _)| k.to_string()).collect();
        for k in same {
            table.remove(&k);
        }
        if let Some(r) = role {
            table[hex] = value(r.to_string());
        }
        Ok(())
    })
}

/// The stops of the walking subagents besides `idle` (`extras`); empty removes the key.
pub fn set_extras(t: &Target, list: &[String]) -> Result<(), String> {
    change(t, |doc| {
        if list.is_empty() {
            doc.remove("extras");
        } else {
            let mut a = Array::new();
            // a PNG sprite lists idle among them (without `extras` it stops only at idle), if
            // the pack has one: a name that isn't in `[anim]` would make it fail to load
            let has_idle = doc.get("anim").and_then(|a| a.get("idle")).is_some();
            if matches!(t, Target::Sprite(_)) && has_idle && !list.iter().any(|n| n == "idle") {
                a.push("idle");
            }
            for n in list {
                a.push(n.as_str());
            }
            // top-level key: before the first table, so it isn't read as part of one
            doc["extras"] = value(a);
        }
        Ok(())
    })
}

/// The `[name]` table, made if missing. One written inline (`moods = { busy = "run" }`)
/// becomes a regular table with the same entries (never emptied).
fn section_mut<'a>(doc: &'a mut DocumentMut, name: &str) -> &'a mut Table {
    if !doc.contains_table(name) {
        let inline = doc.get(name).and_then(|i| i.as_inline_table()).cloned();
        doc[name] = Item::Table(inline.map(|t| t.into_table()).unwrap_or_default());
    }
    doc[name].as_table_mut().expect("just made a table")
}

/// Writes `text` to `path` through a file next to it, renamed at the end: a failed write
/// never leaves `path` half written.
fn write_whole(path: &Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

/// Edits the file, checks that the mascot still loads, and puts the old text back if not.
fn change(t: &Target, edit: impl FnOnce(&mut DocumentMut) -> Result<(), String>) -> Result<(), String> {
    let path = t.path();
    let old = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut doc: DocumentMut = old.parse().map_err(|e: toml_edit::TomlError| format!("{}: {}", path.display(), e.message()))?;
    edit(&mut doc)?;
    write_whole(path, &doc.to_string())?;
    let checked = match t {
        Target::Sprite(p) => pack::read_folder(p.parent().unwrap_or(Path::new("."))).map(|_| ()),
        Target::Code(p) => file::read(p).map(|_| ()),
    };
    if let Err(e) = checked {
        let _ = write_whole(path, &old);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("miniutils-edit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn edits_a_code_mascot_keeping_comments_and_refuses_what_breaks_it() {
        let d = temp("code");
        let p = d.join("jelly.toml");
        std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/appearance/sprites/jellyfish.toml"), &p).unwrap();
        let t = Target::Code(p.clone());
        rename(&t, "Jelly Lu").unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("label = \"Jelly Lu\""), "{text}");
        assert!(text.contains("# A 16 × 14 mascot"), "the comments stay");
        // a stop that isn't an animation of the file: refused, the file as it was
        assert!(set_extras(&t, &["boom".to_string()]).unwrap_err().contains("boom"));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), text);
        assert!(rename(&t, "").is_err());
        // a code mascot has no [palette] of PNG colors nor mood choice here
        assert!(set_palette(&t, "#ffffff", Some('K')).is_err());
        assert!(set_mood(&t, "idle", Some("idle")).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A pack without `idle`: its stops don't get one; and an inline `[moods]` keeps its
    /// entries when another is added.
    #[test]
    fn png_without_idle_and_inline_moods() {
        let d = temp("noidle");
        let p = d.join("bot");
        std::fs::create_dir_all(&p).unwrap();
        let img = image::RgbaImage::from_fn(32, 16, |x, y| {
            if (4..12).contains(&(x % 16)) && (4..12).contains(&y) { image::Rgba([10, 200, 30, 255]) } else { image::Rgba([0, 0, 0, 0]) }
        });
        for f in ["Bot_Walk_2.png", "Bot_Boom_2.png", "Bot_Hurt_2.png"] {
            img.save(p.join(f)).unwrap();
        }
        let spec = p.join("sprite.toml");
        std::fs::write(&spec, "moods = { busy = \"walk\" }\n[anim]\nwalk = \"Bot_Walk_2.png\"\nboom = \"Bot_Boom_2.png\"\nhurt = \"Bot_Hurt_2.png\"\n").unwrap();
        let t = Target::Sprite(spec.clone());
        set_extras(&t, &["boom".to_string()]).unwrap();
        let s = pack::read_folder(&p).unwrap().troupe().unwrap().script.clone();
        assert_eq!(s.extras.iter().map(|&i| s.names[i].as_str()).collect::<Vec<_>>(), ["boom"]);
        set_mood(&t, "alarm", Some("hurt")).unwrap();
        let (moods, _) = pack::read_folder(&p).unwrap().mapping();
        assert_eq!((moods[3].as_str(), moods[4].as_str()), ("walk", "hurt"));
        assert!(!p.join("sprite.toml.tmp").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn edits_a_png_sprite() {
        let d = temp("png");
        let p = d.join("blob");
        std::fs::create_dir_all(&p).unwrap();
        let img = image::RgbaImage::from_fn(32, 16, |x, y| {
            if (4..12).contains(&(x % 16)) && (4..12).contains(&y) { image::Rgba([200, 0, 30, 255]) } else { image::Rgba([0, 0, 0, 0]) }
        });
        for f in ["Blob_Idle_2.png", "Blob_Walk_2.png", "Blob_Boom_2.png"] {
            img.save(p.join(f)).unwrap();
        }
        // no sprite.toml yet: one is written from the names
        let spec = pack::ensure_spec(&p).unwrap();
        let t = Target::Sprite(spec.clone());
        set_mood(&t, "busy", Some("walk")).unwrap();
        set_palette(&t, "#c8001e", Some('P')).unwrap();
        set_extras(&t, &["boom".to_string()]).unwrap();
        rename(&t, "Blobby").unwrap();
        let a = pack::read_folder(&p).unwrap();
        assert_eq!(a.name, "Blobby");
        assert_eq!(a.mapping().0[3], "walk");
        assert!(a.themed());
        let s = &a.troupe().unwrap().script;
        assert_eq!(s.extras.iter().map(|&i| s.names[i].as_str()).collect::<Vec<_>>(), ["boom", "idle"]);
        // an animation that doesn't exist: refused, nothing changes
        let before = std::fs::read_to_string(&spec).unwrap();
        assert!(set_mood(&t, "alarm", Some("nope")).is_err());
        assert_eq!(std::fs::read_to_string(&spec).unwrap(), before);
        // written in capitals by hand: still the same color, one entry, and None removes it
        let text = std::fs::read_to_string(&spec).unwrap().replace("\"#c8001e\"", "\"#C8001E\"");
        std::fs::write(&spec, text).unwrap();
        set_palette(&t, "#c8001e", Some('K')).unwrap();
        assert_eq!(std::fs::read_to_string(&spec).unwrap().to_lowercase().matches("#c8001e").count(), 1);
        set_palette(&t, "#c8001e", None).unwrap();
        assert!(!pack::read_folder(&p).unwrap().themed());
        let _ = std::fs::remove_dir_all(&d);
    }
}
