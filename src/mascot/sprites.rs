//! The pixel-art mascots (16 columns × 14 rows), ported from CyberDock.
//!
//! Each character is a color ROLE; the theme decides the actual color (see `render.rs`):
//!   `.` transparent    `K` outline    `P` main color      `S` shadow     `H` highlight
//!   `M` light detail   `C` visor      `E` eye   `W` eye glint   `B` blush
//!   `T` mouth          `F` skin       `D` hardware        `A` LED (follows the mood)
//!
//! The "pixel" mascots (the 20 critters of the Den, 12×12 centered in the frame) don't use
//! roles: each lowercase letter has its own color (`Pixel::pal`) and `e` is the eye, which
//! follows the mood (closed when asleep, orange when busy, red on alarm).
//!
//! The `key` of each mascot is its id, persisted in the config: never rename it. The
//! `label` is the English display name (`tr_noop!`), shown through [`Sprite::name`].

use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

pub struct Sprite {
    pub key: &'static str,
    /// English display name (an imported mascot carries its own name here).
    pub label: &'static str,
    /// Role used in place of the eye when it closes.
    pub eye_bg: u8,
    /// Row from which `P` becomes shadow (only the neko uses it).
    pub shade_from: usize,
    /// Fixed colors that ignore the theme (green frog, chip, graphics cards).
    pub fur: Option<u32>,
    pub ear: Option<u32>,
    /// Own palette ("pixel" mascots); `None` = colors from the theme's roles.
    pub pixel: Option<Pixel>,
    pub grid: [&'static str; 14],
}

/// Fixed colors of a "pixel" mascot.
pub struct Pixel {
    /// Eye color in a calm mood.
    pub eye: u32,
    pub pal: &'static [(u8, u32)],
}

impl Pixel {
    pub fn color(&self, ch: u8) -> Option<u32> {
        self.pal.iter().find(|(c, _)| *c == ch).map(|(_, v)| *v)
    }
}

impl Sprite {
    /// Name to show, in the interface language. An imported mascot's own name passes
    /// through.
    pub fn name(&self) -> &'static str {
        crate::core::i18n::tr(self.label)
    }

    /// Last row of `grid` (the base drawing or an animation frame) that has an eye: where
    /// the closed-eye stroke goes. Computed per frame, so that a pose that moves the head
    /// closes the eye in the right place.
    pub fn closed_row(&self, grid: &[&str; 14]) -> usize {
        let eye = if self.pixel.is_some() { 'e' } else { 'E' };
        grid.iter().rposition(|r| r.contains(eye)).unwrap_or(0)
    }
}

pub static SPRITES: &[Sprite] = &[
    Sprite {
        key: "neko",
        label: crate::tr_noop!("Cyborg Neko"),
        eye_bg: b'C',
        shade_from: 11,
        fur: None,
        ear: None,
        pixel: None,
        grid: [
            ".K............K.",
            "KMK..........KMK",
            "KMPK..KKKK..KPMK",
            "KPPPKKPPPPKKPPPK",
            "KPPPPPPPPPPPPPPK",
            "KPHPPPPPPPPPPHPK",
            "KCCCCCCCCCCCCCCK",
            "KCEEWCCCCCCEEWCK",
            "KCEEECCCCCCEEECK",
            "KCCCCCCCCCCCCCCK",
            "KPBBPPPPPPPPBBPK",
            "KPPPPPPTTPPPPPPK",
            ".KPPPPPPPPPPPPK.",
            "..KKKKKKKKKKKK..",
        ],
    },
    // Lumi, a little moth with its own palette.
    Sprite {
        key: "lumi",
        label: crate::tr_noop!("Lumi, little moth"),
        eye_bg: b'b',
        shade_from: 99,
        fur: None,
        ear: None,
        pixel: Some(Pixel {
            eye: 0x392640,
            pal: &[(b'a', 0x392640), (b'b', 0xd6addb), (b'c', 0xffe8c6),
                   (b'd', 0x9ed5bd), (b'g', 0xfbcf61)],
        }),
        grid: [
            "...aa......aa...",
            "....aa....aa....",
            ".....a....a.....",
            ".....aaaaaa.....",
            "..aaaabbbbaaaa..",
            ".addabbbbbbadda.",
            "adgdabebebadgda.",
            "adddabbbbbbaddda",
            ".adaabcbcbaadaa.",
            "..aaabbbbbbaaa..",
            ".addaaaccaaadda.",
            ".adgdaccccadgda.",
            "..aaacaaacaaa...",
            ".....aa..aa.....",
        ],
    },
    Sprite {
        key: "robo",
        label: crate::tr_noop!("Unit-7 robot"),
        eye_bg: b'C',
        shade_from: 99,
        fur: None,
        ear: None,
        pixel: None,
        grid: [
            "......KAK.......",
            ".......K........",
            ".KKKKKKKKKKKKKK.",
            "KHPPPPPPPPPPPPSK",
            "KPKKKKKKKKKKKKSK",
            "KPKCCCCCCCCCCKSK",
            "KPKCEEWCCEEWCKSK",
            "MPKCEEECCEEECKSM",
            "KPKCCCCCCCCCCKSK",
            "KPKCCCTTTTCCCKSK",
            "KPKKKKKKKKKKKKSK",
            "KPPPBPPPPPPBPPSK",
            "KSSSSSSSSSSSSSSK",
            ".KKKKKKKKKKKKKK.",
        ],
    },
    Sprite {
        key: "kitsune",
        label: crate::tr_noop!("Kitsune (fox)"),
        eye_bg: b'C',
        shade_from: 99,
        fur: None,
        ear: None,
        pixel: None,
        grid: [
            "K..............K",
            "KK............KK",
            "KMK..........KMK",
            "KMPK........KPMK",
            "KMPPKKKKKKKKPPMK",
            "KPPPPPPPPPPPPPPK",
            "KCCCCCCCCCCCCCCK",
            "KCWEECCCCCCWEECK",
            "KCEEECCCCCCEEECK",
            "KPPCCCCCCCCCCPPK",
            "MPPPMMMMMMMMPPPM",
            ".KPPMMMTTMMMPPK.",
            "..KPPMMMMMMPPK..",
            "...KKKKKKKKKK...",
        ],
    },
    Sprite {
        key: "usagi",
        label: crate::tr_noop!("Usagi (rabbit)"),
        eye_bg: b'C',
        shade_from: 99,
        fur: None,
        ear: None,
        pixel: None,
        grid: [
            "...KKK....KKK...",
            "...KMK....KMK...",
            "...KMK....KMK...",
            "...KMK....KMK...",
            "..KKPKKKKKKPKK..",
            ".KHPPPPPPPPPPPK.",
            "KPPPPPPPPPPPPPPK",
            "KCCCCCCCCCCCCCCK",
            "KCEEWCCCCCCEEWCK",
            "KCEEECCCCCCEEECK",
            "KPBBPPPPPPPPBBPK",
            "KPPPPPPTTPPPPPPK",
            ".KPPPPPMMPPPPPK.",
            "..KKKKKKKKKKKK..",
        ],
    },
    Sprite {
        key: "yurei",
        label: crate::tr_noop!("Yūrei (ghost)"),
        eye_bg: b'P',
        shade_from: 99,
        fur: None,
        ear: None,
        pixel: None,
        grid: [
            ".....KKKKKK.....",
            "...KKHHPPPPKK...",
            "..KHHPPPPPPPPK..",
            ".KHPPPPPPPPPPPK.",
            ".KPPPPPPPPPPPAK.",
            "KPPEEWPPPPEEWPPK",
            "KPPEEEPPPPEEEPPK",
            "KPPEEEPPPPEEEPPK",
            "KPBBPPPTTPPPBBPK",
            "KPPPPPPPPPPPPPPK",
            "KPPPPPPPPPPPPPPK",
            "KSPPPSPPPSPPPSPK",
            "KPPKPPKPPKPPKPPK",
            ".KK.KK.KK.KK.KK.",
        ],
    },
    Sprite {
        key: "hacker",
        label: "Hacker-chan",
        eye_bg: b'F',
        shade_from: 99,
        fur: None,
        ear: None,
        pixel: None,
        grid: [
            "....KKKKKKKK....",
            "..KKHHPPPPPPKK..",
            ".KHHPPPPPPPPPPK.",
            "KHPCCCCPPCCCCPPK",
            "KPPPPPPPPPPPPPPK",
            "KPPFPPFFFFPPFPPK",
            "DPFFFFFFFFFFFFPD",
            "DPFEEWFFFFEEWFPD",
            "APFEEEFFFFEEEFPA",
            "DPFEEEFFFFEEEFPD",
            "DPFBBFFFFFFBBFPD",
            "KPPFFFFTTFFFFPPK",
            ".KPPFFFFFFFFPPK.",
            "..KKKKKKKKKKKK..",
        ],
    },
    Sprite {
        key: "security",
        label: "Security",
        eye_bg: b'F',
        shade_from: 99,
        fur: None,
        ear: None,
        pixel: None,
        // security guard: cap with a badge and a visor (D, visible on a dark background), stern
        // eyebrows, collar and tie, and the chest badge on the LED (follows the mood)
        grid: [
            "...KKKKKKKKKK...",
            ".KKPPPPPPPPPPKK.",
            "KPPPPPPMMPPPPPPK",
            "KHPPPPPMMPPPPPPK",
            ".KSSSSSSSSSSSSK.",
            "..DDDDDDDDDDDD..",
            "..KFKKFFFFKKFK..",
            "..KFEWFFFFEWFK..",
            "..KFEEFFFFEEFK..",
            "..KFFFFFFFFFFK..",
            "..KFFFTTTTFFFK..",
            "...KKFFFFFFKK...",
            ".KPPPPMKKMPPPPK.",
            "KPAPPPPKKPPPPPPK",
        ],
    },
    Sprite {
        key: "sapo",
        label: crate::tr_noop!("Frog"),
        eye_bg: b'M',
        shade_from: 99,
        fur: Some(0x3ddc54),
        ear: Some(0xd8ffb0),
        pixel: None,
        grid: [
            "..KKKK....KKKK..",
            ".KMMMMK..KMMMMK.",
            ".KMEEWK..KMEEWK.",
            ".KMEEEKKKKMEEEK.",
            "KPKKKKPPPPKKKKPK",
            "KHPPPPPAPPPPPPPK",
            "KPPPPPPPPPPPPPPK",
            "KPBBPPPPPPPPBBPK",
            "KPPTPPPPPPPPTPPK",
            "KPPPTTTTTTTTPPPK",
            "KSPPPMMMMMMPPPSK",
            "KSSPMMMMMMMMPSSK",
            ".KSSPMMMMMMPSSK.",
            "..KKKKKKKKKKKK..",
        ],
    },
    Sprite {
        key: "chip",
        label: crate::tr_noop!("Microprocessor"),
        eye_bg: b'C',
        shade_from: 99,
        fur: Some(0x8a93a6),
        ear: None,
        pixel: None,
        grid: [
            "..D.D.D..D.D.D..",
            ".KKKKKKKKKKKKKK.",
            ".KHPPPPPPPPPPSK.",
            "DKPKKKKKKKKKKSKD",
            ".KPKCCCCCCCCKSK.",
            "DKPKCEWCCEWCKSKD",
            ".KPKCEECCEECKSK.",
            "DKPKCCCTTCCCKSKD",
            ".KPKKKKKKKKKKSK.",
            "DKPPBPPPPPPBPSKD",
            ".KPPPPPAPPPPPSK.",
            "DKSSSSSSSSSSSSKD",
            ".KKKKKKKKKKKKKK.",
            "..D.D.D..D.D.D..",
        ],
    },
    Sprite {
        key: "nvidia",
        label: crate::tr_noop!("NVIDIA GPU"),
        eye_bg: b'C',
        shade_from: 99,
        fur: Some(0x76b900),
        ear: Some(0xffcc33),
        pixel: None,
        grid: [
            ".KKKKKKKKKKKKKKK",
            "DKHPPPPPPPPPPPSK",
            "DKPKKKKPPKKKKPSK",
            "DKKCCCCKKCCCCKSK",
            "DKKCEWCKKCEWCKSK",
            "DKKCEECKKCEECKSK",
            "DKKCCCCKKCCCCKSK",
            "DKPKKKKPPKKKKPSK",
            "DKPBPPPTTPPPBPSK",
            "DKPPPPPPPPPPPASK",
            "DKSSSSSSSSSSSSSK",
            ".KKKKKKKKKKKKKKK",
            "....MMMMMM.MMM..",
            "....MMMMMM.MMM..",
        ],
    },
    Sprite {
        key: "amd",
        label: crate::tr_noop!("AMD GPU"),
        eye_bg: b'C',
        shade_from: 99,
        fur: Some(0xed1c24),
        ear: Some(0xffcc33),
        pixel: None,
        grid: [
            ".KKKKKKKKKKKKKK.",
            "DKSHHPPPPPPHHSKK",
            "DKPKKKKHHKKKKPSK",
            "DKKCCCCKKCCCCKSK",
            "DKKCWECKKCWECKSK",
            "DKKCEECKKCEECKSK",
            "DKKCCCCKKCCCCKSK",
            "DKPKKKKHHKKKKPSK",
            "DKHBPPPTTPPPBHSK",
            "DKPHPPPPPPPPHASK",
            "DKSSHSSSSSSHSSSK",
            ".KKKKKKKKKKKKKKK",
            "....MMMMMM.MMM..",
            "....MMMMMM.MMM..",
        ],
    },
    Sprite {
        key: "disquete",
        label: crate::tr_noop!("Floppy disk"),
        eye_bg: b'M',
        shade_from: 99,
        fur: None,
        ear: None,
        pixel: None,
        grid: [
            "KKKKKKKKKKKKKKK.",
            "KPPPDDDDDDDPPPKK",
            "KPPPDDDKKDDPPPPK",
            "KPPPDDDKKDDPPPPK",
            "KPPPDDDDDDDPPPPK",
            "KHPPPPPPPPPPPPAK",
            "KPKKKKKKKKKKKKPK",
            "KPKMMMMMMMMMMKPK",
            "KPKMEEWMMEEWMKPK",
            "KPKMEEEMMEEEMKPK",
            "KPKMBBMMMMBBMKPK",
            "KPKMMMMTTMMMMKPK",
            "KSKMMMMMMMMMMKSK",
            "KKKKKKKKKKKKKKKK",
        ],
    },
    Sprite {
        key: "cd",
        label: crate::tr_noop!("Disc (CD)"),
        eye_bg: b'P',
        shade_from: 99,
        fur: None,
        ear: None,
        pixel: None,
        grid: [
            "................",
            ".....KPPPPK.....",
            "...KPCCPPCCPK...",
            "...PCPPPPPMCP...",
            "..KCPPPSSPMMCK..",
            "..PCHHSKKSPPCP..",
            "..PHHSKDDKSPPP..",
            "..PHHSKDDKSPPP..",
            "..PCHPSKKSPPCP..",
            "..KCPPPSSPPPCK..",
            "...PCPPPPPPCP...",
            "...KPCCPPCCPK...",
            ".....KPPPPK.....",
            "................",
        ],
    },
    // ---- the 20 critters of the Den (from the design "Toca — 20 mascotes") ----
    Sprite {
        key: "rato",
        label: crate::tr_noop!("Network rat"),
        eye_bg: b'.',
        shade_from: 99,
        fur: None,
        ear: None,
        // network: gnaws on lost packets and warns when the connection drops.
        pixel: Some(Pixel { eye: 0x22263A, pal: &[(b'c', 0x3DF2FF), (b'g', 0xA8B0C0), (b'k', 0x22263A), (b'p', 0xFF9EC4)] }),
        grid: [
            "................",
            "................",
            "...kk......kk...",
            "..kppk....kppk..",
            "..kppkkkkkkppk..",
            "...kggggggggk...",
            "...kgeggggegk...",
            "...kggggggggk...",
            "...kgggppgggk...",
            "....kggggggk....",
            ".....kkkkkk.cc..",
            "............c...",
            "...........cc...",
            "................",
        ],
    },
    Sprite {
        key: "coruja",
        label: crate::tr_noop!("On-call owl"),
        eye_bg: b'.',
        shade_from: 99,
        fur: None,
        ear: None,
        // notifications: stays up for you and hoots softly when something arrives.
        pixel: Some(Pixel { eye: 0x1A1228, pal: &[(b'b', 0x7A5A8C), (b'k', 0x1A1228), (b'l', 0xC7A9D9), (b'o', 0xFFC46B), (b'w', 0xF5F0FF), (b'y', 0xFFC46B)] }),
        grid: [
            "................",
            "....k......k....",
            "...kbk....kbk...",
            "...kbbkkkkbbk...",
            "..kbbbbbbbbbbk..",
            "..kbwwbbbbwwbk..",
            "..kbwebbbbwebk..",
            "..kbwwbyybwwbk..",
            "..kbbbbyybbbbk..",
            "..kbllllllllbk..",
            "...kbllllllbk...",
            "....kkookkook...",
            "................",
            "................",
        ],
    },
    Sprite {
        key: "polvo",
        label: crate::tr_noop!("Cable octopus"),
        eye_bg: b'.',
        shade_from: 99,
        fur: None,
        ear: None,
        // processes: a tentacle on each task running in the background.
        pixel: Some(Pixel { eye: 0x0B2E2A, pal: &[(b'c', 0xFF3DB8), (b'k', 0x0B2E2A), (b'm', 0x1E7A6A), (b't', 0x3FD0B5)] }),
        grid: [
            "................",
            "................",
            "......kkkk......",
            ".....kttttk.....",
            "....kttttttk....",
            "...kttttttttk...",
            "...ktettttetk...",
            "...kttttttttk...",
            "...ktttmmtttk...",
            "....tttttttt....",
            "...tt.t..t.tt...",
            "..t...t..t...t..",
            ".....c....c.....",
            "................",
        ],
    },
    Sprite {
        key: "fantasma",
        label: crate::tr_noop!("Cache ghost"),
        eye_bg: b'.',
        shade_from: 99,
        fur: None,
        ear: None,
        // cleanup: haunts temporary files until you let it take them away.
        pixel: Some(Pixel { eye: 0x3A4A7A, pal: &[(b'k', 0x3A4A7A), (b'p', 0xFFB3D1), (b'w', 0xEAF2FF)] }),
        grid: [
            "................",
            "................",
            "......kkkk......",
            ".....kwwwwk.....",
            "....kwwwwwwk....",
            "...kwwwwwwwwk...",
            "...kwewwwwewk...",
            "...kwpwwwwpwk...",
            "...kwwwwwwwwk...",
            "...kwwwwwwwwk...",
            "...kwwwwwwwwk...",
            "...kwkwwkwwkwk..",
            "...k.kk..kk.k...",
            "................",
        ],
    },
    Sprite {
        key: "capivara",
        label: crate::tr_noop!("Zen capybara"),
        eye_bg: b'.',
        shade_from: 99,
        fur: None,
        ear: None,
        // focus: nothing shakes it. Sits by your side during the pomodoro.
        pixel: Some(Pixel { eye: 0x2E1A0E, pal: &[(b'b', 0xA86F45), (b'd', 0x5E3A22), (b'k', 0x2E1A0E), (b'o', 0xFF9A3C)] }),
        grid: [
            "................",
            "................",
            "......oo........",
            ".....oooo.......",
            "....kkkkkkkk....",
            "...kbbbbbbbbk...",
            "..kbbbbbbbbbbk..",
            "..kbebbbbbbebk..",
            "..kbbbbbbbbbbk..",
            "..kbbbbddbbbbk..",
            "...kbbbbbbbbk...",
            "...kbk.kk.kbk...",
            "....k......k....",
            "................",
        ],
    },
    Sprite {
        key: "tartaruga",
        label: crate::tr_noop!("Disk turtle"),
        eye_bg: b'.',
        shade_from: 99,
        fur: None,
        ear: None,
        // disk / backup: slow and steady, keeps every backup in its shell.
        pixel: Some(Pixel { eye: 0x1B2230, pal: &[(b'g', 0x6BBF6A), (b'h', 0x3DF2FF), (b'k', 0x1B2230), (b's', 0x8A93A6)] }),
        grid: [
            "................",
            "................",
            "................",
            ".....kkkkk......",
            "....ksssssk.....",
            "...kssshsssk....",
            "...ksshhhssk....",
            "..ksssshssssgg..",
            "..kkkkkkkkkgeg..",
            "...gg....gggg...",
            "...gg....gg.....",
            "................",
            "................",
            "................",
        ],
    },
    Sprite {
        key: "abelha",
        label: crate::tr_noop!("Bit bee"),
        eye_bg: b'.',
        shade_from: 99,
        fur: None,
        ear: None,
        // cpu: buzzes faster the more cores are busy.
        pixel: Some(Pixel { eye: 0x2A1E05, pal: &[(b'k', 0x2A1E05), (b'w', 0xCFF4FF), (b'y', 0xFFD23F)] }),
        grid: [
            "................",
            "................",
            "....ww....ww....",
            "...wwww..wwww...",
            "....ww.kk.ww....",
            "......kyyk......",
            ".....kyyyyk.....",
            "....kyeyyeyk....",
            "....kkkkkkkk....",
            "....kyyyyyyk....",
            "....kkkkkkkk....",
            ".....kyyyyk.....",
            "......kkk.......",
            "................",
        ],
    },
    Sprite {
        key: "caracol",
        label: crate::tr_noop!("Download snail"),
        eye_bg: b'.',
        shade_from: 99,
        fur: None,
        ear: None,
        // downloads: carries the file on its back. Sometimes slow, but it gets there.
        pixel: Some(Pixel { eye: 0x2C1A28, pal: &[(b'a', 0xFFFFFF), (b'b', 0x9EE29A), (b'k', 0x2C1A28), (b's', 0xFF8FB8)] }),
        grid: [
            "................",
            "................",
            "...e.e..........",
            "...k.k..kkkk....",
            "...kbk.kssssk...",
            "...kbk.ksaask...",
            "...kbkksaaaask..",
            "...kbbkssaassk..",
            "...kbbbkssssk...",
            "..kbbbbbbbbbk...",
            "...kkkkkkkkkk...",
            "................",
            "................",
            "................",
        ],
    },
    Sprite {
        key: "coelho",
        label: crate::tr_noop!("Clock rabbit"),
        eye_bg: b'.',
        shade_from: 99,
        fur: None,
        ear: None,
        // reminders: always late, so it never lets you be late.
        pixel: Some(Pixel { eye: 0x3A3550, pal: &[(b'c', 0xFFE08A), (b'k', 0x3A3550), (b'n', 0x3A2A10), (b'p', 0xFFB3C7), (b'w', 0xF4F4F8)] }),
        grid: [
            "................",
            "....kk....kk....",
            "....kpk..kpk....",
            "....kpk..kpk....",
            "....kwkkkkwk....",
            "...kwwwwwwwwk...",
            "...kwewwwwewk...",
            "...kwwwppwwwk...",
            "....kwwwwwwk....",
            "...kwwcccccwk...",
            "...kwwcncccwk...",
            "....kwwcccwk....",
            ".....kkkkkk.....",
            "................",
        ],
    },
    Sprite {
        key: "ourico",
        label: crate::tr_noop!("Doorman hedgehog"),
        eye_bg: b'.',
        shade_from: 99,
        fur: None,
        ear: None,
        // firewall: bristles its spines at any strange connection.
        pixel: Some(Pixel { eye: 0x2A1A12, pal: &[(b'f', 0xF2D6B3), (b'k', 0x2A1A12), (b'n', 0x2A1A12), (b's', 0x6E5A4A)] }),
        grid: [
            "................",
            "................",
            "...k..k..k..k...",
            "..kskkskkskksk..",
            "..ssssssssssss..",
            "..ssffffffffss..",
            "..sffeffffeffs..",
            "..sffffffffffs..",
            "..sffffnnffffs..",
            "..ssffffffffss..",
            "...ssssssssss...",
            "....kk....kk....",
            "................",
            "................",
        ],
    },
    Sprite {
        key: "chaleira",
        label: crate::tr_noop!("Kettle"),
        eye_bg: b'.',
        shade_from: 99,
        fur: None,
        ear: None,
        // breaks: whistles when it is time to get up and make some tea.
        pixel: Some(Pixel { eye: 0x3E2A1E, pal: &[(b'k', 0x3E2A1E), (b'm', 0x7A2E1E), (b'o', 0xF9D9A8), (b't', 0xE27A5B), (b'v', 0xCFE3FF)] }),
        grid: [
            "................",
            "...........v....",
            "......kk..v.....",
            ".....kook.......",
            "....kkkkkkkk....",
            "...kttttttttk...",
            "...ktettttetkk..",
            "...ktttmmtttkt..",
            "...kttttttttk...",
            "....kkkkkkkk....",
            "................",
            "................",
            "................",
            "................",
        ],
    },
    Sprite {
        key: "cogumelo",
        label: crate::tr_noop!("Sleepy mushroom"),
        eye_bg: b'.',
        shade_from: 99,
        fur: None,
        ear: None,
        // sleep mode: grows in the dark and yawns when the PC goes to sleep.
        pixel: Some(Pixel { eye: 0x3A1A1A, pal: &[(b'f', 0xF6E3C8), (b'k', 0x3A1A1A), (b'm', 0xB87A6A), (b'r', 0xE2475B), (b'w', 0xFFF3E8)] }),
        grid: [
            "................",
            "................",
            ".....kkkkkk.....",
            "....krrwrrrk....",
            "...krwrrrrwrk...",
            "..krrrrwrrrrrk..",
            "..kkkkkkkkkkkk..",
            "....kffffffk....",
            "....kfeffefk....",
            "....kffffffk....",
            "....kffmmffk....",
            ".....kkkkkk.....",
            "................",
            "................",
        ],
    },
    Sprite {
        key: "gatopao",
        label: crate::tr_noop!("Bread cat"),
        eye_bg: b'.',
        shade_from: 99,
        fur: None,
        ear: None,
        // to-dos: sleeps on top of the list. Wakes up when everything is crossed off.
        pixel: Some(Pixel { eye: 0x5A3418, pal: &[(b'k', 0x3A2210), (b'l', 0x5A3418), (b'o', 0xF2A65A), (b'p', 0xFF9EB0), (b'w', 0xFFF1DE)] }),
        grid: [
            "................",
            "................",
            "................",
            "....k......k....",
            "...kok....kok...",
            "...koookkoook...",
            "..kooooooooook..",
            "..koleooooleok..",
            "..kooooppooook..",
            "..kooooooooook..",
            "..kowwoooowwok..",
            "...kkkkkkkkkk...",
            "................",
            "................",
        ],
    },
    Sprite {
        key: "slime",
        label: crate::tr_noop!("RAM slime"),
        eye_bg: b'.',
        shade_from: 99,
        fur: None,
        ear: None,
        // memory: stretches and swells as memory fills up.
        pixel: Some(Pixel { eye: 0x173A12, pal: &[(b'g', 0x8BE36B), (b'k', 0x173A12), (b'm', 0x2F6B22), (b'w', 0xE9FFD9)] }),
        grid: [
            "................",
            "................",
            "................",
            "................",
            ".......kk.......",
            "......kggk......",
            ".....kgwggk.....",
            "....kgggggggk...",
            "...kgeggggegk...",
            "..kggggggggggk..",
            "..kgggggmmgggk..",
            "..kggggggggggk..",
            "...kkkkkkkkkk...",
            "................",
        ],
    },
    Sprite {
        key: "morcego",
        label: crate::tr_noop!("Night bat"),
        eye_bg: b'.',
        shade_from: 99,
        fur: None,
        ear: None,
        // dark mode: shows up at sunset and turns off the screen lights.
        pixel: Some(Pixel { eye: 0x140F2E, pal: &[(b'b', 0x6B5BD6), (b'f', 0xFFFFFF), (b'k', 0x140F2E), (b'w', 0x3A2F7A)] }),
        grid: [
            "................",
            "................",
            "................",
            ".....k....k.....",
            ".....kk..kk.....",
            "..k..kbbbbk..k..",
            "..kk.kebbek.kk..",
            "..kwkkbbbbkkwk..",
            "..kwwkbffbkwwk..",
            "..kwwwkbbkwwwk..",
            "...kwk.kk.kwk...",
            "....k......k....",
            "................",
            "................",
        ],
    },
    Sprite {
        key: "peixe",
        label: crate::tr_noop!("Aquarium fish"),
        eye_bg: b'.',
        shade_from: 99,
        fur: None,
        ear: None,
        // hydration: lets out a bubble every hour: drink a glass of water.
        pixel: Some(Pixel { eye: 0x0E2A4A, pal: &[(b'b', 0xCFF4FF), (b'f', 0x4FB3FF), (b'k', 0x0E2A4A)] }),
        grid: [
            "................",
            "................",
            "...b............",
            "....b...........",
            "................",
            ".....kkkkk..kk..",
            "....kfffffkkfk..",
            "...kfefffffffk..",
            "...kfffffffkfk..",
            "....kfffffkkfk..",
            ".....kkkkk..kk..",
            "................",
            "................",
            "................",
        ],
    },
    Sprite {
        key: "nuvem",
        label: crate::tr_noop!("Little cloud"),
        eye_bg: b'.',
        shade_from: 99,
        fur: None,
        ear: None,
        // weather: rains when it is going to rain and disappears when it is sunny.
        pixel: Some(Pixel { eye: 0x5A6A9A, pal: &[(b'k', 0x5A6A9A), (b'm', 0x5A6A9A), (b'p', 0xFFC0D6), (b'r', 0x6FB8FF), (b'w', 0xF4F7FF)] }),
        grid: [
            "................",
            "................",
            "................",
            "......kkkk......",
            "....kkwwwwk.....",
            "...kwwwwwwwkk...",
            "..kwwwwwwwwwwk..",
            "..kwwewwwwewwk..",
            "..kwpwwmmwwpwk..",
            "...kkkkkkkkkk...",
            "....r..r..r.....",
            "...r..r..r......",
            "................",
            "................",
        ],
    },
    Sprite {
        key: "sapodj",
        label: crate::tr_noop!("DJ frog"),
        eye_bg: b'.',
        shade_from: 99,
        fur: None,
        ear: None,
        // music: taps its foot to the beat of what is playing now.
        pixel: Some(Pixel { eye: 0x143A10, pal: &[(b'g', 0x6BCB5A), (b'h', 0xFF3DB8), (b'k', 0x143A10), (b'w', 0xFFFFFF)] }),
        grid: [
            "................",
            "................",
            ".....hhhhhh.....",
            "....h......h....",
            "...hhkwk.kwkhh..",
            "...hkwek.kewkh..",
            "...kggggggggk...",
            "..kggggggggggk..",
            "..kggkkkkkkggk..",
            "..kggggggggggk..",
            "...kggk..kggk...",
            "................",
            "................",
            "................",
        ],
    },
    Sprite {
        key: "vagalume",
        label: crate::tr_noop!("Firefly"),
        eye_bg: b'.',
        shade_from: 99,
        fur: None,
        ear: None,
        // alerts: flashes its lantern when something needs your attention.
        pixel: Some(Pixel { eye: 0x10122A, pal: &[(b'b', 0x3A3F6B), (b'k', 0x10122A), (b'l', 0xFFE36B), (b'w', 0xCFE3FF)] }),
        grid: [
            "................",
            "................",
            ".....k....k.....",
            "......k..k......",
            ".....kkkkkk.....",
            "....kbbbbbbk....",
            "....kbebbebk....",
            "..ww.kbbbbk.ww..",
            "..www.kkkk.www..",
            "......kllk......",
            ".....kllllk.....",
            "......kllk......",
            ".......kk.......",
            "................",
        ],
    },
    Sprite {
        key: "pato",
        label: crate::tr_noop!("Rubber duck"),
        eye_bg: b'.',
        shade_from: 99,
        fur: None,
        ear: None,
        // debug / logs: listens to you explain the bug until the fix shows up.
        pixel: Some(Pixel { eye: 0x4A3200, pal: &[(b'k', 0x4A3200), (b'o', 0xFF8A3C), (b'w', 0xFFE88A), (b'y', 0xFFD23F)] }),
        grid: [
            "................",
            "................",
            "......kkkk......",
            ".....kyyyyk.....",
            "....kyeyyyyk....",
            "...ooyyyyyyk....",
            "....kyyyyyk.....",
            "...kyyyyyyykk...",
            "..kyyyyyyyyyyk..",
            "..kyyywwyyyyyk..",
            "...kyyyyyyyyk...",
            "....kkkkkkkk....",
            "................",
            "................",
        ],
    },
];

/// Sprites from files (`file.rs`, `sprites/<key>.toml`), after the built-in ones.
/// `file::reload` replaces them.
static FILES: RwLock<Vec<&'static Sprite>> = RwLock::new(Vec::new());
/// Imported mascots (`pack.rs`, PNG), after the sprites. `pack::reload` replaces them.
static EXTRA: RwLock<Vec<&'static Sprite>> = RwLock::new(Vec::new());
/// Changes every time the file or imported ones are replaced: drawing caches (pickers)
/// rebuild.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Replaces the sprites from files.
pub fn set_files(list: Vec<&'static Sprite>) {
    if let Ok(mut files) = FILES.write() {
        *files = list;
    }
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Replaces the imported mascots.
pub fn set_extra(list: Vec<&'static Sprite>) {
    if let Ok(mut extra) = EXTRA.write() {
        *extra = list;
    }
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Number of the current list (see `GENERATION`).
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// Every mascot: the built-in ones, the sprites from files, then the imported ones. This
/// is the pickers' order.
pub fn all() -> Vec<&'static Sprite> {
    let files = FILES.read().map(|e| e.clone()).unwrap_or_default();
    let extra = EXTRA.read().map(|e| e.clone()).unwrap_or_default();
    SPRITES.iter().chain(files).chain(extra).collect()
}

pub fn find(key: &str) -> Option<&'static Sprite> {
    if let Some(s) = SPRITES.iter().find(|s| s.key == key) {
        return Some(s);
    }
    if let Some(s) = FILES.read().ok().and_then(|f| f.iter().copied().find(|s| s.key == key)) {
        return Some(s);
    }
    EXTRA.read().ok()?.iter().copied().find(|s| s.key == key)
}

/// The mascot `key`, or the first one if it doesn't exist (an imported one that left the
/// folder).
pub fn get(key: &str) -> &'static Sprite {
    find(key).unwrap_or(&SPRITES[0])
}

/// The options of a choice are mascots: all are mascot keys (or "auto"), and there is at
/// least one. This is how the hub recognizes a mascot option in a manifest.
pub fn is_choice(options: &[String]) -> bool {
    let sprite = |o: &String| find(o).is_some();
    options.iter().any(sprite) && options.iter().all(|o| o == "auto" || sprite(o))
}
