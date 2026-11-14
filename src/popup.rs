//! Tray popup behavior: open next to the icon, out of the taskbar.
//!
//! Wayland does not let an app position its own window. So, when XWayland is available
//! (`$DISPLAY`), the hub runs as an X11 client — the same applies directly in X11 sessions
//! (XFCE, Cinnamon, MATE). With `MINIUTILS_NATIVE_WAYLAND=1` it stays on native Wayland:
//! frameless and closing when it loses focus, but wherever the compositor puts it.
//!
//! On Windows the window positions itself freely; what changes is how to ask for "out of
//! the taskbar" and the fade opacity (see the `#[cfg(windows)]` blocks).

use slint::winit_030::{WinitWindowAccessor, winit};
use winit::dpi::PhysicalPosition;

/// Distance between the popup and the clicked point / screen edges.
const GAP: i32 = 10;
/// Half of a typical panel: the click lands in its middle.
const PANEL_HALF: i32 = 24;

/// Picks the backend (winit + software renderer). On Windows the popup can always be
/// positioned.
#[cfg(windows)]
pub fn select_backend() -> anyhow::Result<bool> {
    slint::BackendSelector::new().backend_name("winit-software".into()).select()?;
    Ok(true)
}

/// Picks the backend (winit + software renderer) and, if possible, X11.
/// Returns `true` when the popup can be positioned.
#[cfg(target_os = "linux")]
pub fn select_backend() -> anyhow::Result<bool> {
    let native_wayland = std::env::var_os("MINIUTILS_NATIVE_WAYLAND").is_some();
    let x11 = std::env::var_os("DISPLAY").is_some() && !native_wayland;
    let mut selector = slint::BackendSelector::new().backend_name("winit-software".into());
    if x11 {
        use winit::platform::x11::EventLoopBuilderExtX11;
        let mut builder = winit::event_loop::EventLoop::<slint::winit_030::SlintEvent>::with_user_event();
        builder.with_x11();
        selector = selector.with_winit_event_loop_builder(builder);
    }
    selector.select()?;
    Ok(x11)
}

/// Rectangle in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Monitor in physical pixels (as winit reports it) and its scale factor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Monitor {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub scale: f64,
    /// Work area (the monitor without the panels), physical. The monitor itself when unknown.
    pub work: Rect,
}

/// Monitor edge where the tray panel is.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Side {
    Bottom,
    Top,
    Left,
    Right,
}

/// The part of `work` that falls on `m`. `None` when less than half of the monitor is left
/// in some direction: then the work area does not describe this monitor (on X11 there is
/// one for the whole virtual screen, and with monitors of different heights it can be off).
fn useful_part(m: Rect, work: Rect) -> Option<Rect> {
    let x0 = m.x.max(work.x);
    let y0 = m.y.max(work.y);
    let x1 = (m.x + m.w).min(work.x + work.w);
    let y1 = (m.y + m.h).min(work.y + work.h);
    let (w, h) = (x1 - x0, y1 - y0);
    (w * 2 >= m.w && h * 2 >= m.h).then_some(Rect { x: x0, y: y0, w, h })
}

/// Sides where the work area is smaller than the monitor: where there is a panel.
fn panel_sides(m: &Monitor) -> Vec<Side> {
    let wk = m.work;
    let insets = [
        (Side::Bottom, (m.y + m.h) - (wk.y + wk.h)),
        (Side::Top, wk.y - m.y),
        (Side::Left, wk.x - m.x),
        (Side::Right, (m.x + m.w) - (wk.x + wk.w)),
    ];
    insets.into_iter().filter(|&(_, d)| d > 0).map(|(s, _)| s).collect()
}

/// Side of the panel. With a click: the edge closest to it, among those with a panel (if the
/// work area says which) — that settles a click in a corner. Without a click: the first
/// edge with a panel, or the bottom. Ties go to bottom/top, the most common panels.
fn panel_side(m: &Monitor, anchor: Option<(i32, i32)>) -> Side {
    let known = panel_sides(m);
    let Some((ax, ay)) = anchor else { return known.first().copied().unwrap_or(Side::Bottom) };
    let dist = [
        (Side::Bottom, m.y + m.h - 1 - ay),
        (Side::Top, ay - m.y),
        (Side::Left, ax - m.x),
        (Side::Right, m.x + m.w - 1 - ax),
    ];
    dist.into_iter()
        .filter(|(s, _)| known.is_empty() || known.contains(s))
        .min_by_key(|&(_, d)| d)
        .map_or(Side::Bottom, |(s, _)| s)
}

/// Fits `pos` (with `size`) between `start` and `start + len`, `gap` away from the edges.
/// If it does not fit, it sticks to the start (the top/left part, with the title, shows).
fn fit(pos: i32, size: i32, start: i32, len: i32, gap: i32) -> i32 {
    pos.clamp(start + gap, (start + len - size - gap).max(start + gap))
}

/// Does the tray report the clicked point in logical pixels (SNI, on Linux) or physical
/// ones (Windows)?
const ANCHOR_LOGICAL: bool = cfg!(not(windows));

/// Popup position with the anchor in logical pixels (see `position_with`).
#[cfg(test)]
pub fn popup_position(anchor: Option<(i32, i32)>, win: (i32, i32), monitors: &[Monitor], fallback: usize) -> Option<(i32, i32)> {
    position_with(anchor, true, win, monitors, fallback)
}

/// Physical position (top-left corner) of the popup. A pure function, for testing.
/// - `anchor`: point clicked on the tray; `anchor_logical` says whether it is in logical
///   pixels (converted by each monitor's scale) or already physical.
/// - `win`: physical window size.
/// - `fallback`: index of the monitor used without an anchor or when it is on no monitor.
pub fn position_with(
    anchor: Option<(i32, i32)>,
    anchor_logical: bool,
    win: (i32, i32),
    monitors: &[Monitor],
    fallback: usize,
) -> Option<(i32, i32)> {
    let (pw, ph) = win;
    let to_physical = |a: (i32, i32), scale: f64| if anchor_logical { to_physical(a, scale) } else { a };
    // the monitor containing the anchor (each converts the point with its own scale)
    let found = anchor.and_then(|a| {
        monitors.iter().find_map(|m| {
            let p = to_physical(a, m.scale);
            contains(m, p).then_some((m, p))
        })
    });
    let m = match found {
        Some((m, _)) => m,
        None => monitors.get(fallback)?,
    };
    // margins and half panel are in logical pixels: here they become physical
    let gap = (GAP as f64 * m.scale).round() as i32;
    let half = (PANEL_HALF as f64 * m.scale).round() as i32;
    let point = match (anchor, found) {
        (_, Some((_, p))) => Some(p),
        (Some(a), None) => Some(to_physical(a, m.scale)),
        (None, None) => None,
    };
    let wk = m.work;
    let side = panel_side(m, point);
    let (x, y) = match point {
        // opens away from the panel: the click lands in its middle, hence `half`
        Some((ax, ay)) => match side {
            Side::Bottom => (ax - pw / 2, ay - half - gap - ph),
            Side::Top => (ax - pw / 2, ay + half + gap),
            Side::Left => (ax + half + gap, ay - ph / 2),
            Side::Right => (ax - half - gap - pw, ay - ph / 2),
        },
        // no click: in the corner of the work area next to the panel (right, or left if the
        // panel is on the left); the `fit` below pushes it against the edges
        None => {
            let left = side == Side::Left;
            let x = if left { i32::MIN / 2 } else { i32::MAX / 2 };
            let y = if side == Side::Top { i32::MIN / 2 } else { i32::MAX / 2 };
            (x, y)
        }
    };
    // inside the work area: covers no panel, not even one thicker than `half`
    Some((fit(x, pw, wk.x, wk.w, gap), fit(y, ph, wk.y, wk.h, gap)))
}

fn to_physical((x, y): (i32, i32), scale: f64) -> (i32, i32) {
    ((x as f64 * scale).round() as i32, (y as f64 * scale).round() as i32)
}

fn contains(m: &Monitor, (x, y): (i32, i32)) -> bool {
    x >= m.x && x < m.x + m.w && y >= m.y && y < m.y + m.h
}

/// Puts the window next to `anchor` (point clicked on the tray, as the tray reports it).
/// Without an anchor, it goes to the bottom-right corner, where the tray usually is.
pub fn place(window: &slint::Window, anchor: Option<(i32, i32)>) {
    window.with_winit_window(|w| {
        let size = w.outer_size();
        let handles: Vec<_> = w.available_monitors().collect();
        let desktop = desktop_work_area();
        let monitors: Vec<Monitor> = handles
            .iter()
            .map(|h| {
                let (p, s) = (h.position(), h.size());
                let rect = Rect { x: p.x, y: p.y, w: s.width as i32, h: s.height as i32 };
                let work = monitor_work_area(h).or(desktop).and_then(|wk| useful_part(rect, wk)).unwrap_or(rect);
                Monitor { x: rect.x, y: rect.y, w: rect.w, h: rect.h, scale: h.scale_factor(), work }
            })
            .collect();
        // the current monitor (or the primary) is the fallback when the anchor is on none
        let current = w.current_monitor().or_else(|| w.primary_monitor());
        let fallback = current.and_then(|c| handles.iter().position(|m| m.position() == c.position())).unwrap_or(0);
        let win = (size.width as i32, size.height as i32);
        if let Some((x, y)) = position_with(anchor, ANCHOR_LOGICAL, win, &monitors, fallback) {
            w.set_outer_position(PhysicalPosition::new(x, y));
        }
    });
}

/// Work area of a monitor (without the taskbar), physical. Windows gives it per monitor.
#[cfg(windows)]
fn monitor_work_area(h: &winit::monitor::MonitorHandle) -> Option<Rect> {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO};
    use winit::platform::windows::MonitorHandleExtWindows;
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: the HMONITOR comes from winit (a monitor that exists now) and `info` has the right cbSize.
    let ok = unsafe { GetMonitorInfoW(HMONITOR(h.hmonitor() as *mut std::ffi::c_void), &mut info) };
    let r = info.rcWork;
    ok.as_bool().then_some(Rect { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top })
}

/// X11 does not give the work area per monitor (see `desktop_work_area`).
#[cfg(target_os = "linux")]
fn monitor_work_area(_: &winit::monitor::MonitorHandle) -> Option<Rect> {
    None
}

/// Work area of the whole virtual screen (`_NET_WORKAREA`, X11); each monitor takes its
/// part (`useful_part`). A panel between two monitors does not show in it.
#[cfg(target_os = "linux")]
fn desktop_work_area() -> Option<Rect> {
    x11::work_area().unwrap_or_else(|e| {
        log::debug!("no _NET_WORKAREA: {e}");
        None
    })
}

/// On Windows the area comes per monitor (`monitor_work_area`).
#[cfg(windows)]
fn desktop_work_area() -> Option<Rect> {
    None
}

/// Starts moving the (frameless) window with the mouse pressed, as with the system's
/// title bar.
pub fn drag_move(window: &slint::Window) {
    window.with_winit_window(|w| {
        if let Err(e) = w.drag_window() {
            log::warn!("could not move the window: {e}");
        }
    });
}

/// Starts resizing by edge/corner `dir` ("n", "se", …), with the mouse pressed.
pub fn drag_resize(window: &slint::Window, dir: &str) {
    use winit::window::ResizeDirection as D;
    let d = match dir {
        "n" => D::North,
        "s" => D::South,
        "e" => D::East,
        "w" => D::West,
        "ne" => D::NorthEast,
        "nw" => D::NorthWest,
        "se" => D::SouthEast,
        "sw" => D::SouthWest,
        _ => return,
    };
    window.with_winit_window(|w| {
        if let Err(e) = w.drag_resize_window(d) {
            log::warn!("could not resize: {e}");
        }
    });
}

/// Asks for focus (the popup needs it to close when the user clicks outside).
pub fn focus(window: &slint::Window) {
    window.with_winit_window(|w| w.focus_window());
}

/// The native window already exists (after the first `show`).
pub fn has_window(window: &slint::Window) -> bool {
    window.has_winit_window()
}

/// Opacity of the whole window (0..1), as a layered window (`WS_EX_LAYERED`).
#[cfg(windows)]
pub fn set_opacity(window: &slint::Window, alpha: f32) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::{COLORREF, HWND};
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, LWA_ALPHA, SetLayeredWindowAttributes, SetWindowLongPtrW, WS_EX_LAYERED,
    };
    window.with_winit_window(|w| {
        let Ok(handle) = w.window_handle() else { return };
        let RawWindowHandle::Win32(h) = handle.as_raw() else { return };
        let hwnd = HWND(h.hwnd.get() as *mut std::ffi::c_void);
        let layered = WS_EX_LAYERED.0 as isize;
        // SAFETY: `hwnd` is winit's live window, and we are on its thread.
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            if style & layered == 0 {
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | layered);
            }
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), (alpha.clamp(0.0, 1.0) * 255.0) as u8, LWA_ALPHA);
        }
    });
}

/// Takes the window out of the taskbar and keeps it on top (winit already knows how).
#[cfg(windows)]
pub fn skip_taskbar(window: &slint::Window) {
    use winit::platform::windows::WindowExtWindows;
    window.with_winit_window(|w| {
        w.set_skip_taskbar(true);
        w.set_window_level(winit::window::WindowLevel::AlwaysOnTop);
    });
}

/// Opacity of the whole window (0..1), through the compositor's `_NET_WM_WINDOW_OPACITY`.
#[cfg(target_os = "linux")]
pub fn set_opacity(window: &slint::Window, alpha: f32) {
    window.with_winit_window(|w| {
        if let Some(id) = x11_id(w) {
            let _ = x11::set_opacity(id, alpha);
        }
    });
}

#[cfg(target_os = "linux")]
fn x11_id(w: &winit::window::Window) -> Option<u32> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match w.window_handle().ok()?.as_raw() {
        RawWindowHandle::Xlib(h) => Some(h.window as u32),
        RawWindowHandle::Xcb(h) => Some(h.window.get()),
        _ => None,
    }
}

/// Takes the window out of the taskbar and the switcher and keeps it on top (EWMH, X11).
#[cfg(target_os = "linux")]
pub fn skip_taskbar(window: &slint::Window) {
    let done = window.with_winit_window(|w| {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let Ok(handle) = w.window_handle() else { return };
        let id = match handle.as_raw() {
            RawWindowHandle::Xlib(h) => h.window as u32,
            RawWindowHandle::Xcb(h) => h.window.get(),
            _ => return, // native Wayland: there is no way to ask for it
        };
        log::debug!("EWMH states on window 0x{id:x}");
        if let Err(e) = x11::set_states(id) {
            log::warn!("could not take the window out of the taskbar: {e}");
        }
    });
    if done.is_none() {
        log::debug!("winit window does not exist yet");
    }
}

#[cfg(target_os = "linux")]
mod x11 {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        AtomEnum, ClientMessageEvent, ConnectionExt, EventMask, PropMode,
    };
    use x11rb::wrapper::ConnectionExt as _;
    use x11rb::rust_connection::RustConnection;

    thread_local! {
        /// Connection reused by the fade (many opacity changes in a row).
        static CONN: Option<(RustConnection, u32)> = x11rb::connect(None).ok().and_then(|(c, _)| {
            let atom = c.intern_atom(false, b"_NET_WM_WINDOW_OPACITY").ok()?.reply().ok()?.atom;
            Some((c, atom))
        });
    }

    pub fn set_opacity(window: u32, alpha: f32) -> anyhow::Result<()> {
        CONN.with(|c| {
            let Some((conn, atom)) = c else { return Ok(()) };
            let value = (alpha.clamp(0.0, 1.0) as f64 * u32::MAX as f64) as u32;
            conn.change_property32(PropMode::REPLACE, window, *atom, AtomEnum::CARDINAL, &[value])?;
            conn.flush()?;
            Ok(())
        })
    }

    /// `_NET_WORKAREA` of the current desktop (the virtual screen minus the panels).
    pub fn work_area() -> anyhow::Result<Option<super::Rect>> {
        let (conn, screen) = x11rb::connect(None)?;
        let root = conn.setup().roots[screen].root;
        let atom = |name: &str| -> anyhow::Result<u32> { Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom) };
        let current = conn.get_property(false, root, atom("_NET_CURRENT_DESKTOP")?, AtomEnum::CARDINAL, 0, 1)?.reply()?;
        let desk = current.value32().and_then(|mut v| v.next()).unwrap_or(0);
        // one area (x, y, w, h) per desktop; the offset counts in 32-bit words
        let area = conn.get_property(false, root, atom("_NET_WORKAREA")?, AtomEnum::CARDINAL, desk.saturating_mul(4), 4)?.reply()?;
        let v: Vec<u32> = area.value32().map(Iterator::collect).unwrap_or_default();
        Ok(match v[..] {
            [x, y, w, h] if w > 0 && h > 0 => Some(super::Rect { x: x as i32, y: y as i32, w: w as i32, h: h as i32 }),
            _ => None,
        })
    }

    pub fn set_states(window: u32) -> anyhow::Result<()> {
        let (conn, screen) = x11rb::connect(None)?;
        let root = conn.setup().roots[screen].root;
        let atom = |name: &str| -> anyhow::Result<u32> { Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom) };
        let net_wm_state = atom("_NET_WM_STATE")?;
        let net_wm_desktop = atom("_NET_WM_DESKTOP")?;
        let states = [
            atom("_NET_WM_STATE_SKIP_TASKBAR")?,
            atom("_NET_WM_STATE_SKIP_PAGER")?,
            atom("_NET_WM_STATE_ABOVE")?,
            // on every desktop: without it the popup stays stuck on the first one it opened on
            atom("_NET_WM_STATE_STICKY")?,
        ];
        // 0xFFFFFFFF in _NET_WM_DESKTOP = every desktop (EWMH)
        const ALL_DESKTOPS: u32 = 0xFFFF_FFFF;
        // window not managed yet (empty property): the window manager reads it on map.
        // After that the property belongs to the window manager and must not be overwritten.
        let current = conn.get_property(false, window, net_wm_state, AtomEnum::ATOM, 0, 32)?.reply()?;
        if current.value_len == 0 {
            conn.change_property32(PropMode::REPLACE, window, net_wm_state, AtomEnum::ATOM, &states)?;
            conn.change_property32(PropMode::REPLACE, window, net_wm_desktop, AtomEnum::CARDINAL, &[ALL_DESKTOPS])?;
        }
        // window already mapped: the desktop also goes as a message to the root
        // (data: desktop, 1 = request from a normal app)
        let ev = ClientMessageEvent::new(32, window, net_wm_desktop, [ALL_DESKTOPS, 1, 0, 0, 0]);
        conn.send_event(false, root, EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY, ev)?;
        // and the states, one request per state (1 = add)
        for s in states {
            let ev = ClientMessageEvent::new(32, window, net_wm_state, [1, s, 0, 1, 0]);
            conn.send_event(
                false,
                root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                ev,
            )?;
        }
        // round trip: makes sure the server processed everything before the connection closes
        conn.get_input_focus()?.reply()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(x: i32, y: i32, w: i32, h: i32, scale: f64) -> Monitor {
        Monitor { x, y, w, h, scale, work: Rect { x, y, w, h } }
    }

    /// 1920x1080 monitor (scale 1) with the given work area.
    fn mon_work(x: i32, y: i32, w: i32, h: i32) -> Monitor {
        Monitor { work: Rect { x, y, w, h }, ..mon(0, 0, 1920, 1080, 1.0) }
    }

    #[test]
    fn scale_1_panel_at_the_bottom_opens_upwards() {
        let m = [mon(0, 0, 1920, 1080, 1.0)];
        assert_eq!(popup_position(Some((1800, 1070)), (400, 500), &m, 0), Some((1510, 536)));
    }

    #[test]
    fn scale_1_5_uses_logical_margins_and_anchor() {
        // 1920x1080 logical = 2880x1620 physical; the 400x500 logical window = 600x750
        let m = [mon(0, 0, 2880, 1620, 1.5)];
        let (x, y) = popup_position(Some((1200, 1070)), (600, 750), &m, 0).unwrap();
        assert_eq!((x, y), (1500, 804));
        // the same point at scale 1 gives (1000, 536) logical: at 1.5 it must be exactly 1.5×
        let m1 = [mon(0, 0, 1920, 1080, 1.0)];
        assert_eq!(popup_position(Some((1200, 1070)), (400, 500), &m1, 0), Some((1000, 536)));
        assert_eq!((x, y), (1000 * 3 / 2, 536 * 3 / 2));
    }

    #[test]
    fn anchor_in_physical_pixels_is_not_converted() {
        // Windows reports the click already in physical pixels: at 1.5 the point (2700, 1600) stays put
        let m = [mon(0, 0, 2880, 1620, 1.5)];
        let (x, y) = position_with(Some((2700, 1600)), false, (600, 750), &m, 0).unwrap();
        assert_eq!((x, y), (2880 - 600 - 15, 1600 - 36 - 15 - 750));
        // in logical pixels the same point would fall outside the monitor
        assert_ne!(popup_position(Some((2700, 1600)), (600, 750), &m, 0), Some((x, y)));
    }

    #[test]
    fn scale_2() {
        let m = [mon(0, 0, 3840, 2160, 2.0)];
        assert_eq!(popup_position(Some((1800, 1070)), (800, 1000), &m, 0), Some((3020, 1072)));
    }

    #[test]
    fn monitor_to_the_right_and_above() {
        let m = [mon(0, 0, 1920, 1080, 1.0), mon(1920, -1080, 1920, 1080, 1.0)];
        // click on the tray of the upper monitor (negative y), in the right corner
        assert_eq!(popup_position(Some((3800, -10)), (400, 500), &m, 0), Some((3430, -544)));
        // without an anchor, uses the fallback monitor (the upper one), bottom-right corner
        assert_eq!(popup_position(None, (400, 500), &m, 1), Some((3430, -10 - 500)));
    }

    #[test]
    fn panel_on_top_opens_downwards() {
        let m = [mon(0, 0, 1920, 1080, 1.0)];
        assert_eq!(popup_position(Some((100, 10)), (400, 500), &m, 0), Some((10, 44)));
        // at 1.5: anchor (100, 10) logical = (150, 15) physical; margins of 15 and 36
        let m15 = [mon(0, 0, 2880, 1620, 1.5)];
        assert_eq!(popup_position(Some((100, 10)), (600, 750), &m15, 0), Some((15, 66)));
    }

    #[test]
    fn without_anchor_goes_to_the_corner_of_the_fallback_monitor() {
        // against the bottom-right corner, `GAP` away from the edges
        let m = [mon(0, 0, 1920, 1080, 1.0)];
        assert_eq!(popup_position(None, (400, 500), &m, 0), Some((1920 - 10 - 400, 1080 - 10 - 500)));
        let m15 = [mon(0, 0, 2880, 1620, 1.5)];
        assert_eq!(popup_position(None, (600, 750), &m15, 0), Some((2880 - 15 - 600, 1620 - 15 - 750)));
    }

    #[test]
    fn anchor_outside_every_monitor_uses_the_fallback_and_stays_inside() {
        let m = [mon(0, 0, 1920, 1080, 1.0), mon(1920, 0, 1920, 1080, 1.0)];
        let (x, y) = popup_position(Some((5000, 5000)), (400, 500), &m, 1).unwrap();
        assert!((1920..=3840 - 400).contains(&x) && (0..=1080 - 500).contains(&y));
    }

    #[test]
    fn no_monitors_no_position() {
        assert_eq!(popup_position(Some((1, 1)), (400, 500), &[], 0), None);
        assert_eq!(popup_position(None, (400, 500), &[], 0), None);
    }

    #[test]
    fn window_larger_than_the_monitor_does_not_break() {
        let m = [mon(0, 0, 300, 200, 1.0)];
        let (x, y) = popup_position(Some((100, 100)), (400, 500), &m, 0).unwrap();
        // does not fit: stays at the start, `GAP` from the edge (on both axes)
        assert_eq!((x, y), (10, 10));
    }

    #[test]
    fn panel_on_the_left_opens_to_the_right() {
        // 48 px vertical panel on the left, click in its middle
        let m = [mon_work(48, 0, 1872, 1080)];
        assert_eq!(popup_position(Some((24, 540)), (400, 500), &m, 0), Some((24 + 24 + 10, 540 - 250)));
        // without a known work area: the edge closest to the click decides the same way
        let m = [mon(0, 0, 1920, 1080, 1.0)];
        assert_eq!(popup_position(Some((24, 540)), (400, 500), &m, 0), Some((58, 290)));
    }

    #[test]
    fn panel_on_the_right_opens_to_the_left() {
        let m = [mon_work(0, 0, 1872, 1080)];
        assert_eq!(popup_position(Some((1896, 300)), (400, 500), &m, 0), Some((1896 - 24 - 10 - 400, 300 - 250)));
        // near the top: does not go above the work area
        assert_eq!(popup_position(Some((1896, 20)), (400, 500), &m, 0), Some((1462, 10)));
    }

    #[test]
    fn thick_panel_at_the_bottom_is_not_covered() {
        // 80 px panel: the click at 1040 minus `PANEL_HALF` would still fall inside it
        let m = [mon_work(0, 0, 1920, 1000)];
        let (_, y) = popup_position(Some((1800, 1040)), (400, 500), &m, 0).unwrap();
        assert_eq!(y, 1000 - 10 - 500);
    }

    #[test]
    fn click_in_the_corner_follows_the_work_area_panel() {
        // the click (1918, 1060) is closer to the right edge, but the panel is the bottom one
        let m = [mon_work(0, 0, 1920, 1032)];
        let (x, y) = popup_position(Some((1918, 1060)), (400, 500), &m, 0).unwrap();
        assert_eq!((x, y), (1510, 1032 - 10 - 500));
    }

    #[test]
    fn without_anchor_left_panel_goes_to_the_bottom_left_corner() {
        let m = [mon_work(48, 0, 1872, 1080)];
        assert_eq!(popup_position(None, (400, 500), &m, 0), Some((58, 570)));
        // panel on top: top-right corner, below it
        let m = [mon_work(0, 32, 1920, 1048)];
        assert_eq!(popup_position(None, (400, 500), &m, 0), Some((1510, 42)));
    }

    #[test]
    fn work_area_clipped_per_monitor() {
        let left = Rect { x: 0, y: 0, w: 1920, h: 1080 };
        let right = Rect { x: 1920, y: 0, w: 1920, h: 1080 };
        // X11: one area for the whole virtual screen, panel at the bottom
        let desk = Rect { x: 0, y: 0, w: 3840, h: 1032 };
        assert_eq!(useful_part(left, desk), Some(Rect { x: 0, y: 0, w: 1920, h: 1032 }));
        assert_eq!(useful_part(right, desk), Some(Rect { x: 1920, y: 0, w: 1920, h: 1032 }));
        // an area that barely touches the monitor does not count for it
        let other = Rect { x: 1800, y: 0, w: 2040, h: 1080 };
        assert_eq!(useful_part(left, other), None);
        assert_eq!(useful_part(left, Rect { x: 5000, y: 0, w: 10, h: 10 }), None);
    }
}
