//! Tela interpretada dos plugins: o `view.slint` compilado pelo `slint-interpreter`, com
//! os imports permitidos (`miniutils.slint`, `std-widgets.slint`, `ui/` do plugin) e
//! imagens só de `assets/`. Ver a doc de `apps/plugin`.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::task::{Context, Poll};

use slint_interpreter::{ComponentDefinition, Value};

use crate::core::plugins;
use crate::mascot::themes::Theme;
use crate::mascot::render;

/// O único arquivo que a tela de um plugin pode importar (além do `std-widgets.slint`).
const HOST_SLINT: &str = r#"
// Tipos que o MiniUtils oferece às telas dos plugins: import { Theme } from "miniutils.slint";
export struct Theme {
    fur: color, visor: color, outline: color, ear: color, blush: color,
    ok: color, warn: color, alarm: color, sleep: color,
    bg0: color, bg1: color, text: color, sub: color, muted: color,
    // fontes do visual da janela (o texto da tela já usa a font-ui por padrão)
    font-ui: string, font-mono: string, font-title: string,
}
"#;

/// Executor mínimo: a compilação só "espera" se o carregador de arquivo esperar, e o
/// nosso responde na hora.
fn block_on<F: Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    let mut cx = Context::from_waker(std::task::Waker::noop());
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

/// Compila a tela do plugin. Imports: só `std-widgets.slint` (embutido) e `miniutils.slint`.
///
/// O `Main` vira uma `Window`, que é o que o `ComponentContainer` encaixa:
/// - fundo: o container pinta atrás da tela o fundo da janela dela, e um `Main` que não é
///   `Window` ganha do Slint uma janela com o fundo do estilo (cinza), que ignora o tema.
///   Por isso a janela sai de fundo transparente (a célula, com as cores do tema, aparece);
/// - fonte: a janela usa a fonte da interface do visual (`default-font-family`, ligada a
///   uma propriedade `miniutils-font` que o host preenche).
///
/// O host tenta da versão mais completa para a original: se o plugin já define o próprio
/// fundo ou fonte, a propriedade repetida não compila e a próxima versão vale; se usa algo
/// que só um retângulo tem (borda), fica o original.
pub(super) fn compile_view(dir: &Path, source: String) -> Result<ComponentDefinition, String> {
    check_images(dir, &dir.join("view.slint"), &source)?;
    let candidates = [(true, true), (true, false), (false, true), (false, false)];
    for (transparent, font) in candidates {
        if let Some(candidate) = as_window(&source, transparent, font)
            && let Ok(def) = build_view(dir, candidate) {
                return Ok(def);
            }
    }
    build_view(dir, source)
}

/// Propriedade que leva a fonte do visual à janela da tela do plugin.
pub(super) const FONT_PROP: &str = "miniutils-font";

/// `source` com o `Main` herdando de `Window`, de fundo transparente e/ou com a fonte do
/// visual.
fn as_window(source: &str, transparent: bool, font: bool) -> Option<String> {
    let start = source.find("export component Main")?;
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

/// Caminho sem `.` nem `..` (sem tocar o disco: não segue links).
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

/// Um `import` da tela: só `ui/*.slint` do próprio plugin (arquivo comum, até 1 MiB).
fn local_import(dir: &Path, path: &Path) -> Result<String, String> {
    let p = normalize(path);
    let shown = path.display();
    let inside = p.parent() == Some(normalize(&dir.join("ui")).as_path()) && p.extension().is_some_and(|e| e == "slint");
    if !inside {
        return Err(format!("a tela de um plugin só importa std-widgets.slint, miniutils.slint e ui/*.slint ({shown})"));
    }
    let meta = std::fs::symlink_metadata(&p).map_err(|e| format!("{shown}: {e}"))?;
    if !meta.file_type().is_file() || meta.len() > plugins::MAX_FILE {
        return Err(format!("{shown}: precisa ser um arquivo comum de até 1 MiB"));
    }
    let text = std::fs::read_to_string(&p).map_err(|e| format!("{shown}: {e}"))?;
    check_images(dir, &p, &text)?;
    Ok(text)
}

/// Toda `@image-url("…")` de `file` aponta para `assets/` do plugin (nada de caminho
/// absoluto nem de fora da pasta).
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
            return Err(format!("{name}: @image-url(\"{lit}\"): as imagens ficam em assets/ do plugin"));
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
            // erro num arquivo de ui/: o caminho relativo à pasta do plugin
            let file = d.source_file().map_or("view.slint".into(), |p| p.strip_prefix(dir).unwrap_or(p).display().to_string());
            format!("{file}:{line}:{col}: {}", d.message())
        })
        .collect();
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    result.component("Main").ok_or_else(|| "view.slint precisa exportar `component Main`".to_string())
}

/// Confere a tela de um app externo antes de instalar (a loja chama).
pub fn check_view(dir: &Path) -> Result<(), String> {
    let view = std::fs::read_to_string(dir.join("view.slint")).map_err(|e| format!("view.slint: {e}"))?;
    compile_view(dir, view).map(|_| ())
}

/// O `Theme` da tela do plugin: cores do tema do app e fontes do visual da janela.
pub(super) fn theme_value(t: &Theme, skin: &crate::skin::Skin) -> Value {
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
            .collect(),
    )
}


#[cfg(test)]
mod tests {
    use super::*;
    use slint::ComponentHandle;
    use std::rc::Rc;

    #[test]
    fn tela_do_app_externo_de_exemplo_compila() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/app-hello");
        check_view(&dir).unwrap();
    }

    #[test]
    fn tela_em_varios_arquivos_e_imagens_so_de_assets() {
        let dir = std::env::temp_dir().join(format!("miniutils-tela-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("ui")).unwrap();
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(
            dir.join("ui/barra.slint"),
            "export component Barra inherits Rectangle { in property <float> v; Image { source: @image-url(\"../assets/x.png\"); } }",
        )
        .unwrap();
        let view = "import { Barra } from \"ui/barra.slint\";\nexport component Main inherits Rectangle { in property <float> cpu; Barra { v: cpu; } }";
        let def = compile_view(&dir, view.into()).unwrap();
        assert!(def.properties().any(|(n, _)| n == "cpu"));
        // importar fora de ui/: recusa
        std::fs::write(dir.join("fora.slint"), "export component Barra inherits Rectangle {}").unwrap();
        let fora = view.replace("ui/barra.slint", "fora.slint");
        assert!(compile_view(&dir, fora).is_err());
        // imagem fora de assets/ (no ui/ ou na view): recusa
        std::fs::write(dir.join("ui/barra.slint"), "export component Barra inherits Rectangle { Image { source: @image-url(\"/etc/x.png\"); } }").unwrap();
        assert!(compile_view(&dir, view.into()).err().unwrap_or_default().contains("assets/"));
        let img = "export component Main inherits Rectangle { Image { source: @image-url(\"../segredo.png\"); } }";
        assert!(compile_view(&dir, img.into()).err().unwrap_or_default().contains("assets/"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tela_vira_janela_transparente_salvo_se_tiver_fundo_ou_borda() {
        let dir = Path::new("/tmp");
        let sem = "export component Main inherits Rectangle { in property <string> titulo; }";
        assert_eq!(
            as_window(sem, true, false).unwrap(),
            "export component Main inherits Window {\n    background: transparent; in property <string> titulo; }"
        );
        assert!(build_view(dir, as_window(sem, true, true).unwrap()).is_ok());
        let def = compile_view(dir, sem.into()).unwrap();
        assert!(def.properties().any(|(n, _)| n == FONT_PROP), "a versão completa (com a fonte) vale");
        // fundo próprio: a transparente repete a propriedade; vale a janela com o fundo dele
        let com = "export component Main inherits Rectangle { background: red; }";
        assert!(build_view(dir, as_window(com, true, true).unwrap()).is_err());
        assert!(build_view(dir, as_window(com, false, true).unwrap()).is_ok());
        // fonte própria: fica a janela transparente sem a fonte do visual
        let fonte = "export component Main inherits Rectangle { default-font-family: \"Serif\"; }";
        let def = compile_view(dir, fonte.into()).unwrap();
        assert!(!def.properties().any(|(n, _)| n == FONT_PROP));
        // borda só existe em retângulo: fica o original
        let borda = "export component Main inherits Rectangle { border-width: 1px; }";
        assert!(build_view(dir, as_window(borda, false, false).unwrap()).is_err());
        assert!(compile_view(dir, borda.into()).is_ok());
    }

    /// Desenha a tela do exemplo com o renderizador por software (o mesmo do app): o fundo
    /// tem de sair transparente, para a célula (com as cores do tema) aparecer atrás.
    #[test]
    fn tela_do_plugin_tem_fundo_transparente() {
        use slint::platform::software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType};
        use slint::platform::{Platform, WindowAdapter};
        struct P(Rc<MinimalSoftwareWindow>);
        impl Platform for P {
            fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
                Ok(self.0.clone())
            }
        }
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        // só este teste cria janela; se outro já pôs uma plataforma, não dá para conferir
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
        inst.set_property("theme", theme_value(crate::mascot::themes::get("aconchego"), skin)).unwrap();
        inst.set_property(FONT_PROP, Value::String(skin.font_ui.into())).unwrap();
        // as fontes chegam no Theme (o plugin pode usar theme.font-mono)
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
        assert_eq!(corner.alpha, 0, "fundo da tela do plugin não é transparente: {corner:?}");
        // e o texto do tema foi desenhado (algum pixel opaco)
        assert!(px.iter().any(|p| p.alpha == 255));
    }

    #[test]
    fn tela_so_importa_o_permitido() {
        let dir = Path::new("/tmp");
        let ok = "import { Theme } from \"miniutils.slint\";\nexport component Main inherits Rectangle { in property <Theme> theme; in property <string> titulo; callback action(string, string); }";
        assert!(compile_view(dir, ok.into()).is_ok());
        let fora = "import { X } from \"/etc/passwd\";\nexport component Main inherits Rectangle {}";
        assert!(compile_view(dir, fora.into()).is_err());
        let sem_main = "export component Outra inherits Rectangle {}";
        assert!(compile_view(dir, sem_main.into()).err().unwrap_or_default().contains("component Main"));
    }
}
