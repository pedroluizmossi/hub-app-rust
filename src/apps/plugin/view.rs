//! Interpreted screen of plugins: `view.slint` compiled by `slint-interpreter`, with the
//! allowed imports (`miniutils.slint`, `std-widgets.slint`, the plugin's `ui/`) and images
//! only from `assets/`. See the docs of `apps/plugin`.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::task::{Context, Poll};

use slint_interpreter::{ComponentDefinition, Value};

use crate::core::plugins;
use crate::mascot::themes::Theme;
use crate::mascot::render;
use crate::tr;

/// The only file a plugin's screen may import (besides `std-widgets.slint`).
const HOST_SLINT: &str = r#"
// Types MiniUtils offers to plugin screens: import { Theme } from "miniutils.slint";
export struct Theme {
    fur: color, visor: color, outline: color, ear: color, blush: color,
    ok: color, warn: color, alarm: color, sleep: color,
    bg0: color, bg1: color, text: color, sub: color, muted: color,
    // fonts of the window look (the screen's text already uses font-ui by default)
    font-ui: string, font-mono: string, font-title: string,
    // text size chosen in Settings (1 = normal): font-size: 13px * theme.text-scale
    text-scale: float,
}
"#;

/// Minimal executor: compilation only "waits" if the file loader waits, and ours answers
/// right away.
fn block_on<F: Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    let mut cx = Context::from_waker(std::task::Waker::noop());
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

/// Compiles the plugin's screen. Imports: only `std-widgets.slint` (built in) and
/// `miniutils.slint`.
///
/// `Main` becomes a `Window`, which is what the `ComponentContainer` embeds:
/// - background: the container paints its window's background behind the screen, and a
///   `Main` that isn't a `Window` gets from Slint a window with the style's background
///   (gray), which ignores the theme. So the window gets a transparent background (the
///   cell, with the theme colors, shows through);
/// - font: the window uses the look's interface font (`default-font-family`, bound to a
///   `miniutils-font` property the host fills in).
///
/// The host tries from the most complete version down to the original: if the plugin
/// already sets its own background or font, the repeated property doesn't compile and the
/// next version is used; if it uses something only a rectangle has (border), the original
/// stays. When the complete version fails for any other reason, the screen has a real
/// error: the original is compiled once more for its diagnostics (lines as in the file)
/// and the other versions are skipped (a broken screen costs two compiles, not five).
pub(super) fn compile_view(dir: &Path, source: String) -> Result<ComponentDefinition, String> {
    // @tr("…") in the interface language (the interpreter has no bundled translations)
    let source = crate::core::i18n::Catalog::of_app(dir, crate::core::i18n::current()).localize_slint(&source);
    check_images(dir, &dir.join("view.slint"), &source)?;
    let window = |transparent: bool, font: bool| as_window(&source, transparent, font).map(|c| build_view(dir, c));
    let rest = || [(true, false), (false, true), (false, false)].into_iter().find_map(|(t, f)| window(t, f)?.ok());
    match window(true, true) {
        Some(Ok(def)) => Ok(def),
        Some(Err(e)) if only_window_errors(&e) => rest().map_or_else(|| build_view(dir, source.clone()), Ok),
        Some(Err(_)) => {
            let original = build_view(dir, source.clone())?;
            Ok(rest().unwrap_or(original))
        }
        // no `export component Main`: the original says so
        None => build_view(dir, source.clone()),
    }
}

/// The errors only come from turning `Main` into a window: a property the plugin also
/// sets (background, font) or one a window doesn't have (border).
fn only_window_errors(errors: &str) -> bool {
    errors.lines().all(|l| l.ends_with("Duplicated property binding") || l.contains(" in Window"))
}

/// Property that carries the look's font to the window of the plugin's screen.
pub(super) const FONT_PROP: &str = "miniutils-font";

/// `source` with `Main` inheriting from `Window`, with a transparent background and/or the
/// look's font.
fn as_window(source: &str, transparent: bool, font: bool) -> Option<String> {
    let start = find_main(source)?;
    let brace = start + source[start..].find('{')?;
    let header = &source[start..brace];
    let header = match header.find(" inherits ") {
        Some(i) => format!("{} inherits Window ", &header[..i]),
        None => format!("{} inherits Window ", header.trim_end()),
    };
    let mut extra = String::new();
    if transparent {
        extra.push_str("\n    background: transparent;");
    }
    if font {
        extra.push_str(&format!("\n    in property <string> {FONT_PROP};\n    default-font-family: {FONT_PROP};"));
    }
    Some(format!("{}{header}{{{extra}{}", &source[..start], &source[brace + 1..]))
}

/// Where the `export component Main` header starts: the whole name (not `MainRow`), and
/// not inside a `//` comment.
fn find_main(source: &str) -> Option<usize> {
    const HEADER: &str = "export component Main";
    source.match_indices(HEADER).map(|(i, _)| i).find(|&i| {
        let whole = source[i + HEADER.len()..].chars().next().is_some_and(|c| c.is_whitespace() || c == '{');
        let line = &source[source[..i].rfind('\n').map_or(0, |n| n + 1)..i];
        whole && !line.contains("//")
    })
}

/// Path without `.` or `..` (without touching the disk: doesn't follow links).
fn normalize(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// An `import` of the screen: only the plugin's own `ui/*.slint` (a regular file, up to 1 MiB).
fn local_import(dir: &Path, path: &Path) -> Result<String, String> {
    let p = normalize(path);
    let shown = path.display();
    let inside = p.parent() == Some(normalize(&dir.join("ui")).as_path()) && p.extension().is_some_and(|e| e == "slint");
    if !inside {
        return Err(tr!("a plugin screen can only import std-widgets.slint, miniutils.slint and ui/*.slint ({})", shown));
    }
    let meta = std::fs::symlink_metadata(&p).map_err(|e| format!("{shown}: {e}"))?;
    if !meta.file_type().is_file() || meta.len() > plugins::MAX_FILE {
        return Err(tr!("{}: must be a regular file of up to 1 MiB", shown));
    }
    let text = std::fs::read_to_string(&p).map_err(|e| format!("{shown}: {e}"))?;
    let text = crate::core::i18n::Catalog::of_app(dir, crate::core::i18n::current()).localize_slint(&text);
    check_images(dir, &p, &text)?;
    Ok(text)
}

/// Every `@image-url("…")` of `file` points into the plugin's `assets/` (no absolute path,
/// nothing outside the folder).
fn check_images(dir: &Path, file: &Path, source: &str) -> Result<(), String> {
    let assets = normalize(&dir.join("assets"));
    let base = file.parent().unwrap_or(dir);
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    for (i, _) in source.match_indices("@image-url(") {
        let rest = source[i + "@image-url(".len()..].trim_start();
        let Some(rest) = rest.strip_prefix('"') else { continue };
        let Some(end) = rest.find('"') else { continue };
        let lit = &rest[..end];
        let target = normalize(&base.join(lit));
        if Path::new(lit).is_absolute() || target.parent() != Some(assets.as_path()) {
            return Err(tr!("{}: @image-url(\"{}\"): images go in the plugin's assets/", name, lit));
        }
    }
    Ok(())
}

fn build_view(dir: &Path, source: String) -> Result<ComponentDefinition, String> {
    let mut compiler = slint_interpreter::Compiler::default();
    let base = dir.to_path_buf();
    compiler.set_file_loader(move |path: &Path| {
        let result = if path.file_name().is_some_and(|n| n == "miniutils.slint") {
            Ok(HOST_SLINT.to_string())
        } else {
            local_import(&base, path).map_err(|e| std::io::Error::new(std::io::ErrorKind::PermissionDenied, e))
        };
        Box::pin(async move { Some(result) })
    });
    let result = block_on(compiler.build_from_source(source, dir.join("view.slint")));
    let errors: Vec<String> = result
        .diagnostics()
        .filter(|d| d.level() == slint_interpreter::DiagnosticLevel::Error)
        .map(|d| {
            let (line, col) = d.line_column();
            // error in a ui/ file: the path relative to the plugin folder
            let file = d.source_file().map_or("view.slint".into(), |p| p.strip_prefix(dir).unwrap_or(p).display().to_string());
            format!("{file}:{line}:{col}: {}", d.message())
        })
        .collect();
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    result.component("Main").ok_or_else(|| tr!("view.slint must export `component Main`").to_string())
}

/// Checks an external app's screen before installing (the store calls this).
pub fn check_view(dir: &Path) -> Result<(), String> {
    let view = std::fs::read_to_string(dir.join("view.slint")).map_err(|e| format!("view.slint: {e}"))?;
    compile_view(dir, view).map(|_| ())
}

/// The `Theme` of the plugin's screen: colors of the app's theme and fonts of the window look.
pub(super) fn theme_value(t: &Theme, skin: &crate::skin::Skin, text_scale: f32) -> Value {
    let c = |v: u32| Value::Brush(slint::Brush::SolidColor(render::color(v)));
    let fields = [
        ("fur", t.fur),
        ("visor", t.visor),
        ("outline", t.outline),
        ("ear", t.ear),
        ("blush", t.blush),
        ("ok", t.ok),
        ("warn", t.warn),
        ("alarm", t.alarm),
        ("sleep", t.sleep),
        ("bg0", t.bg0),
        ("bg1", t.bg1),
        ("text", t.text),
        ("sub", t.sub),
        ("muted", t.muted),
    ];
    let fonts = [("font-ui", skin.font_ui), ("font-mono", skin.font_mono), ("font-title", skin.font_title)];
    Value::Struct(
        fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), c(v)))
            .chain(fonts.into_iter().map(|(k, f)| (k.to_string(), Value::String(f.into()))))
            .chain(std::iter::once(("text-scale".to_string(), Value::Number(text_scale as f64))))
            .collect(),
    )
}


#[cfg(test)]
mod tests {
    use super::*;
    use slint::ComponentHandle;
    use std::rc::Rc;

    #[test]
    fn example_external_app_screen_compiles() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/app-hello");
        check_view(&dir).unwrap();
    }

    #[test]
    fn screen_in_several_files_and_images_only_from_assets() {
        let dir = std::env::temp_dir().join(format!("miniutils-view-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("ui")).unwrap();
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(
            dir.join("ui/bar.slint"),
            "export component Bar inherits Rectangle { in property <float> v; Image { source: @image-url(\"../assets/x.png\"); } }",
        )
        .unwrap();
        let view = "import { Bar } from \"ui/bar.slint\";\nexport component Main inherits Rectangle { in property <float> cpu; Bar { v: cpu; } }";
        let def = compile_view(&dir, view.into()).unwrap();
        assert!(def.properties().any(|(n, _)| n == "cpu"));
        // importing from outside ui/: refused
        std::fs::write(dir.join("outside.slint"), "export component Bar inherits Rectangle {}").unwrap();
        let outside = view.replace("ui/bar.slint", "outside.slint");
        assert!(compile_view(&dir, outside).is_err());
        // an image outside assets/ (in ui/ or in the view): refused
        std::fs::write(dir.join("ui/bar.slint"), "export component Bar inherits Rectangle { Image { source: @image-url(\"/etc/x.png\"); } }").unwrap();
        assert!(compile_view(&dir, view.into()).err().unwrap_or_default().contains("assets/"));
        let img = "export component Main inherits Rectangle { Image { source: @image-url(\"../secret.png\"); } }";
        assert!(compile_view(&dir, img.into()).err().unwrap_or_default().contains("assets/"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn screen_becomes_a_transparent_window_unless_it_has_background_or_border() {
        let dir = Path::new("/tmp");
        let plain = "export component Main inherits Rectangle { in property <string> heading; }";
        assert_eq!(
            as_window(plain, true, false).unwrap(),
            "export component Main inherits Window {\n    background: transparent; in property <string> heading; }"
        );
        assert!(build_view(dir, as_window(plain, true, true).unwrap()).is_ok());
        let def = compile_view(dir, plain.into()).unwrap();
        assert!(def.properties().any(|(n, _)| n == FONT_PROP), "the complete version (with the font) is used");
        // its own background: the transparent one repeats the property; the window with its background is used
        let with_bg = "export component Main inherits Rectangle { background: red; }";
        assert!(build_view(dir, as_window(with_bg, true, true).unwrap()).is_err());
        assert!(build_view(dir, as_window(with_bg, false, true).unwrap()).is_ok());
        // its own font: the transparent window without the look's font is used
        let font = "export component Main inherits Rectangle { default-font-family: \"Serif\"; }";
        let def = compile_view(dir, font.into()).unwrap();
        assert!(!def.properties().any(|(n, _)| n == FONT_PROP));
        // border only exists on a rectangle: the original stays
        let border = "export component Main inherits Rectangle { border-width: 1px; }";
        assert!(build_view(dir, as_window(border, false, false).unwrap()).is_err());
        assert!(compile_view(dir, border.into()).is_ok());
        // another export whose name starts with Main (and a comment that mentions it): only
        // the real Main becomes the window
        let row = "// export component Main is below\nexport component MainRow inherits Rectangle { }\nexport component Main inherits Rectangle { MainRow { } }";
        let window = as_window(row, true, false).unwrap();
        assert!(window.contains("export component MainRow inherits Rectangle { }"), "{window}");
        assert!(window.contains("export component Main inherits Window {\n    background: transparent;"), "{window}");
        assert!(compile_view(dir, row.into()).unwrap().properties().any(|(n, _)| n == FONT_PROP));
    }

    #[test]
    fn broken_screen_reports_the_lines_of_the_file() {
        let broken = "export component Main inherits Rectangle {\n    in property <int> n;\n    n: = 3;\n}";
        let e = compile_view(Path::new("/tmp"), broken.into()).err().unwrap_or_default();
        assert!(e.starts_with("view.slint:3:"), "{e}");
    }

    /// Draws the example screen with the software renderer (the same as the app's): the
    /// background must come out transparent, so the cell (with the theme colors) shows behind it.
    #[test]
    fn plugin_screen_has_a_transparent_background() {
        use slint::platform::software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType};
        use slint::platform::{Platform, WindowAdapter};
        struct P(Rc<MinimalSoftwareWindow>);
        impl Platform for P {
            fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
                Ok(self.0.clone())
            }
        }
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        // only this test creates a window; if another one already set a platform, it can't be checked
        if slint::platform::set_platform(Box::new(P(window.clone()))).is_err() {
            return;
        }
        let (w, h) = (300, 200);
        window.set_size(slint::PhysicalSize::new(w, h));
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/plugin-top"));
        let def = compile_view(dir, std::fs::read_to_string(dir.join("view.slint")).unwrap()).unwrap();
        let inst = def.create().unwrap();
        inst.show().unwrap();
        let skin = crate::skin::get("aconchego");
        inst.set_property("theme", theme_value(crate::mascot::themes::get("aconchego"), skin, 1.0)).unwrap();
        inst.set_property(FONT_PROP, Value::String(skin.font_ui.into())).unwrap();
        // the fonts arrive in the Theme (the plugin can use theme.font-mono)
        let font = match inst.get_property("theme").unwrap() {
            Value::Struct(st) => st.get_field("font-mono").cloned(),
            _ => None,
        };
        assert_eq!(font, Some(Value::String(skin.font_mono.into())));
        let mut px = vec![PremultipliedRgbaColor::default(); (w * h) as usize];
        window.draw_if_needed(|r| {
            r.render(&mut px, w as usize);
        });
        let corner = px[((h - 1) * w + 1) as usize];
        assert_eq!(corner.alpha, 0, "the plugin screen background is not transparent: {corner:?}");
        // and the theme text was drawn (some opaque pixel)
        assert!(px.iter().any(|p| p.alpha == 255));
    }

    #[test]
    fn screen_only_imports_what_is_allowed() {
        let dir = Path::new("/tmp");
        let ok = "import { Theme } from \"miniutils.slint\";\nexport component Main inherits Rectangle { in property <Theme> theme; in property <string> heading; callback action(string, string); }";
        assert!(compile_view(dir, ok.into()).is_ok());
        let outside = "import { X } from \"/etc/passwd\";\nexport component Main inherits Rectangle {}";
        assert!(compile_view(dir, outside.into()).is_err());
        let no_main = "export component Other inherits Rectangle {}";
        assert!(compile_view(dir, no_main.into()).err().unwrap_or_default().contains("component Main"));
    }
}
