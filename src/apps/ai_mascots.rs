//! Mascotes das fontes de IA, na mesma grade e formato dos Sinais vitais.
use crate::mascot::sprites::{Pixel, Sprite};

pub static MASCOTS: &[Sprite] = &[
    Sprite {
        key: "claude", label: "claude", eye_bg: b'.', shade_from: 99, fur: None, ear: None,
        pixel: Some(Pixel { eye: 0x422a2a, pal: &[
            (b'k', 0x422a2a),
            (b'l', 0xf2a789),
            (b'o', 0xd97757),
            (b's', 0xb75c46),
        ] }),
        grid: [
            "................",
            "....kkkkkkkk....",
            "...kllllllllk...",
            "..kloooooooolk..",
            "..kooooooooook..",
            ".kkoooeooeoookk.",
            "kookooeooeookook",
            "kookooooooookook",
            "kookoossssookook",
            ".kkoossssssookk.",
            "..kssssssssssk..",
            "...ksskkkssk....",
            "..kkk....kkk....",
            "................",
        ],
    },
    Sprite {
        key: "codex", label: "codex", eye_bg: b'.', shade_from: 99, fur: None, ear: None,
        pixel: Some(Pixel { eye: 0x273934, pal: &[
            (b'k', 0x273934),
            (b'w', 0xe4eee8),
            (b'h', 0xffffff),
            (b'c', 0x76b9a6),
        ] }),
        grid: [
            ".......kk.......",
            ".......ck.......",
            "....kkkkkkkk....",
            "...kwwwwwwwwk...",
            "..kwhwwwwwhwwk..",
            "..kwewwwwewwk...",
            "..kwewwwwewwk...",
            "..kwwccwwccwwk..",
            "..kwwwwwwwwwwk..",
            ".kkwwkwwwwkwwkk.",
            "kckkwckkkkcwkkck",
            ".kk.kwwwwwwk.kk.",
            "....kkk..kkk....",
            "................",
        ],
    },
    Sprite {
        key: "kiro", label: "kiro", eye_bg: b'.', shade_from: 99, fur: None, ear: None,
        pixel: Some(Pixel { eye: 0x302845, pal: &[
            (b'k', 0x302845),
            (b'l', 0xbda7ff),
            (b'h', 0xe2d6ff),
            (b'b', 0x8cd4df),
        ] }),
        grid: [
            "................",
            ".....kkkkkk.....",
            "...kkllllllkk...",
            "..klhhhhlllllk..",
            "..klhlllllllbk..",
            ".klllellllellbk.",
            ".klllellllellbk.",
            ".klllllllllllbk.",
            ".klllbllllbllbk.",
            ".klllbllllbllbk.",
            ".kllllllllllbbk.",
            ".klklklklklklbk.",
            "..k.k.k.k.k.kk..",
            "................",
        ],
    },
    Sprite {
        key: "opencode", label: "opencode", eye_bg: b'.', shade_from: 99, fur: None, ear: None,
        pixel: Some(Pixel { eye: 0x26282e, pal: &[
            (b'k', 0x26282e),
            (b'g', 0x9399a4),
            (b'w', 0xe6e8eb),
        ] }),
        grid: [
            "................",
            ".....kkkkkk.....",
            "....kgwwwwgk....",
            "...kgwwggwwgk...",
            "..kgwwwggwwwgk..",
            "..kgwwwwwwwwgk..",
            ".kggegwwwwgeggk.",
            ".kggegwwwwgeggk.",
            ".kggggggggggggk.",
            ".kgggkwwwwkgggk.",
            "..kgggkkkkgggk..",
            "...kggggggggk...",
            "...kkk....kkk...",
            "................",
        ],
    },
];

pub fn get(name: &str) -> Option<&'static Sprite> {
    let key = match name {
        "Claude" => "claude", "Codex" => "codex", "Kiro" => "kiro",
        "OpenCode Go" => "opencode", _ => return None,
    };
    MASCOTS.iter().find(|sprite| sprite.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mascot::{anim::Look, render, themes, Mood};

    #[test]
    fn todas_tem_olhos_animaveis_e_grade_valida() {
        let theme = themes::get("neon");
        for sprite in MASCOTS {
            assert!(sprite.grid.iter().all(|row| row.len() == 16));
            assert!(sprite.grid.iter().any(|row| row.contains('e')));
            let look = Look { mood: Mood::Idle, reaction: None, blink: false, frame: 3 };
            let awake = render::render_look(sprite, theme, &look);
            let blink = render::render_look(sprite, theme, &Look { blink: true, ..look });
            let alarm = render::render_look(sprite, theme, &Look { mood: Mood::Alarm, ..look });
            assert_ne!(awake, blink, "{} não pisca", sprite.key);
            assert_ne!(awake, alarm, "{} não reage ao alerta", sprite.key);
        }
    }
}
