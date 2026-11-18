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

/// Words of the `Exec` line of the entry `text` (the program first), undoing the escapes of
/// the Desktop Entry specification (see `quote_exec`). Empty without an `Exec` line.
pub fn command_words(text: &str) -> Vec<String> {
    let Some(value) = text.lines().find_map(|l| l.trim_start().strip_prefix("Exec=")) else { return Vec::new() };
    // 1st level: escapes of any string value (`\\`, `\s`…)
    let mut unescaped = String::new();
    let mut chars = value.trim().chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            unescaped.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => unescaped.push(' '),
            Some('n') => unescaped.push('\n'),
            Some('t') => unescaped.push('\t'),
            Some('r') => unescaped.push('\r'),
            Some(c) => unescaped.push(c),
            None => {}
        }
    }
    // 2nd level: the Exec quoting (`"…"` with `\` before `"`, `` ` ``, `$`, `\`) and `%%`
    let mut words = Vec::new();
    let mut word = String::new();
    let (mut quoted, mut any) = (false, false);
    let mut chars = unescaped.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            '\\' if quoted => word.extend(chars.next()),
            '%' if chars.peek() == Some(&'%') => {
                chars.next();
                word.push('%');
                any = true;
            }
            ' ' if !quoted => {
                if any {
                    words.push(std::mem::take(&mut word));
                }
                any = false;
            }
            c => {
                word.push(c);
                any = true;
            }
        }
    }
    if any {
        words.push(word);
    }
    words
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
    fn exec_words_undo_the_quoting() {
        for path in ["/home/x/bin/miniutils", "/opt/mini utils/miniutils", "/a/$b/100%", "/a\\b", "/q\"u`o"] {
            let words = command_words(&entry_for(Path::new(path)));
            assert_eq!(words, vec![path.to_string(), "--hidden".to_string()], "{path}");
        }
        assert!(command_words("[Desktop Entry]\nName=x\n").is_empty());
    }

    #[test]
    fn an_edited_exec_for_the_same_binary_is_kept() {
        let exe = Path::new("/home/x/bin/miniutils");
        let edited = "[Desktop Entry]\nExec=env MINIUTILS_NATIVE_WAYLAND=1 \"/home/x/bin/miniutils\" --hidden --tab X\nIcon=y\n";
        assert!(super::super::runs(&command_words(edited), exe));
        let other = "[Desktop Entry]\nExec=\"/opt/old/miniutils\" --hidden\n";
        assert!(!super::super::runs(&command_words(other), exe));
    }

    #[test]
    fn file_opens_hidden() {
        let e = entry_for(Path::new("/home/x/bin/miniutils"));
        assert!(e.contains("\nExec=\"/home/x/bin/miniutils\" --hidden\n"));
        assert!(e.starts_with("[Desktop Entry]\n"));
    }
}
