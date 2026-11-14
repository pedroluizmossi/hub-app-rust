//! Start with the system on Linux: a `.desktop` file in `~/.config/autostart/`.

use std::path::{Path, PathBuf};

fn file() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("autostart").join("miniutils.desktop"))
}

/// The content of the file, if it exists and is active.
pub fn current() -> Option<String> {
    let text = std::fs::read_to_string(file()?).ok()?;
    let off = text.lines().any(|l| {
        let l = l.trim();
        l == "Hidden=true" || l == "X-GNOME-Autostart-enabled=false"
    });
    (!off).then_some(text)
}

pub fn enable(exe: &Path) -> Result<(), String> {
    let path = file().ok_or(crate::tr!("no configuration folder"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&path, entry_for(exe)).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn disable() -> Result<(), String> {
    let Some(path) = file() else { return Ok(()) };
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("{}: {e}", path.display())),
        _ => Ok(()),
    }
}

/// The `.desktop` entry that opens `exe` in the tray only.
pub fn entry_for(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=MiniUtils\n\
         Comment=Mini-app hub in the tray\n\
         Exec={} --hidden\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n\
         X-KDE-autostart-after=panel\n",
        quote_exec(&exe.to_string_lossy())
    )
}

/// Quoted path for the `Exec` line (Desktop Entry specification): inside the quotes, `"`,
/// `` ` ``, `$` and `\` get a `\` in front; since the file value also treats `\` as an
/// escape, each `\` becomes `\\` again. `%` is `%%` (otherwise it is a field code).
fn quote_exec(path: &str) -> String {
    let mut out = String::from("\"");
    for c in path.chars() {
        match c {
            '"' | '`' | '$' => {
                out.push_str("\\\\");
                out.push(c);
            }
            '\\' => out.push_str("\\\\\\\\"),
            '%' => out.push_str("%%"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_in_quotes() {
        assert_eq!(quote_exec("/opt/mini utils/miniutils"), "\"/opt/mini utils/miniutils\"");
        assert_eq!(quote_exec("/a/$b/100%"), "\"/a/\\\\$b/100%%\"");
        assert_eq!(quote_exec("/a\\b"), "\"/a\\\\\\\\b\"");
    }

    #[test]
    fn file_opens_hidden() {
        let e = entry_for(Path::new("/home/x/bin/miniutils"));
        assert!(e.contains("\nExec=\"/home/x/bin/miniutils\" --hidden\n"));
        assert!(e.starts_with("[Desktop Entry]\n"));
    }
}
