//! Temas de cor (portados do CyberDock). Cada tema pinta a mascote e o HUD.
//!
//! Os embutidos ficam em `BUILTIN`; os da pessoa vêm de `temas/<chave>.toml` (ver
//! `skin::user`) e entram por `set_extra`. Quem lista ou procura usa `all`/`get`/`find`.

use std::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub key: &'static str,
    pub label: &'static str,
    // mascote
    pub fur: u32,
    pub visor: u32,
    pub outline: u32,
    pub ear: u32,
    pub blush: u32,
    // estados
    pub ok: u32,
    pub warn: u32,
    pub alarm: u32,
    pub sleep: u32,
    // HUD
    pub bg0: u32,
    pub bg1: u32,
    pub text: u32,
    pub sub: u32,
    pub muted: u32,
}

pub static BUILTIN: &[Theme] = &[
    Theme {
        key: "neon",
        label: "Neon (magenta / ciano)",
        fur: 0xff2bd6, visor: 0x00f0ff, outline: 0x12061f, ear: 0xffd1f5, blush: 0xff8fc8,
        ok: 0x39ff14, warn: 0xfff200, alarm: 0xff1744, sleep: 0x7a5cff,
        bg0: 0x14062a, bg1: 0x05020c, text: 0xf2e9ff, sub: 0x8f7bb3, muted: 0x5a4a78,
    },
    Theme {
        key: "matrix",
        label: "Matrix (verde terminal)",
        fur: 0x00c853, visor: 0xb6ff00, outline: 0x001a06, ear: 0xa5ffb0, blush: 0x7dff9b,
        ok: 0x00ff41, warn: 0xe6ff00, alarm: 0xff3b30, sleep: 0x2e7d32,
        bg0: 0x021a08, bg1: 0x000400, text: 0xd7ffd9, sub: 0x5fae6a, muted: 0x2f5e37,
    },
    Theme {
        key: "synthwave",
        label: "Synthwave (roxo / laranja)",
        fur: 0x7b2cff, visor: 0xff8a00, outline: 0x10002a, ear: 0xffb3e6, blush: 0xff6ec7,
        ok: 0x00e5ff, warn: 0xffd600, alarm: 0xff1f5a, sleep: 0x4527a0,
        bg0: 0x1a0633, bg1: 0x0b0016, text: 0xffe8ff, sub: 0xb38bd9, muted: 0x5e3b85,
    },
    Theme {
        key: "vaporwave",
        label: "Vaporwave (pastel)",
        fur: 0xff9ecf, visor: 0x7df9ff, outline: 0x2a1035, ear: 0xffffff, blush: 0xffb6d9,
        ok: 0x8affc1, warn: 0xfff59d, alarm: 0xff5c8a, sleep: 0xb39ddb,
        bg0: 0x2b1b3d, bg1: 0x120a1f, text: 0xfff0fb, sub: 0xc7a8e0, muted: 0x6e5a85,
    },
    Theme {
        key: "akira",
        label: "Akira (vermelho / branco)",
        fur: 0xff2a2a, visor: 0xf5f5f5, outline: 0x120000, ear: 0xffc2c2, blush: 0xff8080,
        ok: 0x00e676, warn: 0xffc400, alarm: 0xffea00, sleep: 0x8e8e8e,
        bg0: 0x1a0505, bg1: 0x050101, text: 0xffecec, sub: 0xc48a8a, muted: 0x6b3a3a,
    },
    Theme {
        key: "ghost",
        label: "Ghost (azul gelo)",
        fur: 0x4fc3f7, visor: 0xe0f7ff, outline: 0x021627, ear: 0xffffff, blush: 0x9be7ff,
        ok: 0x64ffda, warn: 0xffe57f, alarm: 0xff5252, sleep: 0x5c7c99,
        bg0: 0x061a2b, bg1: 0x01070d, text: 0xe8f6ff, sub: 0x7fa7c4, muted: 0x3b5a73,
    },
    // claro: papel e terracota
    Theme {
        key: "aconchego",
        label: "Aconchego (papel / terracota)",
        fur: 0xc0602f, visor: 0x3f6b3a, outline: 0x3e2a1e, ear: 0xf4e6cc, blush: 0xf28ca0,
        ok: 0x3f8a3a, warn: 0xb87514, alarm: 0xb4322a, sleep: 0x8a6e58,
        bg0: 0xfbf1de, bg1: 0xf1dfc0, text: 0x3e2a1e, sub: 0x6f5a4a, muted: 0xb8936a,
    },
    Theme {
        key: "noite",
        label: "Noite (azul / âmbar)",
        fur: 0xff6fb5, visor: 0xffc46b, outline: 0x0c1024, ear: 0xf3ebd0, blush: 0xff9ec8,
        ok: 0x6be3a4, warn: 0xffc46b, alarm: 0xff5c6c, sleep: 0x6b78c4,
        bg0: 0x151b3d, bg1: 0x0c1024, text: 0xe4e8ff, sub: 0xb9c2f0, muted: 0x3b4578,
    },
];

/// Temas lidos de arquivo (depois dos embutidos). Cada recarga vaza os anteriores: são
/// poucos e pequenos (~100 bytes), e as telas guardam `&'static Theme`.
static EXTRA: RwLock<Vec<&'static Theme>> = RwLock::new(Vec::new());

/// Troca os temas de arquivo.
pub fn set_extra(list: Vec<Theme>) {
    let leaked = list.into_iter().map(|t| &*Box::leak(Box::new(t))).collect();
    if let Ok(mut extra) = EXTRA.write() {
        *extra = leaked;
    }
}

/// Todos os temas: os embutidos e, depois, os de arquivo.
pub fn all() -> Vec<&'static Theme> {
    let extra = EXTRA.read().map(|e| e.clone()).unwrap_or_default();
    BUILTIN.iter().chain(extra).collect()
}

pub fn find(key: &str) -> Option<&'static Theme> {
    if let Some(t) = BUILTIN.iter().find(|t| t.key == key) {
        return Some(t);
    }
    EXTRA.read().ok()?.iter().copied().find(|t| t.key == key)
}

/// O tema `key`, ou o primeiro se não existir (um tema de arquivo que sumiu).
pub fn get(key: &str) -> &'static Theme {
    find(key).unwrap_or(&BUILTIN[0])
}

/// Nomes das cores, como no arquivo `temas/<chave>.toml` e na tabela do DESIGN.md.
pub const COLOR_FIELDS: &[&str] =
    &["fur", "visor", "outline", "ear", "blush", "ok", "warn", "alarm", "sleep", "bg0", "bg1", "text", "sub", "muted"];

impl Theme {
    /// A cor de nome `name` (um de `COLOR_FIELDS`), para o leitor dos arquivos.
    pub fn color_mut(&mut self, name: &str) -> Option<&mut u32> {
        Some(match name {
            "fur" => &mut self.fur,
            "visor" => &mut self.visor,
            "outline" => &mut self.outline,
            "ear" => &mut self.ear,
            "blush" => &mut self.blush,
            "ok" => &mut self.ok,
            "warn" => &mut self.warn,
            "alarm" => &mut self.alarm,
            "sleep" => &mut self.sleep,
            "bg0" => &mut self.bg0,
            "bg1" => &mut self.bg1,
            "text" => &mut self.text,
            "sub" => &mut self.sub,
            "muted" => &mut self.muted,
            _ => return None,
        })
    }
}

