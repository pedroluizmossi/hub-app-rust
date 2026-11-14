//! Mascots in the store: packs of drawings, with no code. An entry in the index's
//! `mascots` list pins the repository and the reviewed commit, like a plugin; the
//! repository (or its `path`) holds any of:
//!
//! ```text
//! sprites/<key>.toml     16 × 14 sprites that follow the theme (`mascot/file.rs`)
//! mascots/<key>/         imported PNG mascots: sheets and sprite.toml (`mascot/pack.rs`)
//! ```
//!
//! Only those files are copied (`.toml` in `sprites/`; `.png` and `sprite.toml` one level
//! inside each `mascots/<key>/`, real files only), and the copy is checked with the same
//! readers the hub uses before anything goes into the appearance folder
//! (`skin::user::dir`). A receipt, `store/<id>.json`, lists what went in: an update or a
//! removal only touches those files, and a key that already exists there (the user's own,
//! or another entry's) is refused.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Trust, check_source, checkout};
use crate::core::plugins;
use crate::mascot::{file as sprite_file, pack};
use crate::tr;

/// Folder of the receipts, inside the appearance folder.
pub const RECEIPTS: &str = "store";
/// Most sprites and PNG mascots one entry may bring.
const MAX_SPRITES: usize = 64;
const MAX_PACKS: usize = 16;
/// Most files in one PNG mascot (sheets and `sprite.toml`).
const MAX_PACK_FILES: usize = 40;

/// A mascot entry in the index.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MascotEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    pub license: String,
    pub version: String,
    /// Git repository: `https://…` or a local folder (for tests).
    pub repo: String,
    /// Subfolder in the repository with `sprites/` and `mascots/`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    /// Reviewed commit (full SHA, 40 characters).
    pub commit: String,
}

/// What an installed entry put in the appearance folder (`store/<id>.json`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MascotInstalled {
    pub id: String,
    pub version: String,
    pub repo: String,
    pub commit: String,
    /// Keys of `sprites/<key>.toml`.
    #[serde(default)]
    pub sprites: Vec<String>,
    /// Keys of `mascots/<key>/`.
    #[serde(default)]
    pub packs: Vec<String>,
}

impl MascotInstalled {
    /// "sprites: owl, cat · PNG: knight"
    pub fn contents(&self) -> String {
        contents(&self.sprites, &self.packs)
    }
}

fn contents(sprites: &[String], packs: &[String]) -> String {
    let mut parts = Vec::new();
    if !sprites.is_empty() {
        parts.push(tr!("sprites: {}", sprites.join(", ")));
    }
    if !packs.is_empty() {
        parts.push(tr!("PNG: {}", packs.join(", ")));
    }
    parts.join(" · ")
}

/// A mascot entry from the index and its state here.
#[derive(Debug, Clone)]
pub struct MascotStatus {
    pub entry: MascotEntry,
    pub installed: Option<MascotInstalled>,
}

impl MascotStatus {
    /// Installed at a commit other than the one the index pins now.
    pub fn has_update(&self) -> bool {
        self.installed.as_ref().is_some_and(|i| !i.commit.eq_ignore_ascii_case(&self.entry.commit))
    }
}

/// What entry `id` installed in `appearance`, if anything.
pub fn installed(appearance: &Path, id: &str) -> Option<MascotInstalled> {
    let text = std::fs::read_to_string(receipt(appearance, id)).ok()?;
    serde_json::from_str(&text).ok()
}

fn receipt(appearance: &Path, id: &str) -> PathBuf {
    appearance.join(RECEIPTS).join(format!("{id}.json"))
}

/// `dir` is a real folder, not a link (a link in the repository could point outside it).
fn real_dir(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir())
}

/// The files of `dir` that pass `keep`: real files only (no links), in order.
fn real_files(dir: &Path, keep: impl Fn(&str) -> bool) -> Vec<(String, PathBuf)> {
    if !real_dir(dir) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| Some((e.file_name().to_str()?.to_string(), e.path())))
        .filter(|(name, _)| keep(name))
        .collect();
    v.sort();
    v
}

/// Copies the allowed files of `src` into `stage` (same layout) and checks the copy with
/// the hub's readers. Returns the keys: (sprites, PNG mascots).
fn stage(id: &str, src: &Path, stage: &Path) -> Result<(Vec<String>, Vec<String>), String> {
    let copy = |from: &Path, to: &Path| -> Result<(), String> {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::copy(from, to).map(|_| ()).map_err(|e| format!("{}: {e}", from.display()))
    };
    let sprite_files = real_files(&src.join(sprite_file::DIR), |n| n.ends_with(".toml"));
    if sprite_files.len() > MAX_SPRITES {
        return Err(tr!("{}: too many sprites ({}; up to {})", id, sprite_files.len(), MAX_SPRITES));
    }
    for (name, path) in &sprite_files {
        copy(path, &stage.join(sprite_file::DIR).join(name))?;
    }
    let packs_root = src.join(pack::DIR);
    let mut pack_dirs: Vec<(String, PathBuf)> = std::fs::read_dir(&packs_root)
        .into_iter()
        .filter(|_| real_dir(&packs_root))
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| Some((e.file_name().to_str()?.to_string(), e.path())))
        .collect();
    pack_dirs.sort();
    if pack_dirs.len() > MAX_PACKS {
        return Err(tr!("{}: too many PNG mascots ({}; up to {})", id, pack_dirs.len(), MAX_PACKS));
    }
    for (key, dir) in &pack_dirs {
        let files = real_files(dir, |n| n.to_ascii_lowercase().ends_with(".png") || n == "sprite.toml");
        if files.len() > MAX_PACK_FILES {
            return Err(tr!("{}: {}/{} has too many files (up to {})", id, pack::DIR, key, MAX_PACK_FILES));
        }
        for (name, path) in files {
            copy(&path, &stage.join(pack::DIR).join(key).join(name))?;
        }
    }

    // the copy, checked as the hub will read it
    let (arts, skipped) = pack::load(&stage.join(pack::DIR));
    if let Some(s) = skipped.first() {
        return Err(format!("{id}: {}: {}", s.dir, s.reason));
    }
    let packs: Vec<String> = arts.iter().map(|a| a.key.clone()).collect();
    let (sprites, skipped) = sprite_file::load(&stage.join(sprite_file::DIR), &packs);
    if let Some(s) = skipped.first() {
        return Err(format!("{id}: {}: {}", s.file, s.reason));
    }
    let sprites: Vec<String> = sprites.iter().map(|f| f.sprite.key.to_string()).collect();
    if sprites.is_empty() && packs.is_empty() {
        return Err(tr!("{}: no mascot (sprites/<key>.toml or mascots/<key>/) in the repository", id));
    }
    Ok((sprites, packs))
}

/// Installs (or updates) mascot entry `e` into `appearance`. `confirm` gets the summary and
/// decides. Returns a line on what went in.
pub fn install(
    e: &MascotEntry,
    trust: &Trust,
    appearance: &Path,
    confirm: &mut dyn FnMut(&str) -> bool,
) -> Result<String, String> {
    plugins::validate_id(&e.id)?;
    check_source(&e.id, &e.repo, &e.path, &e.commit, &e.license)?;
    let previous = installed(appearance, &e.id);
    if previous.as_ref().is_some_and(|p| p.commit.eq_ignore_ascii_case(&e.commit)) {
        return Err(tr!("{} is already installed at this version ({})", e.id, e.version));
    }
    // next to the destination (the final `rename` does not cross disks)
    std::fs::create_dir_all(appearance).map_err(|err| format!("{}: {err}", appearance.display()))?;
    let tmp = appearance.join(format!(".{}.{}", e.id, std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let result = fetch_and_install(e, trust, previous.as_ref(), appearance, &tmp, confirm);
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

fn fetch_and_install(
    e: &MascotEntry,
    trust: &Trust,
    previous: Option<&MascotInstalled>,
    appearance: &Path,
    tmp: &Path,
    confirm: &mut dyn FnMut(&str) -> bool,
) -> Result<String, String> {
    let repo = tmp.join("repo");
    checkout(&e.id, &e.repo, &e.commit, &repo)?;
    let src = if e.path.is_empty() { repo.clone() } else { repo.join(&e.path) };
    let staged = tmp.join("stage");
    let (sprites, packs) = stage(&e.id, &src, &staged)?;

    // a key may only replace what this same entry put there before; and a key is one
    // mascot, whatever its kind: a sprite `knight` and a PNG `knight` can't live together (the
    // hub would leave one out)
    let owned = |key: &str| previous.is_some_and(|p| p.sprites.iter().chain(&p.packs).any(|k| k == key));
    for key in sprites.iter().chain(&packs) {
        let as_sprite = format!("{}/{key}.toml", sprite_file::DIR);
        let as_pack = format!("{}/{key}", pack::DIR);
        for taken in [as_sprite, as_pack] {
            if appearance.join(&taken).exists() && !owned(key) {
                return Err(tr!("{}: {} already exists (yours or from another entry): rename or delete it first", e.id, taken));
            }
        }
    }

    let mut summary = format!("\n{} {}  ({})\n  {}\n  ", e.name, e.version, e.id, e.description);
    summary += &tr!("author: {}   license: {}", if e.author.is_empty() { "?" } else { &e.author }, e.license);
    summary += "\n  ";
    summary += &tr!("repository: {}", e.repo);
    summary += "\n  commit: ";
    summary += &e.commit;
    summary += "\n  ";
    summary += &tr!(
        "index: {}",
        if *trust == Trust::Signed { tr!("signature checked") } else { tr!("local, signature not checked") }
    );
    summary += "\n  ";
    if let Some(p) = previous {
        summary += &tr!("update from {} to {}", p.version, e.version);
        summary += "\n  ";
    }
    summary += &contents(&sprites, &packs);
    summary += "\n  ";
    summary += tr!("only drawings: no code and no permissions");
    summary += "\n";
    if !confirm(&summary) {
        return Err(tr!("installation cancelled").into());
    }

    // before touching the folder, the receipt lists the old and the new files together (at
    // the old version): if a move fails halfway, a retry still owns them and Remove still
    // deletes them
    let union = |old: Option<&Vec<String>>, new: &[String]| -> Vec<String> {
        let mut v: Vec<String> = old.cloned().unwrap_or_default();
        v.extend(new.iter().filter(|k| !v.contains(k)).cloned().collect::<Vec<_>>());
        v
    };
    let pending = MascotInstalled {
        id: e.id.clone(),
        version: previous.map(|p| p.version.clone()).unwrap_or_default(),
        repo: e.repo.clone(),
        commit: previous.map(|p| p.commit.clone()).unwrap_or_default(),
        sprites: union(previous.map(|p| &p.sprites), &sprites),
        packs: union(previous.map(|p| &p.packs), &packs),
    };
    write_receipt(appearance, &pending)?;

    // out with the previous version's files, in with the new ones
    if let Some(p) = previous {
        remove_files(appearance, p)?;
    }
    for key in &sprites {
        let name = format!("{key}.toml");
        let to = appearance.join(sprite_file::DIR).join(&name);
        std::fs::create_dir_all(appearance.join(sprite_file::DIR)).map_err(|err| err.to_string())?;
        std::fs::rename(staged.join(sprite_file::DIR).join(&name), &to).map_err(|err| format!("{}: {err}", to.display()))?;
    }
    for key in &packs {
        let to = appearance.join(pack::DIR).join(key);
        std::fs::create_dir_all(appearance.join(pack::DIR)).map_err(|err| err.to_string())?;
        std::fs::rename(staged.join(pack::DIR).join(key), &to).map_err(|err| format!("{}: {err}", to.display()))?;
    }
    let done = MascotInstalled {
        id: e.id.clone(),
        version: e.version.clone(),
        repo: e.repo.clone(),
        commit: e.commit.clone(),
        sprites,
        packs,
    };
    write_receipt(appearance, &done)?;
    Ok(tr!("{} installed ({})", e.name, done.contents()))
}

fn write_receipt(appearance: &Path, r: &MascotInstalled) -> Result<(), String> {
    let path = receipt(appearance, &r.id);
    std::fs::create_dir_all(appearance.join(RECEIPTS)).map_err(|err| err.to_string())?;
    let json = serde_json::to_string_pretty(r).map_err(|err| err.to_string())?;
    std::fs::write(&path, json).map_err(|err| format!("{}: {err}", path.display()))
}

/// Deletes the files a receipt lists. The keys are checked again: a receipt edited by hand
/// can't point outside the folders.
fn remove_files(appearance: &Path, r: &MascotInstalled) -> Result<(), String> {
    for key in &r.sprites {
        sprite_file::check_key(key)?;
        let path = appearance.join(sprite_file::DIR).join(format!("{key}.toml"));
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    for key in &r.packs {
        sprite_file::check_key(key)?;
        let path = appearance.join(pack::DIR).join(key);
        if path.exists() {
            std::fs::remove_dir_all(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// Removes what entry `id` installed in `appearance`.
pub fn remove(appearance: &Path, id: &str) -> Result<String, String> {
    plugins::validate_id(id)?;
    let r = installed(appearance, id).ok_or_else(|| tr!("\"{}\" is not installed", id))?;
    remove_files(appearance, &r)?;
    let path = receipt(appearance, id);
    std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(tr!("{} removed ({})", id, r.contents()))
}
