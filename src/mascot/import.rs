//! Brings mascots in from a folder or a `.zip` (the Mascots app's "Import"; what a person
//! downloads): every PNG sprite (`pack::import_sheets`) and every code mascot (`.toml`
//! checked by `file.rs`) found inside, a few levels down. Nothing is overwritten: a key that
//! already exists is reported and left alone.
//!
//! A zip is opened into a temporary folder next to the appearance folder, and only `.png`
//! and `.toml` files come out of it, with limits on size and count; a path that would land
//! outside that folder (`../`) is skipped.

use std::io::Read;
use std::path::{Path, PathBuf};

use super::pack::Imported;
use super::{file, pack};
use crate::tr;

/// Limits of a zip: files taken out, size of each and in total.
const ZIP_MAX_FILES: usize = 2000;
const ZIP_MAX_FILE: u64 = 8 << 20;
const ZIP_MAX_TOTAL: u64 = 256 << 20;

/// Imports the mascots in `src` (folder or `.zip`) into `appearance` (`skin::user::dir`).
/// Returns one line per mascot (shown to the user) and how many came in.
pub fn import(src: &Path, appearance: &Path) -> Result<Imported, String> {
    if src.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
        std::fs::create_dir_all(appearance).map_err(|e| format!("{}: {e}", appearance.display()))?;
        remove_leftovers(appearance);
        let tmp = appearance.join(format!(".import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        // made even for an empty zip: "no mascot found", not a missing folder
        let result = std::fs::create_dir_all(&tmp)
            .map_err(|e| format!("{}: {e}", tmp.display()))
            .and_then(|()| unzip(src, &tmp)).and_then(|()| import_folder(&tmp, appearance, src));
        let _ = std::fs::remove_dir_all(&tmp);
        return result;
    }
    if !src.is_dir() {
        return Err(tr!("{}: choose a folder or a .zip", src.display()));
    }
    import_folder(src, appearance, src)
}

/// Removes the zip folders (`.import-<pid>`) an import that crashed left behind, and the
/// half-made mascots (`mascots/.<key>.partial`) of a copy that never finished.
fn remove_leftovers(appearance: &Path) {
    let hidden = |dir: &Path, prefix: &str, suffix: &str| -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
        entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter(|e| e.file_name().to_str().is_some_and(|n| n.starts_with(prefix) && n.ends_with(suffix)))
            .map(|e| e.path())
            .collect()
    };
    let mut old = hidden(appearance, ".import-", "");
    old.extend(hidden(&appearance.join(pack::DIR), ".", ".partial"));
    for dir in old {
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => log::info!("removed the leftover import folder {}", dir.display()),
            Err(e) => log::warn!("could not remove the leftover import folder {}: {e}", dir.display()),
        }
    }
}

/// `shown` is what the messages call the source (the zip, not its temporary folder).
fn import_folder(src: &Path, appearance: &Path, shown: &Path) -> Result<Imported, String> {
    let mut out = pack::import_sheets(src, &appearance.join(pack::DIR))?;
    out.extend(import_code(src, appearance));
    if out.lines.is_empty() {
        return Err(tr!("{}: no mascot found (PNG sheets named like Name_Animation_N.png, or a mascot .toml)", shown.display()));
    }
    Ok(out)
}

/// The code mascots (`<name>.toml` with a `grid`) under `src`, copied to `sprites/`.
fn import_code(src: &Path, appearance: &Path) -> Imported {
    let mut out = Imported::default();
    let dest_dir = appearance.join(file::DIR);
    for dir in pack::folders(src) {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "toml") && p.file_name().is_some_and(|n| n != "sprite.toml"))
            .collect();
        files.sort();
        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            // a .toml that isn't a mascot (a theme, a config) is none of our business
            if !text.lines().any(|l| l.trim_start().starts_with("grid")) {
                continue;
            }
            let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let key: String = stem.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '-' }).collect();
            let checked = file::check_key(&key)
                .and_then(|()| if text.len() as u64 > file::MAX_BYTES { Err(tr!("file too large (64 KiB at most)").into()) } else { Ok(()) })
                .and_then(|()| file::parse(&key, &text));
            let line = match checked {
                Err(e) => tr!("{}: not imported: {}", stem, e),
                Ok(f) => {
                    let to = dest_dir.join(format!("{key}.toml"));
                    if to.exists() || appearance.join(pack::DIR).join(&key).exists() {
                        tr!("{}: already exists in {} (delete the folder to import it again)", key, to.display())
                    } else {
                        match std::fs::create_dir_all(&dest_dir).and_then(|()| std::fs::write(&to, &text)) {
                            Ok(()) => {
                                out.count += 1;
                                tr!("{}: {} (code mascot)", key, f.sprite.label)
                            }
                            Err(e) => format!("{}: {e}", to.display()),
                        }
                    }
                }
            };
            out.lines.push(line);
        }
    }
    out
}

/// Opens `zip` into `to`: only `.png` and `.toml`, within the limits.
fn unzip(zip: &Path, to: &Path) -> Result<(), String> {
    let f = std::fs::File::open(zip).map_err(|e| format!("{}: {e}", zip.display()))?;
    let mut archive = zip::ZipArchive::new(f).map_err(|e| tr!("{}: not a zip file I can read ({})", zip.display(), e))?;
    let (mut files, mut total) = (0usize, 0u64);
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("{}: {e}", zip.display()))?;
        // `../` or an absolute path: skipped (it would land outside the folder)
        let Some(rel) = entry.enclosed_name() else { continue };
        let name = rel.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        if entry.is_dir() || name.starts_with('.') || !(name.ends_with(".png") || name.ends_with(".toml")) {
            continue;
        }
        let too_big = || tr!("{}: too big to import (more than {} files or {} MiB)", zip.display(), ZIP_MAX_FILES, ZIP_MAX_TOTAL >> 20);
        files += 1;
        if files > ZIP_MAX_FILES || entry.size() > ZIP_MAX_FILE {
            return Err(too_big());
        }
        let path = to.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let mut out = std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        // the size the zip declares may lie: what counts is what came out, at most the
        // limit per file (one byte more tells that it was bigger), and the sum of it all
        let written = std::io::copy(&mut (&mut entry).take(ZIP_MAX_FILE + 1), &mut out).map_err(|e| format!("{}: {e}", path.display()))?;
        total += written;
        if written > ZIP_MAX_FILE || total > ZIP_MAX_TOTAL {
            return Err(too_big());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("miniutils-import-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A PNG sheet: `n` frames of 16×16 with a block in each.
    fn sheet(n: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(16 * n, 16, |x, y| {
            if (4..12).contains(&(x % 16)) && (4..12).contains(&y) { image::Rgba([40, 90, 200, 255]) } else { image::Rgba([0, 0, 0, 0]) }
        });
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    /// A downloaded pack, zipped: nested folders with two characters' sheets, a code mascot,
    /// Mac junk, a file that isn't a mascot and a path that tries to leave the folder.
    #[test]
    fn imports_both_kinds_from_a_zip_and_nothing_else() {
        let d = temp("zip");
        let zip_path = d.join("Pack.zip");
        let mut z = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in [
            ("Pack/Characters/Knight/Knight_Idle_2.png", sheet(2)),
            ("Pack/Characters/Knight/Knight_Walk_2.png", sheet(2)),
            // no frame count in the name: square frames (Tiny RPG packs)
            ("Pack/Characters/Orc/Orc_Idle.png", sheet(3)),
            ("Pack/Characters/Orc/Orc_Walk.png", sheet(2)),
            ("Pack/Characters/Orc/Orc.png", sheet(4)),
            ("__MACOSX/Pack/._Knight_Idle_2.png", b"junk".to_vec()),
            ("Pack/readme.txt", b"hello".to_vec()),
            ("../evil.png", sheet(1)),
            ("Pack/Owl.toml", include_bytes!("../../examples/appearance/sprites/jellyfish.toml").to_vec()),
        ] {
            z.start_file(name, opts).unwrap();
            z.write_all(&data).unwrap();
        }
        z.finish().unwrap();

        let appearance = d.join("appearance");
        let first = import(&zip_path, &appearance).unwrap();
        assert_eq!((first.count, first.lines.len()), (3, 3), "{first:?}");
        assert!(appearance.join("mascots/orc/Orc_Walk.png").is_file());
        assert!(appearance.join("mascots/knight/Knight_Walk_2.png").is_file());
        assert!(appearance.join("mascots/knight/sprite.toml").is_file());
        assert!(appearance.join("sprites/owl.toml").is_file());
        assert!(!d.join("evil.png").exists() && !appearance.join("evil.png").exists());
        // the temporary folder is gone, and the hub reads what came in
        assert!(std::fs::read_dir(&appearance).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with(".import")));
        let (arts, skipped) = pack::load(&appearance.join(pack::DIR));
        assert!(skipped.is_empty() && arts.len() == 2, "{skipped:?}");
        // again: nothing is overwritten
        let again = import(&zip_path, &appearance).unwrap();
        assert_eq!(again.count, 0, "{again:?}");
        assert!(again.lines.iter().all(|l| l.contains("already exists")), "{again:?}");
        assert_eq!(again.summary(), "0 imported · 3 skipped");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A PNG pack whose key a code mascot already has is refused: it would hide it.
    #[test]
    fn png_pack_does_not_hide_a_code_mascot() {
        let d = temp("taken");
        let appearance = d.join("appearance");
        std::fs::create_dir_all(appearance.join(file::DIR)).unwrap();
        let owl = include_str!("../../examples/appearance/sprites/jellyfish.toml");
        std::fs::write(appearance.join(file::DIR).join("owl.toml"), owl).unwrap();
        let src = d.join("src/Owl");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("Owl_Idle_2.png"), sheet(2)).unwrap();
        std::fs::write(src.join("Owl_Walk_2.png"), sheet(2)).unwrap();
        let out = import(&d.join("src"), &appearance).unwrap();
        assert_eq!(out.count, 0, "{out:?}");
        assert!(out.lines.iter().any(|l| l.starts_with("owl:") && l.contains("already exists")), "{out:?}");
        assert!(!appearance.join(pack::DIR).join("owl").exists());
        let (sprites, skipped) = file::load(&appearance.join(file::DIR), &[]);
        assert!(skipped.is_empty() && sprites.iter().any(|f| f.sprite.key == "owl"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A pack whose `sprite.toml` names a sheet that isn't there leaves nothing behind, and
    /// the leftovers of a crashed import go away on the next zip (the copy that fails
    /// halfway is `pack::tests::failed_copy_leaves_nothing`).
    #[test]
    fn failed_import_leaves_no_folder() {
        let d = temp("partial");
        let appearance = d.join("appearance");
        let src = d.join("src/Ghost");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("Ghost_Idle_2.png"), sheet(2)).unwrap();
        std::fs::write(src.join("sprite.toml"), "name = \"Ghost\"\n[anim]\nidle = \"Ghost_Idle_2.png\"\nwalk = \"Ghost_Walk_2.png\"\n").unwrap();
        let out = import(&d.join("src"), &appearance).unwrap();
        assert_eq!(out.count, 0, "{out:?}");
        let mascots = appearance.join(pack::DIR);
        let left: Vec<_> = std::fs::read_dir(&mascots).map(|r| r.flatten().map(|e| e.file_name()).collect()).unwrap_or_default();
        assert!(left.is_empty(), "{left:?}");

        // leftovers of a crash: removed before the next zip opens
        std::fs::create_dir_all(appearance.join(".import-999999")).unwrap();
        std::fs::create_dir_all(mascots.join(".ghost.partial")).unwrap();
        let zip_path = d.join("empty.zip");
        zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap()).finish().unwrap();
        let r = import(&zip_path, &appearance);
        assert!(r.is_err(), "{r:?}");
        assert!(!appearance.join(".import-999999").exists() && !mascots.join(".ghost.partial").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_folder_without_mascots_is_an_error() {
        let d = temp("empty");
        std::fs::write(d.join("notes.toml"), "title = 'not a mascot'").unwrap();
        assert!(import(&d, &d.join("appearance")).unwrap_err().contains("no mascot"));
        assert!(import(&d.join("missing.txt"), &d).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
