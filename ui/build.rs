use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    // ComponentContainer (embedding a plugin's view, interpreted at run time) is still an
    // experimental Slint 1.18 feature
    // SAFETY: the build script is single-threaded; nothing reads the environment meanwhile
    unsafe { std::env::set_var("SLINT_ENABLE_EXPERIMENTAL_FEATURES", "1") };
    // live-preview: the generator reads this variable and, instead of the compiled UI,
    // emits a shell that interprets hub.slint at run time and reloads it on save
    if std::env::var_os("CARGO_FEATURE_LIVE_PREVIEW").is_some() {
        // SAFETY: same as above
        unsafe { std::env::set_var("SLINT_LIVE_PREVIEW", "1") };
    }
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let bundle = out.join("translations");
    merge_catalog(Path::new("../i18n/pt-BR"), &bundle.join("pt-BR/LC_MESSAGES/miniutils-ui.po"));

    // dev-mcp: element ids in the generated code, so the MCP server can find them
    // (`find_elements_by_id`); without the feature, `SLINT_EMIT_DEBUG_INFO` from the
    // environment applies, which the UI tests use
    let mut config = slint_build::CompilerConfiguration::new()
        .with_bundled_translations(&bundle)
        // msgids without context: the same English text has one translation everywhere,
        // shared with the Rust side (`tr!`)
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    if std::env::var_os("CARGO_FEATURE_DEV_MCP").is_some() {
        config = config.with_debug_info(true);
    }
    // hub.slint imports the mini-app views (../miniapps/*/view.slint); slint-build tells
    // cargo about every file it reads, so changing any of them rebuilds this crate.
    slint_build::compile_with_config("hub.slint", config).expect("failed to compile the Slint UI");
}

/// Merges the `.po` fragments of one language (one per area of the code, so parallel
/// work does not collide) into the single catalog Slint bundles. The Rust side reads the
/// same file (`PT_BR_CATALOG` in lib.rs). A msgid translated two different ways fails.
fn merge_catalog(dir: &Path, dest: &Path) {
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "po")).collect())
        .unwrap_or_default();
    files.sort();
    let mut entries: BTreeMap<String, (String, String)> = BTreeMap::new();
    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
        let text = std::fs::read_to_string(f).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
        for (id, s) in entries_of(&text) {
            let name = f.file_name().unwrap().to_string_lossy().into_owned();
            if let Some((prev, from)) = entries.get(&id) {
                assert!(prev == &s, "msgid {id:?} has two translations: {prev:?} ({from}) and {s:?} ({name})");
                continue;
            }
            entries.insert(id, (s, name));
        }
    }
    let mut po = String::from("msgid \"\"\nmsgstr \"\"\n\"Content-Type: text/plain; charset=UTF-8\\n\"\n\"Language: pt_BR\\n\"\n\"Plural-Forms: nplurals=2; plural=(n > 1);\\n\"\n\n");
    for (id, (s, _)) in &entries {
        let _ = writeln!(po, "msgid \"{}\"\nmsgstr \"{}\"\n", escape(id), escape(s));
    }
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::write(dest, po).unwrap();
}

/// (msgid, msgstr) pairs of a .po, unescaped, skipping the header and empty msgstr.
fn entries_of(po: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let (mut id, mut s, mut target) = (None::<String>, None::<String>, 0);
    let flush = |id: &mut Option<String>, s: &mut Option<String>, out: &mut Vec<(String, String)>| {
        if let (Some(i), Some(t)) = (id.take(), s.take())
            && !i.is_empty()
            && !t.is_empty()
        {
            out.push((i, t));
        }
    };
    for line in po.lines().map(str::trim) {
        if let Some(r) = line.strip_prefix("msgid ") {
            flush(&mut id, &mut s, &mut out);
            id = Some(unquote(r));
            target = 1;
        } else if let Some(r) = line.strip_prefix("msgstr ") {
            s = Some(unquote(r));
            target = 2;
        } else if line.starts_with('"') {
            let piece = unquote(line);
            match target {
                1 => id.get_or_insert_default().push_str(&piece),
                2 => s.get_or_insert_default().push_str(&piece),
                _ => {}
            }
        } else if !(line.is_empty() || line.starts_with('#')) {
            target = 0;
        }
    }
    flush(&mut id, &mut s, &mut out);
    out
}

fn unquote(s: &str) -> String {
    let inner = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(s);
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(o) => out.push(o),
            None => {}
        }
    }
    out
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\t', "\\t")
}
