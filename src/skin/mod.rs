//! Visuais da janela ("skins"): fundo, moldura, cabeçalho, fontes e cores da interface.
//!
//! Um visual é só uma tabela de valores; a tela (`ui/hub.slint`) lê tudo do global `Skin`.
//! Os embutidos ficam em `BUILTIN`; os da pessoa vêm de `visuais/<chave>.toml` (ver
//! `user.rs`). O fundo do painel é um dos fundos de `backdrop.rs`, pelo nome. O conteúdo
//! dos apps segue o tema de cada app; com o tema "auto", o app usa o `app_theme` do visual.
//!
//! Por cima do visual, a config pode trocar a cor de destaque (`effective`) e o tamanho do
//! texto (`apply`).

pub mod backdrop;
pub mod color;
pub mod user;

use std::sync::RwLock;

use slint::{Color, ComponentHandle};

use crate::ui::{HubWindow, Skin as SkinGlobal};
use color::mix;

/// Estilo dos botões do cabeçalho.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Buttons {
    /// Ícone de traço com borda fina.
    Icon,
    /// Texto entre colchetes: `[ fixar ]`.
    Bracket,
    /// Bloco claro com sombra dura e ícone em pixel.
    Chunky,
}

/// Como o nome do app aparece no título.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleStyle {
    /// `MINIUTILS`
    Upper,
    /// `MiniUtils`
    Plain,
    /// `miniutils@bandeja:~$`
    Prompt,
}

impl TitleStyle {
    pub fn format(self, name: &str) -> String {
        match self {
            TitleStyle::Upper => name.to_uppercase(),
            TitleStyle::Plain => name.to_string(),
            TitleStyle::Prompt => format!("{}@bandeja:~$", name.to_lowercase().split_whitespace().collect::<Vec<_>>().join("-")),
        }
    }
}

/// O que aparece ao lado do título.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    /// `// 2 APPS · 3 × 2`
    Grid,
    /// `boa noite · 21:57`
    Greeting,
    None,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Skin {
    pub key: &'static str,
    pub label: &'static str,
    /// Tema dos apps que escolheram "Acompanha a janela".
    pub app_theme: &'static str,
    /// Fundo do painel: chave de um fundo de `backdrop` (embutido ou de arquivo).
    pub backdrop: &'static str,

    // superfícies (0xRRGGBB)
    pub bg: u32,
    pub panel: u32,
    pub panel_top: Option<u32>,
    pub edge: u32,
    pub edge_inner: Option<u32>,
    pub line: u32,
    pub surface: u32,
    pub surface_edge: u32,
    pub field: u32,
    pub field_edge: u32,
    pub slot: u32,
    // texto
    pub ink: u32,
    pub sub: u32,
    pub dim: u32,
    pub panel_ink: u32,
    pub panel_sub: u32,
    pub tag_ink: u32,
    // acentos
    pub accent: u32,
    pub accent_ink: u32,
    pub accent2: u32,
    pub hover: u32,
    pub danger: u32,
    pub bad: u32,

    // forma (pixels lógicos)
    pub chamfer: f32,
    pub border: f32,
    pub header_h: f32,
    pub header_line: f32,
    pub panel_top_h: f32,
    pub footer_h: f32,
    pub card_alpha: f32,

    // letras
    pub font_ui: &'static str,
    pub font_mono: &'static str,
    pub font_title: &'static str,

    // cabeçalho
    pub title: TitleStyle,
    pub title_size: f32,
    pub title_weight: i32,
    pub title_spacing: f32,
    pub title_cursor: bool,
    /// Ícone em pixel (quadro 8×8) e o furo dele.
    pub title_icon: &'static str,
    pub title_icon_cut: &'static str,
    pub tag: Tag,
    pub buttons: Buttons,
}

const CHAKRA: &str = "Chakra Petch";
const MONO: &str = "JetBrains Mono";
const PIXEL: &str = "Pixelify Sans";

pub static BUILTIN: &[Skin] = &[
    Skin {
        key: "neon",
        label: "Neon",
        app_theme: "neon",
        backdrop: "neon",
        bg: 0x0B0912, panel: 0x0B0912, panel_top: None, edge: 0x2E2546, edge_inner: None, line: 0x1E1830,
        surface: 0x120A1F, surface_edge: 0x3A1F55, field: 0x0D0717, field_edge: 0x3A2A55, slot: 0x2A4A55,
        ink: 0xEDE6FF, sub: 0xC9BEE6, dim: 0x8E80AA, panel_ink: 0xEDE6FF, panel_sub: 0xC9BEE6, tag_ink: 0x8E80AA,
        accent: 0x3DF2FF, accent_ink: 0x04161A, accent2: 0xFF3DB8, hover: 0x24123A, danger: 0xFF8095, bad: 0xFF4D6A,
        chamfer: 18.0, border: 1.0, header_h: 48.0, header_line: 1.0, panel_top_h: 0.0, footer_h: 0.0, card_alpha: 1.0,
        font_ui: CHAKRA, font_mono: MONO, font_title: CHAKRA,
        title: TitleStyle::Upper, title_size: 18.0, title_weight: 700, title_spacing: 3.2, title_cursor: false,
        title_icon: "", title_icon_cut: "", tag: Tag::Grid, buttons: Buttons::Icon,
    },
    Skin {
        key: "terminal",
        label: "Terminal",
        app_theme: "matrix",
        backdrop: "terminal",
        bg: 0x030A05, panel: 0x061209, panel_top: None, edge: 0x1F4D2A, edge_inner: None, line: 0x16391F,
        surface: 0x061209, surface_edge: 0x16391F, field: 0x030A05, field_edge: 0x1F4D2A, slot: 0x12301A,
        ink: 0xB9F5B0, sub: 0x8FD486, dim: 0x6FB866, panel_ink: 0xB9F5B0, panel_sub: 0x8FD486, tag_ink: 0x6FB866,
        accent: 0x7CFF6B, accent_ink: 0x031006, accent2: 0xE6FF5C, hover: 0x0E2A14, danger: 0xFF7A6B, bad: 0xFF5C4D,
        chamfer: 0.0, border: 1.0, header_h: 44.0, header_line: 1.0, panel_top_h: 0.0, footer_h: 28.0, card_alpha: 1.0,
        font_ui: MONO, font_mono: MONO, font_title: MONO,
        title: TitleStyle::Prompt, title_size: 14.0, title_weight: 400, title_spacing: 0.0, title_cursor: true,
        title_icon: "", title_icon_cut: "", tag: Tag::None, buttons: Buttons::Bracket,
    },
    Skin {
        key: "aconchego",
        label: "Aconchego",
        app_theme: "aconchego",
        backdrop: "papel",
        bg: 0xF4E6CC, panel: 0x8A5F40, panel_top: Some(0xA87650), edge: 0x4A3122, edge_inner: Some(0x8A5F40), line: 0x4A3122,
        surface: 0xFBF1DE, surface_edge: 0xD9BF97, field: 0xFFF8EC, field_edge: 0xB8936A, slot: 0xB8936A,
        ink: 0x3E2A1E, sub: 0x6F5A4A, dim: 0x8A6E58, panel_ink: 0xFFF3DC, panel_sub: 0xF4E6CC, tag_ink: 0xF4E6CC,
        accent: 0xC0602F, accent_ink: 0xFFF3DC, accent2: 0x3F8A3A, hover: 0xEAD6B3, danger: 0xA8402A, bad: 0xB4322A,
        chamfer: 0.0, border: 6.0, header_h: 48.0, header_line: 4.0, panel_top_h: 3.0, footer_h: 10.0, card_alpha: 1.0,
        font_ui: PIXEL, font_mono: PIXEL, font_title: PIXEL,
        title: TitleStyle::Plain, title_size: 22.0, title_weight: 700, title_spacing: 0.0, title_cursor: false,
        title_icon: "M3 0h2v1H3zM2 1h4v1H2zM1 2h6v1H1zM0 3h8v1H0zM1 4h6v4H1z", title_icon_cut: "M3 5h2v3H3z",
        tag: Tag::None, buttons: Buttons::Chunky,
    },
    Skin {
        key: "noite",
        label: "Noite chuvosa",
        app_theme: "noite",
        backdrop: "cidade",
        bg: 0x0C1024, panel: 0x0F1430, panel_top: None, edge: 0x2C3566, edge_inner: None, line: 0x252E5A,
        surface: 0x10163A, surface_edge: 0x2C3566, field: 0x0A0E22, field_edge: 0x2C3566, slot: 0x2C3566,
        ink: 0xE4E8FF, sub: 0xB9C2F0, dim: 0x7F89BF, panel_ink: 0xE4E8FF, panel_sub: 0xB9C2F0, tag_ink: 0xFFC46B,
        accent: 0xFFC46B, accent_ink: 0x1A1205, accent2: 0xFF6FB5, hover: 0x1A2250, danger: 0xFF8A9A, bad: 0xFF5C6C,
        chamfer: 0.0, border: 1.0, header_h: 48.0, header_line: 1.0, panel_top_h: 0.0, footer_h: 0.0, card_alpha: 0.84,
        font_ui: CHAKRA, font_mono: MONO, font_title: PIXEL,
        title: TitleStyle::Plain, title_size: 22.0, title_weight: 700, title_spacing: 0.0, title_cursor: false,
        title_icon: "", title_icon_cut: "", tag: Tag::Greeting, buttons: Buttons::Icon,
    },
    // os quatro abaixo reaproveitam os fundos de cima, pintados com as cores de cada um
    Skin {
        key: "synthwave",
        label: "Synthwave",
        app_theme: "synthwave",
        backdrop: "neon",
        bg: 0x0E0420, panel: 0x0E0420, panel_top: None, edge: 0x3B1A66, edge_inner: None, line: 0x24103F,
        surface: 0x170833, surface_edge: 0x4A1F7A, field: 0x0B031A, field_edge: 0x4A2A80, slot: 0x5A2A55,
        ink: 0xFFE8FF, sub: 0xD9B8F0, dim: 0xA787C8, panel_ink: 0xFFE8FF, panel_sub: 0xD9B8F0, tag_ink: 0xA787C8,
        accent: 0xFF8A00, accent_ink: 0x1A0800, accent2: 0xFF6EC7, hover: 0x2A0F4A, danger: 0xFF8095, bad: 0xFF4D6A,
        chamfer: 10.0, border: 1.0, header_h: 48.0, header_line: 1.0, panel_top_h: 0.0, footer_h: 0.0, card_alpha: 1.0,
        font_ui: CHAKRA, font_mono: MONO, font_title: CHAKRA,
        title: TitleStyle::Upper, title_size: 18.0, title_weight: 700, title_spacing: 3.2, title_cursor: false,
        title_icon: "", title_icon_cut: "", tag: Tag::Grid, buttons: Buttons::Icon,
    },
    Skin {
        key: "gelo",
        label: "Gelo",
        app_theme: "ghost",
        backdrop: "terminal",
        bg: 0x020B14, panel: 0x041321, panel_top: None, edge: 0x1C3B55, edge_inner: None, line: 0x123049,
        surface: 0x061A2B, surface_edge: 0x163852, field: 0x020B14, field_edge: 0x1F4766, slot: 0x163852,
        ink: 0xE8F6FF, sub: 0xA9C9E0, dim: 0x82A9C6, panel_ink: 0xE8F6FF, panel_sub: 0xA9C9E0, tag_ink: 0x82A9C6,
        accent: 0x4FC3F7, accent_ink: 0x021627, accent2: 0x64FFDA, hover: 0x0B2740, danger: 0xFF8A80, bad: 0xFF5252,
        chamfer: 0.0, border: 1.0, header_h: 44.0, header_line: 1.0, panel_top_h: 0.0, footer_h: 28.0, card_alpha: 1.0,
        font_ui: MONO, font_mono: MONO, font_title: MONO,
        title: TitleStyle::Prompt, title_size: 14.0, title_weight: 400, title_spacing: 0.0, title_cursor: true,
        title_icon: "", title_icon_cut: "", tag: Tag::None, buttons: Buttons::Bracket,
    },
    Skin {
        key: "vaporwave",
        label: "Vaporwave",
        app_theme: "vaporwave",
        backdrop: "papel",
        bg: 0x1E1230, panel: 0x2B1B3D, panel_top: Some(0x3A2550), edge: 0x6E5A85, edge_inner: Some(0x2B1B3D), line: 0x6E5A85,
        surface: 0x2B1B3D, surface_edge: 0x4E3A66, field: 0x1E1230, field_edge: 0x6E5A85, slot: 0x4E3A66,
        ink: 0xFFF0FB, sub: 0xD9C2EC, dim: 0xB49BCB, panel_ink: 0xFFF0FB, panel_sub: 0xD9C2EC, tag_ink: 0xD9C2EC,
        accent: 0x7DF9FF, accent_ink: 0x1E1230, accent2: 0xFF9ECF, hover: 0x3A2550, danger: 0xFF8FB0, bad: 0xFF5C8A,
        chamfer: 0.0, border: 4.0, header_h: 48.0, header_line: 3.0, panel_top_h: 3.0, footer_h: 10.0, card_alpha: 1.0,
        font_ui: CHAKRA, font_mono: MONO, font_title: PIXEL,
        title: TitleStyle::Plain, title_size: 22.0, title_weight: 700, title_spacing: 0.0, title_cursor: false,
        title_icon: "", title_icon_cut: "", tag: Tag::None, buttons: Buttons::Chunky,
    },
    Skin {
        key: "akira",
        label: "Akira",
        app_theme: "akira",
        backdrop: "cidade",
        bg: 0x0D0303, panel: 0x140505, panel_top: None, edge: 0x4A1515, edge_inner: None, line: 0x2E0B0B,
        surface: 0x1A0606, surface_edge: 0x4A1515, field: 0x0D0303, field_edge: 0x5A1F1F, slot: 0x4A1515,
        ink: 0xFFECEC, sub: 0xE0B5B5, dim: 0xB58585, panel_ink: 0xFFECEC, panel_sub: 0xE0B5B5, tag_ink: 0xFF6A6A,
        accent: 0xFF3B3B, accent_ink: 0x120000, accent2: 0xF5F5F5, hover: 0x2A0A0A, danger: 0xFF8A8A, bad: 0xFF5C5C,
        chamfer: 0.0, border: 1.0, header_h: 48.0, header_line: 1.0, panel_top_h: 0.0, footer_h: 0.0, card_alpha: 0.84,
        font_ui: CHAKRA, font_mono: MONO, font_title: CHAKRA,
        title: TitleStyle::Upper, title_size: 18.0, title_weight: 700, title_spacing: 3.2, title_cursor: false,
        title_icon: "", title_icon_cut: "", tag: Tag::Greeting, buttons: Buttons::Icon,
    },
];

/// Fontes que vêm no binário (ver `ui/hub.slint`): as únicas aceitas num visual de arquivo.
pub const FONTS: &[&str] = &[CHAKRA, MONO, PIXEL];

/// Visuais lidos de arquivo (depois dos embutidos); vazam a cada recarga, como os temas.
static EXTRA: RwLock<Vec<&'static Skin>> = RwLock::new(Vec::new());

pub fn set_extra(list: Vec<Skin>) {
    let leaked = list.into_iter().map(|s| &*Box::leak(Box::new(s))).collect();
    if let Ok(mut extra) = EXTRA.write() {
        *extra = leaked;
    }
}

/// Todos os visuais: os embutidos e, depois, os de arquivo.
pub fn all() -> Vec<&'static Skin> {
    let extra = EXTRA.read().map(|e| e.clone()).unwrap_or_default();
    BUILTIN.iter().chain(extra).collect()
}

pub fn find(key: &str) -> Option<&'static Skin> {
    if let Some(s) = BUILTIN.iter().find(|s| s.key == key) {
        return Some(s);
    }
    EXTRA.read().ok()?.iter().copied().find(|s| s.key == key)
}

/// O visual `key`, ou o primeiro se não existir (um visual de arquivo que sumiu).
pub fn get(key: &str) -> &'static Skin {
    find(key).unwrap_or(&BUILTIN[0])
}

pub fn index_of(key: &str) -> usize {
    all().iter().position(|s| s.key == key).unwrap_or(0)
}

/// Cores de destaque que a pessoa pode escolher (Configurações › Aparência). A cor é
/// ajustada ao visual para continuar legível (`effective`).
pub const ACCENTS: &[(&str, &str, u32)] = &[
    ("ciano", "Ciano", 0x3DF2FF),
    ("magenta", "Magenta", 0xFF3DB8),
    ("ambar", "Âmbar", 0xFFB020),
    ("verde", "Verde", 0x4CE06A),
    ("violeta", "Violeta", 0xA77BFF),
    ("coral", "Coral", 0xFF6F59),
    ("azul", "Azul", 0x4D8DFF),
];

/// O visual com a cor de destaque da config (`""` = a do visual). O destaque vira texto e
/// borda sobre `bg` e `surface`, então é clareado ou escurecido até 3:1 sobre os dois.
pub fn effective(base: &Skin, accent: &str) -> Skin {
    let mut s = base.clone();
    if let Some(&(_, _, c)) = ACCENTS.iter().find(|a| a.0 == accent) {
        s.accent = color::ensure_contrast(c, &[s.bg, s.surface], 3.0);
        s.accent_ink = color::ink_on(s.accent);
    }
    s
}

/// Gera, a partir da lista de nomes, as cores que um arquivo de visual (ou um fundo, como
/// `color = "accent"`) pode citar pelo nome.
macro_rules! color_fields {
    ($($f:ident),* $(,)?) => {
        /// Cores de um visual, pelo nome (`bg`, `accent`…).
        pub const COLOR_FIELDS: &[&str] = &[$(stringify!($f)),*];
        impl Skin {
            pub fn color(&self, name: &str) -> Option<u32> {
                match name {
                    $(stringify!($f) => Some(self.$f),)*
                    _ => None,
                }
            }
            pub fn color_mut(&mut self, name: &str) -> Option<&mut u32> {
                match name {
                    $(stringify!($f) => Some(&mut self.$f),)*
                    _ => None,
                }
            }
        }
    };
}
color_fields!(
    bg, panel, edge, line, surface, surface_edge, field, field_edge, slot, ink, sub, dim, panel_ink, panel_sub, tag_ink,
    accent, accent_ink, accent2, hover, danger, bad,
);

/// Medidas de um visual, pelo nome, com o intervalo aceito num arquivo.
pub const NUM_FIELDS: &[(&str, f32, f32)] = &[
    ("chamfer", 0.0, 40.0),
    ("border", 0.0, 12.0),
    ("header_h", 32.0, 80.0),
    ("header_line", 0.0, 8.0),
    ("panel_top_h", 0.0, 12.0),
    ("footer_h", 0.0, 40.0),
    ("card_alpha", 0.5, 1.0),
    ("title_size", 10.0, 32.0),
    ("title_spacing", 0.0, 8.0),
];

impl Skin {
    pub fn num_mut(&mut self, name: &str) -> Option<&mut f32> {
        Some(match name {
            "chamfer" => &mut self.chamfer,
            "border" => &mut self.border,
            "header_h" => &mut self.header_h,
            "header_line" => &mut self.header_line,
            "panel_top_h" => &mut self.panel_top_h,
            "footer_h" => &mut self.footer_h,
            "card_alpha" => &mut self.card_alpha,
            "title_size" => &mut self.title_size,
            "title_spacing" => &mut self.title_spacing,
            _ => return None,
        })
    }
}

fn rgb(c: u32) -> Color {
    Color::from_rgb_u8((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

thread_local! {
    /// O último visual aplicado ao hub (já com destaque e tamanho do texto): as outras
    /// janelas (ex.: o detalhe de um agente) recebem o mesmo com `apply_current`.
    static CURRENT: std::cell::RefCell<Option<(Skin, String, f32)>> = const { std::cell::RefCell::new(None) };
}

/// Passa o visual para o hub e o guarda para as outras janelas; `name` é o nome exibido do
/// app e `text_scale` o tamanho do texto (1,0 = o do design).
pub fn apply(ui: &HubWindow, s: &Skin, name: &str, text_scale: f32) {
    CURRENT.with(|c| *c.borrow_mut() = Some((s.clone(), name.to_string(), text_scale)));
    apply_to(ui, s, name, text_scale);
}

/// Passa o último visual aplicado ao hub para outra janela (cada janela do Slint tem os
/// próprios globais). Antes do hub, não faz nada.
pub fn apply_current<W>(ui: &W)
where
    W: ComponentHandle,
    for<'a> SkinGlobal<'a>: slint::Global<'a, W>,
{
    if let Some((s, name, scale)) = CURRENT.with(|c| c.borrow().clone()) {
        apply_to(ui, &s, &name, scale);
    }
}

fn apply_to<W>(ui: &W, s: &Skin, name: &str, text_scale: f32)
where
    W: ComponentHandle,
    for<'a> SkinGlobal<'a>: slint::Global<'a, W>,
{
    let g = ui.global::<SkinGlobal>();
    g.set_text_scale(text_scale);
    g.set_bg(rgb(s.bg));
    g.set_panel(rgb(s.panel));
    g.set_panel_top(s.panel_top.map_or(Color::from_argb_u8(0, 0, 0, 0), rgb));
    g.set_edge(rgb(s.edge));
    g.set_edge_inner(s.edge_inner.map_or(Color::from_argb_u8(0, 0, 0, 0), rgb));
    g.set_line(rgb(s.line));
    g.set_surface(rgb(s.surface));
    g.set_surface_edge(rgb(s.surface_edge));
    g.set_field(rgb(s.field));
    g.set_field_edge(rgb(s.field_edge));
    g.set_slot(rgb(s.slot));
    g.set_ink(rgb(s.ink));
    g.set_sub(rgb(s.sub));
    g.set_dim(rgb(s.dim));
    g.set_panel_ink(rgb(s.panel_ink));
    g.set_panel_sub(rgb(s.panel_sub));
    g.set_tag_ink(rgb(s.tag_ink));
    g.set_accent(rgb(s.accent));
    g.set_accent_ink(rgb(s.accent_ink));
    g.set_accent2(rgb(s.accent2));
    g.set_hover(rgb(s.hover));
    g.set_danger(rgb(s.danger));
    g.set_danger_edge(rgb(mix(s.danger, s.bg, 0.35)));
    g.set_bad(rgb(s.bad));
    // modo de edição: tons do acento sobre o fundo
    g.set_edit_bg(rgb(mix(s.accent, s.bg, 0.08)));
    g.set_edit_line(rgb(mix(s.accent, s.bg, 0.4)));
    g.set_edit_ink(rgb(mix(s.accent, s.ink, 0.3)));
    g.set_edit_sub(rgb(mix(s.accent, s.ink, 0.55)));

    g.set_chamfer(s.chamfer);
    g.set_border(s.border);
    g.set_header_h(s.header_h);
    g.set_header_line(s.header_line);
    g.set_panel_top_h(s.panel_top_h);
    g.set_footer_h(s.footer_h);
    g.set_card_alpha(s.card_alpha);

    g.set_font_ui(s.font_ui.into());
    g.set_font_mono(s.font_mono.into());
    g.set_font_title(s.font_title.into());

    g.set_title(s.title.format(name).into());
    g.set_title_size(s.title_size);
    g.set_title_weight(s.title_weight);
    g.set_title_spacing(s.title_spacing);
    g.set_title_cursor(s.title_cursor);
    g.set_title_icon(s.title_icon.into());
    g.set_title_icon_cut(s.title_icon_cut.into());
    g.set_tag(
        match s.tag {
            Tag::Grid => "grid",
            Tag::Greeting => "greeting",
            Tag::None => "none",
        }
        .into(),
    );
    g.set_buttons(
        match s.buttons {
            Buttons::Icon => "icon",
            Buttons::Bracket => "bracket",
            Buttons::Chunky => "chunky",
        }
        .into(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mascot::themes;

    #[test]
    fn chaves_unicas_e_tema_e_fundo_existem() {
        for (i, s) in BUILTIN.iter().enumerate() {
            assert!(BUILTIN.iter().skip(i + 1).all(|o| o.key != s.key), "{} repetido", s.key);
            assert!(themes::find(s.app_theme).is_some(), "{}: tema {} não existe", s.key, s.app_theme);
            assert!(backdrop::find(s.backdrop).is_some(), "{}: fundo {} não existe", s.key, s.backdrop);
            assert!(FONTS.contains(&s.font_ui) && FONTS.contains(&s.font_mono) && FONTS.contains(&s.font_title));
        }
    }

    #[test]
    fn desconhecido_cai_no_primeiro() {
        assert_eq!(get("nao-existe").key, BUILTIN[0].key);
        assert_eq!(index_of("noite"), 3);
    }

    #[test]
    fn titulo_segue_o_nome() {
        assert_eq!(TitleStyle::Upper.format("MiniUtils"), "MINIUTILS");
        assert_eq!(TitleStyle::Plain.format("Toca"), "Toca");
        assert_eq!(TitleStyle::Prompt.format("Minha Casa"), "minha-casa@bandeja:~$");
    }

    #[test]
    fn destaque_escolhido_fica_legivel() {
        let papel = get("aconchego");
        // sem escolha: o do visual
        assert_eq!(effective(papel, "").accent, papel.accent);
        // ciano no papel claro escurece até 3:1
        let e = effective(papel, "ciano");
        assert!(color::contrast(e.accent, e.bg) >= 3.0 && color::contrast(e.accent, e.surface) >= 3.0);
        assert!(color::contrast(e.accent_ink, e.accent) >= 4.5);
        // no Neon escuro, o ciano já serve
        assert_eq!(effective(get("neon"), "ciano").accent, 0x3DF2FF);
    }

    #[test]
    fn campos_pelo_nome() {
        let mut s = get("neon").clone();
        assert_eq!(s.color("accent"), Some(0x3DF2FF));
        assert_eq!(COLOR_FIELDS.len(), 21);
        for f in COLOR_FIELDS {
            assert!(s.color_mut(f).is_some(), "{f}");
        }
        for (f, _, _) in NUM_FIELDS {
            assert!(s.num_mut(f).is_some(), "{f}");
        }
    }
}
