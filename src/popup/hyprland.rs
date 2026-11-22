//! The popup on Hyprland, through the compositor's own socket.
//!
//! Hyprland lets a client ask for what Wayland itself does not: where a window opens. So
//! here the hub stays a native Wayland client (no XWayland) and talks to
//! `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock`:
//!
//! - before the popup is mapped, it registers a window rule (floating, on every workspace, at
//!   the position next to the tray), so the window is born in place, with no jump;
//! - a window already on screen is moved, focused and tagged by its address;
//! - monitors, their reserved areas (the bars) and the pointer come from the same socket;
//! - while the popup is open, the keyboard focus only moves on a click (`hold_focus`), so the
//!   popup closes when the user clicks outside, not when the pointer merely leaves it.
//!
//! Everything here is in Hyprland's layout coordinates: logical pixels. It needs the Lua
//! API (`eval`, Hyprland 0.56 or newer); older versions keep the XWayland path.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;

use super::{Kind, Monitor, Rect};

/// A local socket answers in well under a millisecond; this only bounds a hung compositor.
const TIMEOUT: Duration = Duration::from_millis(300);

/// Whether this session uses the Hyprland backend (decided once, in `detect`).
static ACTIVE: OnceLock<bool> = OnceLock::new();

/// The tag of each kind of window (`match = { tag = "miniutils-popup" }` in a rule).
const TAGS: [(Kind, &str); 3] = [(Kind::Popup, "miniutils-popup"), (Kind::App, "miniutils-app"), (Kind::Detail, "miniutils-detail")];

/// Checks, once, whether Hyprland runs this session and answers the Lua API.
pub fn detect() -> bool {
    *ACTIVE.get_or_init(|| {
        if std::env::var_os("WAYLAND_DISPLAY").is_none() || socket_path().is_none() {
            return false;
        }
        match request("eval local _ = hl.window_rule") {
            Ok(reply) if reply == "ok" => {
                log::info!("Hyprland: native Wayland, the popup is placed through its socket");
                true
            }
            Ok(reply) => {
                log::info!("Hyprland without the Lua API ({reply}): using XWayland");
                false
            }
            Err(e) => {
                log::info!("Hyprland socket did not answer ({e}): using XWayland");
                false
            }
        }
    })
}

/// The Hyprland backend is in use.
pub fn active() -> bool {
    ACTIVE.get().copied().unwrap_or(false)
}

fn socket_path() -> Option<PathBuf> {
    let signature = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").filter(|s| !s.is_empty())?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").filter(|s| !s.is_empty())?;
    Some(PathBuf::from(runtime).join("hypr").join(signature).join(".socket.sock"))
}

/// One request, one answer: Hyprland closes the connection after replying.
fn request(command: &str) -> anyhow::Result<String> {
    let path = socket_path().ok_or_else(|| anyhow::anyhow!("not a Hyprland session"))?;
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    stream.write_all(command.as_bytes())?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    Ok(reply.trim().to_string())
}

/// Runs a request that answers `ok`; anything else goes to the log.
fn run(command: &str) -> bool {
    match request(command) {
        Ok(reply) if reply == "ok" => true,
        Ok(reply) => {
            log::warn!("Hyprland refused `{command}`: {reply}");
            false
        }
        Err(e) => {
            log::warn!("Hyprland did not answer `{command}`: {e}");
            false
        }
    }
}

fn query<T: serde::de::DeserializeOwned>(what: &str) -> Option<T> {
    let reply = request(&format!("j/{what}")).map_err(|e| log::warn!("Hyprland did not answer {what}: {e}")).ok()?;
    serde_json::from_str(&reply).map_err(|e| log::warn!("unexpected {what} from Hyprland: {e}")).ok()
}

/// A monitor as `j/monitors` reports it: position in the layout (logical), mode in physical pixels.
#[derive(Debug, Deserialize)]
struct RawMonitor {
    name: String,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    scale: f64,
    #[serde(default)]
    transform: i32,
    /// Space taken by the bars: left, top, right, bottom (logical).
    #[serde(default)]
    reserved: [i32; 4],
    #[serde(default)]
    focused: bool,
    #[serde(default)]
    disabled: bool,
}

/// The monitors in logical pixels (scale 1, so `position_for` converts nothing), with
/// their names, and the index of the focused one.
fn layout(raw: &[RawMonitor]) -> (Vec<(String, Monitor)>, usize) {
    let mut monitors = Vec::new();
    let mut focused = 0;
    for m in raw.iter().filter(|m| !m.disabled) {
        let scale = if m.scale > 0.0 { m.scale } else { 1.0 };
        let (mut w, mut h) = ((m.width as f64 / scale).round() as i32, (m.height as f64 / scale).round() as i32);
        // odd transforms are the 90° and 270° rotations
        if m.transform % 2 == 1 {
            std::mem::swap(&mut w, &mut h);
        }
        let [left, top, right, bottom] = m.reserved;
        let work = Rect { x: m.x + left, y: m.y + top, w: (w - left - right).max(1), h: (h - top - bottom).max(1) };
        if m.focused {
            focused = monitors.len();
        }
        monitors.push((m.name.clone(), Monitor { x: m.x, y: m.y, w, h, scale: 1.0, work }));
    }
    (monitors, focused)
}

thread_local! {
    /// The user's `input:follow_mouse`, while `hold_focus` has it replaced.
    static FOLLOW_MOUSE: std::cell::Cell<Option<i64>> = const { std::cell::Cell::new(None) };
}

/// `input:follow_mouse` value that moves the keyboard focus only on a click.
const FOCUS_ON_CLICK: i64 = 2;

/// With `on`, the keyboard focus stops following the pointer (Hyprland's default): a popup
/// hides when it loses the focus, and it should take a click outside for that, not the
/// pointer passing over another window. Without `on`, the user's setting comes back. The
/// option is global, so it is held only while the popup is on screen; reloading Hyprland's
/// config in the meantime also restores it.
pub fn hold_focus(on: bool) {
    #[derive(Deserialize)]
    struct Value {
        int: i64,
    }
    let set = |v: i64| run(&format!("eval hl.config({{ input = {{ follow_mouse = {v} }} }})"));
    if !on {
        if let Some(saved) = FOLLOW_MOUSE.take() {
            set(saved);
        }
        return;
    }
    if FOLLOW_MOUSE.get().is_some() {
        return; // already held
    }
    if let Some(now) = query::<Value>("getoption input:follow_mouse")
        && now.int != FOCUS_ON_CLICK
        && set(FOCUS_ON_CLICK)
    {
        FOLLOW_MOUSE.set(Some(now.int));
    }
}

/// Where the pointer is (layout coordinates): the anchor when the tray host gives none.
pub fn pointer() -> Option<(i32, i32)> {
    #[derive(Deserialize)]
    struct Pos {
        x: i32,
        y: i32,
    }
    query::<Pos>("cursorpos").map(|p| (p.x, p.y))
}

#[derive(Debug, Deserialize)]
struct Client {
    address: String,
    pid: i64,
    title: String,
    at: [i32; 2],
    #[serde(default)]
    floating: bool,
    #[serde(default)]
    pinned: bool,
}

/// Our window with this title, if it is on screen.
fn client(title: &str) -> Option<Client> {
    let pid = i64::from(std::process::id());
    query::<Vec<Client>>("clients")?.into_iter().find(|c| c.pid == pid && c.title == title)
}

/// Top-left corner of our window with this title (layout coordinates).
pub fn position(title: &str) -> Option<(i32, i32)> {
    client(title).map(|c| (c.at[0], c.at[1]))
}

/// Gives our window the keyboard focus. `false` when it is not on screen.
pub fn focus(title: &str) -> bool {
    client(title).is_some_and(|c| run(&format!("dispatch hl.dsp.focus({{ window = {} }})", lua_string(&format!("address:{}", c.address)))))
}

/// Tags our window as `kind` (and drops the other kinds' tags).
pub fn tag(title: &str, kind: Kind) {
    let Some(c) = client(title) else { return };
    let window = lua_string(&format!("address:{}", c.address));
    for (k, name) in TAGS {
        let sign = if k == kind { '+' } else { '-' };
        run(&format!("dispatch hl.dsp.window.tag({{ window = {window}, tag = \"{sign}{name}\" }})"));
    }
}

/// Where the hub window goes and how it behaves there.
pub struct Request<'a> {
    /// The window title (how the rule and the lookups find the window).
    pub title: &'a str,
    /// The app id (class) of the window.
    pub class: &'a str,
    /// Logical size of the window; `None` while unknown (never shown).
    pub size: Option<(i32, i32)>,
    /// Desktop app: a normal, tiled window. Otherwise the tray popup.
    pub desktop: bool,
    /// Global position to move to (see `target`); `None` leaves the window where it is.
    pub target: Option<(i32, i32)>,
}

/// Position for the popup: `position_for` over Hyprland's monitors.
pub fn target(place: super::Place, anchor: Option<(i32, i32)>, size: (i32, i32), last: Option<(i32, i32)>) -> Option<(i32, i32)> {
    let raw: Vec<RawMonitor> = query("monitors")?;
    let (named, focused) = layout(&raw);
    let monitors: Vec<Monitor> = named.iter().map(|(_, m)| *m).collect();
    super::position_for(place, anchor, true, size, &monitors, focused, last)
}

/// Makes the window match `req`: registers the rule the next mapping will follow and, if the
/// window is on screen now, applies the same to it.
pub fn apply(req: &Request, rule_name: &str) {
    let on_monitor = req.target.and_then(|pos| {
        let raw: Vec<RawMonitor> = query("monitors")?;
        let (named, _) = layout(&raw);
        let (w, h) = req.size?;
        monitor_of(&named, pos, (w, h)).map(|(name, m)| (name.to_string(), (pos.0 - m.x, pos.1 - m.y)))
    });
    run(&format!("eval {}", rule(rule_name, req.class, req.title, req.desktop, on_monitor.as_ref().map(|(n, p)| (n.as_str(), *p)))));
    let Some(c) = client(req.title) else { return };
    let window = lua_string(&format!("address:{}", c.address));
    // only what differs is asked for: Hyprland refuses to pin what is already pinned
    if req.desktop {
        if c.pinned {
            run(&format!("dispatch hl.dsp.window.pin({{ window = {window}, action = \"unset\" }})"));
        }
        if c.floating {
            run(&format!("dispatch hl.dsp.window.float({{ window = {window}, action = \"unset\" }})"));
        }
        return;
    }
    if !c.floating {
        run(&format!("dispatch hl.dsp.window.float({{ window = {window}, action = \"set\" }})"));
    }
    if !c.pinned {
        run(&format!("dispatch hl.dsp.window.pin({{ window = {window}, action = \"set\" }})"));
    }
    if let Some((x, y)) = req.target
        && (x, y) != (c.at[0], c.at[1])
    {
        run(&format!("dispatch hl.dsp.window.move({{ window = {window}, x = {x}, y = {y}, relative = false }})"));
    }
}

/// The monitor holding the center of a `size` window at `pos` (or its corner).
fn monitor_of(named: &[(String, Monitor)], pos: (i32, i32), size: (i32, i32)) -> Option<(&str, &Monitor)> {
    let on = |p: (i32, i32)| named.iter().find(|(_, m)| super::contains(m, p));
    on((pos.0 + size.0 / 2, pos.1 + size.1 / 2)).or_else(|| on(pos)).map(|(n, m)| (n.as_str(), m))
}

/// Lua for the popup's window rule. Registering a rule with the same name replaces it, so
/// each call updates where the next mapping lands. `on_monitor`: the monitor and the position
/// inside it; a desktop app turns the rule off (it opens as any window).
fn rule(name: &str, class: &str, title: &str, desktop: bool, on_monitor: Option<(&str, (i32, i32))>) -> String {
    let matcher = matcher(class, title);
    let name = lua_string(name);
    if desktop {
        return format!("hl.window_rule({{ name = {name}, enabled = false, {matcher} }})");
    }
    let place = match on_monitor {
        Some((monitor, (x, y))) => format!(", monitor = {}, move = \"{x} {y}\"", lua_string(monitor)),
        None => String::new(),
    };
    format!("hl.window_rule({{ name = {name}, {matcher}, float = true, pin = true{place} }})")
}

/// The `match` of a rule for our window with this class and title.
fn matcher(class: &str, title: &str) -> String {
    format!("match = {{ class = {}, title = {} }}", lua_string(&format!("^{}$", regex_escape(class))), lua_string(&format!("^{}$", regex_escape(title))))
}

/// Registers a rule so the next mapping of our window `title` floats at `size` (logical),
/// centered, instead of being tiled. Registering again under `name` replaces it.
pub fn float_rule(name: &str, class: &str, title: &str, size: (i32, i32)) {
    let (w, h) = size;
    run(&format!(
        "eval hl.window_rule({{ name = {}, {}, float = true, size = \"{w} {h}\", center = true }})",
        lua_string(name),
        matcher(class, title)
    ));
}

/// `s` as a Lua string literal.
fn lua_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `s` as a regular expression that matches only itself.
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if r"\.^$|()[]{}*+?".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONITORS: &str = r#"[
        {"id":0,"name":"DP-2","x":0,"y":0,"width":2560,"height":1440,"scale":1.0,"transform":0,
         "reserved":[0,0,0,38],"focused":false,"disabled":false},
        {"id":1,"name":"HDMI-A-1","x":2560,"y":0,"width":3840,"height":2160,"scale":1.5,"transform":1,
         "reserved":[0,30,0,0],"focused":true},
        {"id":2,"name":"off","x":9000,"y":0,"width":800,"height":600,"scale":1.0,"disabled":true}
    ]"#;

    fn monitors() -> (Vec<(String, Monitor)>, usize) {
        layout(&serde_json::from_str::<Vec<RawMonitor>>(MONITORS).unwrap())
    }

    #[test]
    fn layout_is_logical_with_the_bars_taken_out() {
        let (m, focused) = monitors();
        assert_eq!(m.len(), 2, "a disabled monitor is not part of the layout");
        assert_eq!(focused, 1);
        assert_eq!(m[0].0, "DP-2");
        assert_eq!(m[0].1, Monitor { x: 0, y: 0, w: 2560, h: 1440, scale: 1.0, work: Rect { x: 0, y: 0, w: 2560, h: 1402 } });
        // 3840x2160 at 1.5 is 2560x1440 logical; rotated 90°, it is 1440 wide and 2560 tall
        assert_eq!(m[1].1, Monitor { x: 2560, y: 0, w: 1440, h: 2560, scale: 1.0, work: Rect { x: 2560, y: 30, w: 1440, h: 2530 } });
    }

    #[test]
    fn popup_opens_above_a_bottom_bar_next_to_the_pointer() {
        let (named, _) = monitors();
        let m: Vec<Monitor> = named.iter().map(|(_, m)| *m).collect();
        // pointer on the tray, in the 38 px bar at the bottom of the first monitor
        let pos = super::super::position_for(super::super::Place::Tray, Some((2400, 1421)), true, (400, 500), &m, 1, None).unwrap();
        // half a panel and the gap above the click; against the right edge
        assert_eq!(pos, (2560 - 10 - 400, 1421 - 24 - 10 - 500));
        let (name, on) = monitor_of(&named, pos, (400, 500)).unwrap();
        assert_eq!((name, pos.0 - on.x, pos.1 - on.y), ("DP-2", 2150, 887));
        // on the second monitor, the rule gets the position inside it
        let (name, on) = monitor_of(&named, (2600, 100), (400, 500)).unwrap();
        assert_eq!((name, 2600 - on.x), ("HDMI-A-1", 40));
    }

    #[test]
    fn rule_places_the_popup_and_is_switched_off_for_a_desktop_app() {
        assert_eq!(
            rule("miniutils-popup", "miniutils", "MiniUtils", false, Some(("DP-2", (2150, 892)))),
            r#"hl.window_rule({ name = "miniutils-popup", match = { class = "^miniutils$", title = "^MiniUtils$" }, float = true, pin = true, monitor = "DP-2", move = "2150 892" })"#
        );
        // size still unknown: floats, and is moved once it is on screen
        assert_eq!(
            rule("miniutils-popup", "miniutils", "MiniUtils", false, None),
            r#"hl.window_rule({ name = "miniutils-popup", match = { class = "^miniutils$", title = "^MiniUtils$" }, float = true, pin = true })"#
        );
        assert_eq!(
            rule("miniutils-popup", "miniutils", "MiniUtils", true, Some(("DP-2", (1, 2)))),
            r#"hl.window_rule({ name = "miniutils-popup", enabled = false, match = { class = "^miniutils$", title = "^MiniUtils$" } })"#
        );
    }

    #[test]
    fn a_hub_name_with_special_characters_stays_a_literal() {
        // the hub name is free text (config `name`): it must not end the Lua string or act
        // as a regular expression
        let code = rule("r", "miniutils", r#"My "hub" (v1.0)\"#, false, None);
        assert!(code.contains(r#"title = "^My \"hub\" \\(v1\\.0\\)\\\\$""#), "{code}");
        assert_eq!(lua_string("a\nb"), "\"a\\nb\"");
        assert_eq!(regex_escape("a.b*c"), r"a\.b\*c");
    }

}
