//! Visuais da janela ("skins"): fundo, moldura, cabeçalho, fontes e cores da interface.
//!
//! Um visual é só uma tabela de valores; a tela (`ui/hub.slint`) lê tudo do global `Skin`.
//! Para criar outro, acrescente uma entrada em `SKINS` (e, se quiser um fundo novo, um
//! caso em `Backdrop` e em `backdrop::render`). O conteúdo dos apps segue o tema de cada
//! app; com o tema "auto", o app usa o `app_theme` do visual.

pub mod backdrop;

use slint::{Color, ComponentHandle};

use crate::ui::{HubWindow, Skin as SkinGlobal};

/// Desenho atrás da grade do painel (ver `backdrop::render`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backdrop {
    /// Grade de pontos ciano e brilhos magenta/ciano nos cantos.
    NeonGrid,
    /// Fósforo verde com vinheta e linhas de varredura.
    Crt,
    /// Papel pontilhado.
    Paper,
    /// Cidade à noite: lua, prédios com janelas acesas, chuva e brilho quente.
    RainyCity,
}

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

pub struct Skin {
    pub key: &'static str,
    pub label: &'static str,
    /// Tema dos apps que escolheram "Acompanha a janela".
    pub app_theme: &'static str,
    pub backdrop: Backdrop,

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

pub static SKINS: &[Skin] = &[
    Skin {
        key: "neon",
        label: "Neon",
        app_theme: "neon",
        backdrop: Backdrop::NeonGrid,
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
        backdrop: Backdrop::Crt,
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
        backdrop: Backdrop::Paper,
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
        backdrop: Backdrop::RainyCity,
        bg: 0x0C1024, panel: 0x0F1430, panel_top: None, edge: 0x2C3566, edge_inner: None, line: 0x252E5A,
        surface: 0x10163A, surface_edge: 0x2C3566, field: 0x0A0E22, field_edge: 0x2C3566, slot: 0x2C3566,
        ink: 0xE4E8FF, sub: 0xB9C2F0, dim: 0x7F89BF, panel_ink: 0xE4E8FF, panel_sub: 0xB9C2F0, tag_ink: 0xFFC46B,
        accent: 0xFFC46B, accent_ink: 0x1A1205, accent2: 0xFF6FB5, hover: 0x1A2250, danger: 0xFF8A9A, bad: 0xFF5C6C,
        chamfer: 0.0, border: 1.0, header_h: 48.0, header_line: 1.0, panel_top_h: 0.0, footer_h: 0.0, card_alpha: 0.84,
        font_ui: CHAKRA, font_mono: MONO, font_title: PIXEL,
        title: TitleStyle::Plain, title_size: 22.0, title_weight: 700, title_spacing: 0.0, title_cursor: false,
        title_icon: "", title_icon_cut: "", tag: Tag::Greeting, buttons: Buttons::Icon,
    },
];

pub fn get(key: &str) -> &'static Skin {
    SKINS.iter().find(|s| s.key == key).unwrap_or(&SKINS[0])
}

pub fn index_of(key: &str) -> usize {
    SKINS.iter().position(|s| s.key == key).unwrap_or(0)
}

fn rgb(c: u32) -> Color {
    Color::from_rgb_u8((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

/// `a` com peso `t` sobre `b`.
fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |s: u32| -> u32 {
        let (x, y) = (((a >> s) & 0xff) as f32, ((b >> s) & 0xff) as f32);
        ((x * t + y * (1.0 - t)).round() as u32) << s
    };
    ch(16) | ch(8) | ch(0)
}

/// Passa o visual para a tela; `name` é o nome exibido do app.
pub fn apply(ui: &HubWindow, s: &Skin, name: &str) {
    let g = ui.global::<SkinGlobal>();
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
    fn chaves_unicas_e_tema_dos_apps_existe() {
        for (i, s) in SKINS.iter().enumerate() {
            assert!(SKINS.iter().skip(i + 1).all(|o| o.key != s.key), "{} repetido", s.key);
            assert!(themes::THEMES.iter().any(|t| t.key == s.app_theme), "{}: tema {} não existe", s.key, s.app_theme);
        }
    }

    #[test]
    fn desconhecido_cai_no_primeiro() {
        assert_eq!(get("nao-existe").key, SKINS[0].key);
        assert_eq!(index_of("noite"), 3);
    }

    #[test]
    fn titulo_segue_o_nome() {
        assert_eq!(TitleStyle::Upper.format("MiniUtils"), "MINIUTILS");
        assert_eq!(TitleStyle::Plain.format("Toca"), "Toca");
        assert_eq!(TitleStyle::Prompt.format("Minha Casa"), "minha-casa@bandeja:~$");
    }

    #[test]
    fn mistura() {
        assert_eq!(mix(0xffffff, 0x000000, 1.0), 0xffffff);
        assert_eq!(mix(0xffffff, 0x000000, 0.0), 0x000000);
        assert_eq!(mix(0xff0000, 0x0000ff, 0.5), 0x800080);
    }
}
