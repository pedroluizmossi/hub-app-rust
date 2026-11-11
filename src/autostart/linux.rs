//! Iniciar com o sistema no Linux: um `.desktop` em `~/.config/autostart/`.

use std::path::{Path, PathBuf};

fn file() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("autostart").join("miniutils.desktop"))
}

/// O conteúdo do arquivo, se existe e está ativo.
pub fn current() -> Option<String> {
    let text = std::fs::read_to_string(file()?).ok()?;
    let off = text.lines().any(|l| {
        let l = l.trim();
        l == "Hidden=true" || l == "X-GNOME-Autostart-enabled=false"
    });
    (!off).then_some(text)
}

pub fn enable(exe: &Path) -> Result<(), String> {
    let path = file().ok_or("sem pasta de configuração")?;
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

/// O `.desktop` que abre `exe` só na bandeja.
pub fn entry_for(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=MiniUtils\n\
         Comment=Hub de mini-apps na bandeja\n\
         Exec={} --hidden\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n\
         X-KDE-autostart-after=panel\n",
        quote_exec(&exe.to_string_lossy())
    )
}

/// Caminho entre aspas para a linha `Exec` (especificação Desktop Entry): dentro das
/// aspas, `"`, `` ` ``, `$` e `\` levam `\` antes; como o valor do arquivo também trata
/// `\` como escape, cada `\` vira `\\` de novo. `%` é `%%` (senão vira código de campo).
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
    fn exec_entre_aspas() {
        assert_eq!(quote_exec("/opt/mini utils/miniutils"), "\"/opt/mini utils/miniutils\"");
        assert_eq!(quote_exec("/a/$b/100%"), "\"/a/\\\\$b/100%%\"");
        assert_eq!(quote_exec("/a\\b"), "\"/a\\\\\\\\b\"");
    }

    #[test]
    fn arquivo_abre_escondido() {
        let e = entry_for(Path::new("/home/x/bin/miniutils"));
        assert!(e.contains("\nExec=\"/home/x/bin/miniutils\" --hidden\n"));
        assert!(e.starts_with("[Desktop Entry]\n"));
    }
}
