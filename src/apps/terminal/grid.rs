//! What the terminal window draws, without a screen: the colors, the screen split into runs
//! of text with the same look, and the keys turned into the bytes the shell expects.

use slint::Color;
use slint::platform::Key;

/// The 16 colors of the terminal plus the default text and background.
#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub fg: Color,
    pub bg: Color,
    pub ansi: [Color; 16],
}

/// xterm's 16 colors (when the app theme is not used for them).
const XTERM: [u32; 16] = [
    0x000000, 0xcd0000, 0x00cd00, 0xcdcd00, 0x0000ee, 0xcd00cd, 0x00cdcd, 0xe5e5e5, //
    0x7f7f7f, 0xff0000, 0x00ff00, 0xffff00, 0x5c5cff, 0xff00ff, 0x00ffff, 0xffffff,
];

fn rgb(v: u32) -> Color {
    Color::from_rgb_u8((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// `a` moved towards `b` by `t` (0..1).
fn mix(a: Color, b: Color, t: f32) -> Color {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color::from_rgb_u8(m(a.red(), b.red()), m(a.green(), b.green()), m(a.blue(), b.blue()))
}

impl Palette {
    /// From the app theme: default text and background, and the 16 colors either drawn from
    /// the theme (`themed`) or xterm's.
    pub fn new(t: &crate::ui::Colors, themed: bool) -> Self {
        let bg = mix(t.bg0, t.bg1, 0.25);
        let ansi = if themed {
            let white = Color::from_rgb_u8(255, 255, 255);
            let bright = |c: Color| mix(c, white, 0.3);
            [
                t.bg1,
                t.alarm,
                t.ok,
                t.warn,
                t.visor,
                t.fur,
                t.sleep,
                t.sub,
                t.muted,
                bright(t.alarm),
                bright(t.ok),
                bright(t.warn),
                bright(t.visor),
                bright(t.fur),
                bright(t.sleep),
                t.text,
            ]
        } else {
            XTERM.map(rgb)
        };
        Palette { fg: t.text, bg, ansi }
    }

    /// Color `c` of the terminal; `None` = the default (text or background).
    fn color(&self, c: vt100::Color) -> Option<Color> {
        match c {
            vt100::Color::Default => None,
            vt100::Color::Idx(i) => Some(self.indexed(i)),
            vt100::Color::Rgb(r, g, b) => Some(Color::from_rgb_u8(r, g, b)),
        }
    }

    /// The 256-color table: the 16 of the palette, the 6×6×6 cube and 24 grays.
    fn indexed(&self, i: u8) -> Color {
        match i {
            0..16 => self.ansi[i as usize],
            16..232 => {
                let n = i - 16;
                let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                Color::from_rgb_u8(level(n / 36), level((n / 6) % 6), level(n % 6))
            }
            _ => {
                let v = 8 + (i - 232) * 10;
                Color::from_rgb_u8(v, v, v)
            }
        }
    }
}

/// A piece of a line with one look, `len` cells wide, starting at column `col`.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub col: u16,
    pub len: u16,
    pub text: String,
    pub fg: Color,
    /// Transparent = the window background.
    pub bg: Color,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

/// Selected area, in screen cells (row, column), start before end; the end is included.
pub type Selection = ((u16, u16), (u16, u16));

/// Puts the two ends of a selection in reading order.
pub fn ordered(a: (u16, u16), b: (u16, u16)) -> Selection {
    if a <= b { (a, b) } else { (b, a) }
}

fn selected(sel: Option<Selection>, row: u16, col: u16) -> bool {
    sel.is_some_and(|(a, b)| (row, col) >= a && (row, col) <= b)
}

/// The runs of each visible line. Cells with the same look join in one run; a character
/// outside ASCII gets a run of its own (wide characters and glyphs from another font
/// would push the rest of the line off the grid). Blank cells on the window background
/// are left out.
pub fn lines(screen: &vt100::Screen, pal: &Palette, sel: Option<Selection>) -> Vec<Vec<Run>> {
    let (rows, cols) = screen.size();
    (0..rows.get()).map(|row| line(screen, pal, sel, row, cols.get())).collect()
}

fn line(screen: &vt100::Screen, pal: &Palette, sel: Option<Selection>, row: u16, cols: u16) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    let mut joinable = false;
    for col in 0..cols {
        let Some(cell) = screen.cell(row, col) else { break };
        if cell.is_wide_continuation() {
            continue;
        }
        let mut fg = pal.color(cell.fgcolor()).unwrap_or(pal.fg);
        let mut bg = pal.color(cell.bgcolor());
        if cell.inverse() != selected(sel, row, col) {
            (fg, bg) = (bg.unwrap_or(pal.bg), Some(fg));
        }
        if cell.dim() {
            fg = fg.with_alpha(0.6);
        }
        let text = if cell.has_contents() { cell.contents() } else { " " };
        let ascii = text.len() == 1 && text.is_ascii();
        let blank = text == " " && bg.is_none();
        let bg = bg.unwrap_or(Color::from_argb_u8(0, 0, 0, 0));
        let len = if cell.is_wide() { 2 } else { 1 };
        let (bold, italic, underline) = (cell.bold(), cell.italic(), cell.underline());
        if let Some(last) = runs.last_mut()
            && joinable
            && ascii
            && last.col + last.len == col
            && (last.fg, last.bg, last.bold, last.italic, last.underline) == (fg, bg, bold, italic, underline)
        {
            last.text.push_str(text);
            last.len += 1;
            continue;
        }
        if blank && !underline {
            joinable = false;
            continue;
        }
        runs.push(Run { col, len, text: text.to_string(), fg, bg, bold, italic, underline });
        joinable = ascii;
    }
    // spaces at the end of a run on the window background draw nothing
    for r in &mut runs {
        if r.bg.alpha() == 0 && !r.underline {
            let kept = r.text.trim_end_matches(' ').len();
            r.len -= (r.text.len() - kept) as u16;
            r.text.truncate(kept);
        }
    }
    runs.retain(|r| r.len > 0);
    runs
}

/// Keyboard modifiers of a key press.
#[derive(Debug, Clone, Copy, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// The bytes a key press sends to the shell (`None` = the key sends nothing, e.g. Shift
/// alone). `text` is what Slint reports: the typed character or a special key.
/// `app_cursor`: the program asked for the arrows' "application" form (`ESC O A`).
pub fn key_bytes(text: &str, m: Mods, app_cursor: bool) -> Option<Vec<u8>> {
    let mut chars = text.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        // several characters at once (input method): goes as it is
        return Some(text.as_bytes().to_vec());
    }
    // xterm's modifier code: 1 + shift + 2·alt + 4·ctrl
    let code = 1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.ctrl as u8;
    let csi = |end: char| -> Vec<u8> {
        if code > 1 {
            format!("\x1b[1;{code}{end}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{end}").into_bytes()
        } else {
            format!("\x1b[{end}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if code > 1 { format!("\x1b[{n};{code}~").into_bytes() } else { format!("\x1b[{n}~").into_bytes() }
    };
    let ss3 = |end: char| -> Vec<u8> {
        if code > 1 { format!("\x1b[1;{code}{end}").into_bytes() } else { format!("\x1bO{end}").into_bytes() }
    };
    let special: &[(Key, &dyn Fn() -> Vec<u8>)] = &[
        (Key::UpArrow, &|| csi('A')),
        (Key::DownArrow, &|| csi('B')),
        (Key::RightArrow, &|| csi('C')),
        (Key::LeftArrow, &|| csi('D')),
        (Key::Home, &|| csi('H')),
        (Key::End, &|| csi('F')),
        (Key::Insert, &|| tilde(2)),
        (Key::Delete, &|| tilde(3)),
        (Key::PageUp, &|| tilde(5)),
        (Key::PageDown, &|| tilde(6)),
        (Key::F1, &|| ss3('P')),
        (Key::F2, &|| ss3('Q')),
        (Key::F3, &|| ss3('R')),
        (Key::F4, &|| ss3('S')),
        (Key::F5, &|| tilde(15)),
        (Key::F6, &|| tilde(17)),
        (Key::F7, &|| tilde(18)),
        (Key::F8, &|| tilde(19)),
        (Key::F9, &|| tilde(20)),
        (Key::F10, &|| tilde(21)),
        (Key::F11, &|| tilde(23)),
        (Key::F12, &|| tilde(24)),
    ];
    if let Some((_, bytes)) = special.iter().find(|(k, _)| char::from(*k) == c) {
        return Some(bytes());
    }
    let alt = |mut b: Vec<u8>| -> Vec<u8> {
        if m.alt {
            b.insert(0, 0x1b);
        }
        b
    };
    match c {
        _ if c == char::from(Key::Return) || c == '\r' => return Some(alt(vec![b'\r'])),
        _ if c == char::from(Key::Backspace) => return Some(alt(vec![if m.ctrl { 0x08 } else { 0x7f }])),
        _ if c == char::from(Key::Tab) => return Some(if m.shift { b"\x1b[Z".to_vec() } else { alt(vec![b'\t']) }),
        _ if c == char::from(Key::Escape) => return Some(alt(vec![0x1b])),
        _ => {}
    }
    // other special keys (Shift, Control, Meta, media…): Slint gives them control
    // characters or the private use area; they send nothing
    if ('\u{10}'..='\u{1a}').contains(&c) || ('\u{f700}'..='\u{f8ff}').contains(&c) {
        return None;
    }
    // Ctrl+Alt is AltGr on many keyboards: the character it produced goes as it is
    if m.ctrl && !(m.alt && !c.is_ascii_alphabetic()) {
        let b = match c {
            'a'..='z' | 'A'..='Z' => c.to_ascii_lowercase() as u8 & 0x1f,
            ' ' | '@' | '2' => 0,
            '[' | '3' => 0x1b,
            '\\' | '4' => 0x1c,
            ']' | '5' => 0x1d,
            '^' | '6' => 0x1e,
            '_' | '/' | '7' => 0x1f,
            '8' => 0x7f,
            _ => return Some(text.as_bytes().to_vec()),
        };
        return Some(alt(vec![b]));
    }
    if m.alt && !m.ctrl {
        return Some(alt(text.as_bytes().to_vec()));
    }
    Some(text.as_bytes().to_vec())
}

/// Text pasted into the terminal: line breaks become Enter (`\r`), and with "bracketed
/// paste" on, the program is told it is a paste (so a shell does not run each line).
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let body = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        // a paste that closes the bracket itself would escape it
        let body = body.replace("\x1b[201~", "");
        format!("\x1b[200~{body}\x1b[201~").into_bytes()
    } else {
        body.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pal() -> Palette {
        Palette { fg: rgb(0xeeeeee), bg: rgb(0x101010), ansi: XTERM.map(rgb) }
    }

    fn screen(bytes: &[u8], rows: u16, cols: u16) -> vt100::Parser {
        let mut p = vt100::Parser::new(rows.try_into().unwrap(), cols.try_into().unwrap(), 0);
        p.process(bytes);
        p
    }

    #[test]
    fn joins_cells_with_the_same_look() {
        let p = screen(b"ab \x1b[31mred\x1b[0m end   ", 2, 20);
        let l = lines(p.screen(), &pal(), None);
        assert_eq!(l.len(), 2);
        let r = &l[0];
        assert_eq!(r.iter().map(|r| (r.col, r.len, r.text.as_str())).collect::<Vec<_>>(), [
            (0, 2, "ab"),
            (3, 3, "red"),
            (7, 3, "end")
        ]);
        assert_eq!(r[1].fg, rgb(0xcd0000));
        assert!(l[1].is_empty(), "an empty line has no runs");
    }

    #[test]
    fn colors_inverse_wide_and_selection() {
        let p = screen("\x1b[44mx\x1b[0m \x1b[7mi\x1b[0m 日b\x1b[38;2;1;2;3mc\x1b[38;5;196md".as_bytes(), 1, 20);
        let l = &lines(p.screen(), &pal(), None)[0];
        assert_eq!((l[0].text.as_str(), l[0].bg), ("x", rgb(0x0000ee)));
        // inverse: the text gets the background color and vice versa
        assert_eq!((l[1].text.as_str(), l[1].fg, l[1].bg), ("i", pal().bg, pal().fg));
        // the wide character takes two cells, in a run of its own
        assert_eq!((l[2].col, l[2].len, l[2].text.as_str()), (4, 2, "日"));
        assert_eq!((l[3].col, l[3].text.as_str()), (6, "b"));
        assert_eq!(l[4].fg, Color::from_rgb_u8(1, 2, 3));
        assert_eq!(l[5].fg, Color::from_rgb_u8(255, 0, 0), "256-color cube");
        // a selection inverts what it covers, blanks included
        let s = &lines(p.screen(), &pal(), Some(((0, 0), (0, 1))))[0];
        assert_eq!((s[0].text.as_str(), s[0].fg, s[0].bg), ("x", rgb(0x0000ee), pal().fg));
        // the selected blank looks like the inverse "i" next to it: they join
        assert_eq!((s[1].col, s[1].text.as_str(), s[1].bg), (1, " i", pal().fg));
    }

    #[test]
    fn keys() {
        let none = Mods::default();
        let ctrl = Mods { ctrl: true, ..none };
        let k = |key: Key| String::from(char::from(key));
        assert_eq!(key_bytes("a", none, false).unwrap(), b"a");
        assert_eq!(key_bytes("ç", none, false).unwrap(), "ç".as_bytes());
        assert_eq!(key_bytes("c", ctrl, false).unwrap(), [3]);
        assert_eq!(key_bytes("C", Mods { shift: true, ..ctrl }, false).unwrap(), [3]);
        assert_eq!(key_bytes("x", Mods { alt: true, ..none }, false).unwrap(), b"\x1bx");
        // AltGr (Ctrl+Alt) produces the character itself
        assert_eq!(key_bytes("@", Mods { alt: true, ..ctrl }, false).unwrap(), b"@");
        assert_eq!(key_bytes(&k(Key::Return), none, false).unwrap(), b"\r");
        assert_eq!(key_bytes(&k(Key::Backspace), none, false).unwrap(), [0x7f]);
        assert_eq!(key_bytes(&k(Key::Delete), none, false).unwrap(), b"\x1b[3~");
        assert_eq!(key_bytes(&k(Key::Tab), Mods { shift: true, ..none }, false).unwrap(), b"\x1b[Z");
        assert_eq!(key_bytes(&k(Key::UpArrow), none, false).unwrap(), b"\x1b[A");
        assert_eq!(key_bytes(&k(Key::UpArrow), none, true).unwrap(), b"\x1bOA");
        assert_eq!(key_bytes(&k(Key::LeftArrow), ctrl, true).unwrap(), b"\x1b[1;5D");
        assert_eq!(key_bytes(&k(Key::F5), none, false).unwrap(), b"\x1b[15~");
        assert_eq!(key_bytes(&k(Key::F1), none, false).unwrap(), b"\x1bOP");
        assert_eq!(key_bytes(&k(Key::Shift), none, false), None);
        assert_eq!(key_bytes(&k(Key::Control), ctrl, false), None);
        assert_eq!(key_bytes("", none, false), None);
    }

    #[test]
    fn paste() {
        assert_eq!(paste_bytes("a\nb\r\nc", false), b"a\rb\rc");
        assert_eq!(paste_bytes("ls\n", true), b"\x1b[200~ls\r\x1b[201~");
        assert_eq!(paste_bytes("x\x1b[201~rm", true), b"\x1b[200~xrm\x1b[201~");
    }

    #[test]
    fn selection_order() {
        assert_eq!(ordered((3, 1), (1, 5)), ((1, 5), (3, 1)));
        assert_eq!(ordered((1, 1), (1, 4)), ((1, 1), (1, 4)));
    }
}
