//! Comportamento de popup de bandeja: abrir junto do ícone, fora da barra de tarefas.
//!
//! O Wayland não deixa um app posicionar a própria janela. Por isso, quando há XWayland
//! (`$DISPLAY`), o hub roda como cliente X11 — o mesmo vale direto em sessões X11 (XFCE,
//! Cinnamon, MATE). Com `MINIUTILS_NATIVE_WAYLAND=1` ele fica no Wayland nativo: sem
//! moldura e fechando ao perder o foco, mas onde o compositor quiser.
//!
//! No Windows a janela se posiciona sem restrição; o que muda é como pedir "fora da barra
//! de tarefas" e a opacidade do fade (ver os blocos `#[cfg(windows)]`).

use slint::winit_030::{WinitWindowAccessor, winit};
use winit::dpi::PhysicalPosition;

/// Distância entre o popup e o ponto clicado / bordas da tela.
const GAP: i32 = 10;
/// Metade de um painel típico: o clique cai no meio dele.
const PANEL_HALF: i32 = 24;

/// Escolhe o backend (winit + renderizador por software). No Windows o popup sempre pode
/// ser posicionado.
#[cfg(windows)]
pub fn select_backend() -> anyhow::Result<bool> {
    slint::BackendSelector::new().backend_name("winit-software".into()).select()?;
    Ok(true)
}

/// Escolhe o backend (winit + renderizador por software) e, se possível, o X11.
/// Devolve `true` quando o popup pode ser posicionado.
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

/// Retângulo em pixels físicos.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Monitor em pixels físicos (como o winit informa) e o fator de escala dele.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Monitor {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub scale: f64,
    /// Área útil (o monitor sem os painéis), física. O próprio monitor quando não se sabe.
    pub work: Rect,
}

/// Borda do monitor em que fica o painel da bandeja.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Side {
    Bottom,
    Top,
    Left,
    Right,
}

/// O pedaço de `work` que cai em `m`. `None` quando sobra menos da metade do monitor em
/// alguma direção: aí a área útil não descreve este monitor (no X11 ela é uma só para a
/// tela virtual inteira, e com monitores de alturas diferentes pode sair torta).
fn useful_part(m: Rect, work: Rect) -> Option<Rect> {
    let x0 = m.x.max(work.x);
    let y0 = m.y.max(work.y);
    let x1 = (m.x + m.w).min(work.x + work.w);
    let y1 = (m.y + m.h).min(work.y + work.h);
    let (w, h) = (x1 - x0, y1 - y0);
    (w * 2 >= m.w && h * 2 >= m.h).then_some(Rect { x: x0, y: y0, w, h })
}

/// Lados em que a área útil é menor que o monitor: onde há painel.
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

/// Lado do painel. Com o clique: a borda mais próxima dele, entre as que têm painel (se a
/// área útil diz quais) — isso desempata o clique no canto. Sem clique: a primeira borda
/// com painel, ou embaixo. Empate vai para embaixo/em cima, que é o painel mais comum.
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

/// Encaixa `pos` (com `size`) entre `start` e `start + len`, a `gap` das bordas. Se não
/// cabe, fica colado no começo (a parte de cima/esquerda, onde está o título, aparece).
fn fit(pos: i32, size: i32, start: i32, len: i32, gap: i32) -> i32 {
    pos.clamp(start + gap, (start + len - size - gap).max(start + gap))
}

/// A bandeja entrega o ponto clicado em pixels lógicos (o SNI, no Linux) ou físicos (o
/// Windows)?
const ANCHOR_LOGICAL: bool = cfg!(not(windows));

/// Posição do popup com a âncora em pixels lógicos (ver `position_with`).
#[cfg(test)]
pub fn popup_position(anchor: Option<(i32, i32)>, win: (i32, i32), monitors: &[Monitor], fallback: usize) -> Option<(i32, i32)> {
    position_with(anchor, true, win, monitors, fallback)
}

/// Posição física (canto superior esquerdo) do popup. Função pura, para testar.
/// - `anchor`: ponto clicado na bandeja; `anchor_logical` diz se vem em pixels lógicos
///   (convertidos pela escala de cada monitor) ou já em físicos.
/// - `win`: tamanho físico da janela.
/// - `fallback`: índice do monitor usado sem âncora ou quando ela não cai em nenhum monitor.
pub fn position_with(
    anchor: Option<(i32, i32)>,
    anchor_logical: bool,
    win: (i32, i32),
    monitors: &[Monitor],
    fallback: usize,
) -> Option<(i32, i32)> {
    let (pw, ph) = win;
    let to_physical = |a: (i32, i32), scale: f64| if anchor_logical { to_physical(a, scale) } else { a };
    // o monitor que contém a âncora (cada um converte o ponto pela própria escala)
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
    // margens e metade do painel são em pixels lógicos: aqui viram físicos
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
        // abre para longe do painel: o clique cai no meio dele, por isso `half`
        Some((ax, ay)) => match side {
            Side::Bottom => (ax - pw / 2, ay - half - gap - ph),
            Side::Top => (ax - pw / 2, ay + half + gap),
            Side::Left => (ax + half + gap, ay - ph / 2),
            Side::Right => (ax - half - gap - pw, ay - ph / 2),
        },
        // sem clique: no canto da área útil junto do painel (direita, ou esquerda se o
        // painel é o da esquerda); o `fit` abaixo encosta nas bordas
        None => {
            let left = side == Side::Left;
            let x = if left { i32::MIN / 2 } else { i32::MAX / 2 };
            let y = if side == Side::Top { i32::MIN / 2 } else { i32::MAX / 2 };
            (x, y)
        }
    };
    // dentro da área útil: não cobre painel nenhum, nem o que for mais grosso que `half`
    Some((fit(x, pw, wk.x, wk.w, gap), fit(y, ph, wk.y, wk.h, gap)))
}

fn to_physical((x, y): (i32, i32), scale: f64) -> (i32, i32) {
    ((x as f64 * scale).round() as i32, (y as f64 * scale).round() as i32)
}

fn contains(m: &Monitor, (x, y): (i32, i32)) -> bool {
    x >= m.x && x < m.x + m.w && y >= m.y && y < m.y + m.h
}

/// Põe a janela junto de `anchor` (ponto clicado na bandeja, como a bandeja o entrega).
/// Sem âncora, vai para o canto inferior direito, onde costuma ficar a bandeja.
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
        // o monitor atual (ou o primário) é o reserva quando a âncora não cai em nenhum
        let current = w.current_monitor().or_else(|| w.primary_monitor());
        let fallback = current.and_then(|c| handles.iter().position(|m| m.position() == c.position())).unwrap_or(0);
        let win = (size.width as i32, size.height as i32);
        if let Some((x, y)) = position_with(anchor, ANCHOR_LOGICAL, win, &monitors, fallback) {
            w.set_outer_position(PhysicalPosition::new(x, y));
        }
    });
}

/// Área útil de um monitor (sem a barra de tarefas), física. O Windows dá por monitor.
#[cfg(windows)]
fn monitor_work_area(h: &winit::monitor::MonitorHandle) -> Option<Rect> {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO};
    use winit::platform::windows::MonitorHandleExtWindows;
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: o HMONITOR vem do winit (monitor que existe agora) e `info` tem o cbSize certo.
    let ok = unsafe { GetMonitorInfoW(HMONITOR(h.hmonitor() as *mut std::ffi::c_void), &mut info) };
    let r = info.rcWork;
    ok.as_bool().then_some(Rect { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top })
}

/// O X11 não dá a área útil por monitor (ver `desktop_work_area`).
#[cfg(target_os = "linux")]
fn monitor_work_area(_: &winit::monitor::MonitorHandle) -> Option<Rect> {
    None
}

/// Área útil da tela virtual inteira (`_NET_WORKAREA`, X11); cada monitor fica com o seu
/// pedaço (`useful_part`). Painel no meio de dois monitores não aparece nela.
#[cfg(target_os = "linux")]
fn desktop_work_area() -> Option<Rect> {
    x11::work_area().unwrap_or_else(|e| {
        log::debug!("sem _NET_WORKAREA: {e}");
        None
    })
}

/// No Windows a área vem por monitor (`monitor_work_area`).
#[cfg(windows)]
fn desktop_work_area() -> Option<Rect> {
    None
}

/// Começa a mover a janela (sem moldura) com o mouse pressionado, como na barra de título
/// do sistema.
pub fn drag_move(window: &slint::Window) {
    window.with_winit_window(|w| {
        if let Err(e) = w.drag_window() {
            log::warn!("não deu para mover a janela: {e}");
        }
    });
}

/// Começa a redimensionar pela borda/canto `dir` ("n", "se", …), com o mouse pressionado.
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
            log::warn!("não deu para redimensionar: {e}");
        }
    });
}

/// Pede o foco (o popup precisa dele para fechar quando o usuário clica fora).
pub fn focus(window: &slint::Window) {
    window.with_winit_window(|w| w.focus_window());
}

/// A janela nativa já existe (depois do primeiro `show`).
pub fn has_window(window: &slint::Window) -> bool {
    window.has_winit_window()
}

/// Opacidade da janela inteira (0..1), como janela em camadas (`WS_EX_LAYERED`).
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
        // SAFETY: `hwnd` é a janela viva do winit, e estamos na thread dela.
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            if style & layered == 0 {
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | layered);
            }
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), (alpha.clamp(0.0, 1.0) * 255.0) as u8, LWA_ALPHA);
        }
    });
}

/// Tira a janela da barra de tarefas e a mantém por cima (o winit já sabe pedir isso).
#[cfg(windows)]
pub fn skip_taskbar(window: &slint::Window) {
    use winit::platform::windows::WindowExtWindows;
    window.with_winit_window(|w| {
        w.set_skip_taskbar(true);
        w.set_window_level(winit::window::WindowLevel::AlwaysOnTop);
    });
}

/// Opacidade da janela inteira (0..1), via `_NET_WM_WINDOW_OPACITY` do compositor.
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

/// Tira a janela da barra de tarefas e do alternador e a mantém por cima (EWMH, X11).
#[cfg(target_os = "linux")]
pub fn skip_taskbar(window: &slint::Window) {
    let done = window.with_winit_window(|w| {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let Ok(handle) = w.window_handle() else { return };
        let id = match handle.as_raw() {
            RawWindowHandle::Xlib(h) => h.window as u32,
            RawWindowHandle::Xcb(h) => h.window.get(),
            _ => return, // Wayland nativo: não há como pedir isso
        };
        log::debug!("estados EWMH na janela 0x{id:x}");
        if let Err(e) = x11::set_states(id) {
            log::warn!("não consegui tirar da barra de tarefas: {e}");
        }
    });
    if done.is_none() {
        log::debug!("janela winit ainda não existe");
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
        /// Conexão reaproveitada pelo fade (várias mudanças de opacidade seguidas).
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

    /// `_NET_WORKAREA` da área de trabalho atual (a tela virtual menos os painéis).
    pub fn work_area() -> anyhow::Result<Option<super::Rect>> {
        let (conn, screen) = x11rb::connect(None)?;
        let root = conn.setup().roots[screen].root;
        let atom = |name: &str| -> anyhow::Result<u32> { Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom) };
        let current = conn.get_property(false, root, atom("_NET_CURRENT_DESKTOP")?, AtomEnum::CARDINAL, 0, 1)?.reply()?;
        let desk = current.value32().and_then(|mut v| v.next()).unwrap_or(0);
        // uma área (x, y, w, h) por área de trabalho; o deslocamento conta em palavras de 32 bits
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
            // em todas as áreas de trabalho: sem isso o popup fica preso na primeira em que abriu
            atom("_NET_WM_STATE_STICKY")?,
        ];
        // 0xFFFFFFFF em _NET_WM_DESKTOP = todas as áreas de trabalho (EWMH)
        const ALL_DESKTOPS: u32 = 0xFFFF_FFFF;
        // janela ainda não gerenciada (propriedade vazia): o gerenciador lê ao mapear.
        // Depois disso a propriedade é do gerenciador e não pode ser sobrescrita.
        let current = conn.get_property(false, window, net_wm_state, AtomEnum::ATOM, 0, 32)?.reply()?;
        if current.value_len == 0 {
            conn.change_property32(PropMode::REPLACE, window, net_wm_state, AtomEnum::ATOM, &states)?;
            conn.change_property32(PropMode::REPLACE, window, net_wm_desktop, AtomEnum::CARDINAL, &[ALL_DESKTOPS])?;
        }
        // janela já mapeada: a área de trabalho também vai por mensagem à raiz
        // (dados: área, 1 = pedido de app comum)
        let ev = ClientMessageEvent::new(32, window, net_wm_desktop, [ALL_DESKTOPS, 1, 0, 0, 0]);
        conn.send_event(false, root, EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY, ev)?;
        // e os estados, um pedido por estado (1 = adicionar)
        for s in states {
            let ev = ClientMessageEvent::new(32, window, net_wm_state, [1, s, 0, 1, 0]);
            conn.send_event(
                false,
                root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                ev,
            )?;
        }
        // ida e volta: garante que o servidor processou tudo antes de a conexão fechar
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

    /// Monitor 1920x1080 (escala 1) com a área útil dada.
    fn mon_work(x: i32, y: i32, w: i32, h: i32) -> Monitor {
        Monitor { work: Rect { x, y, w, h }, ..mon(0, 0, 1920, 1080, 1.0) }
    }

    #[test]
    fn escala_1_painel_embaixo_abre_para_cima() {
        let m = [mon(0, 0, 1920, 1080, 1.0)];
        assert_eq!(popup_position(Some((1800, 1070)), (400, 500), &m, 0), Some((1510, 536)));
    }

    #[test]
    fn escala_1_5_usa_margens_e_ancora_em_logico() {
        // 1920x1080 lógicos = 2880x1620 físicos; a janela 400x500 lógicos = 600x750
        let m = [mon(0, 0, 2880, 1620, 1.5)];
        let (x, y) = popup_position(Some((1200, 1070)), (600, 750), &m, 0).unwrap();
        assert_eq!((x, y), (1500, 804));
        // o mesmo ponto em escala 1 dá (1000, 536) lógicos: a 1,5 tem que dar exatamente 1,5×
        let m1 = [mon(0, 0, 1920, 1080, 1.0)];
        assert_eq!(popup_position(Some((1200, 1070)), (400, 500), &m1, 0), Some((1000, 536)));
        assert_eq!((x, y), (1000 * 3 / 2, 536 * 3 / 2));
    }

    #[test]
    fn ancora_em_pixels_fisicos_nao_e_convertida() {
        // o Windows entrega o clique já em físicos: a 1,5 o ponto (2700, 1600) fica onde está
        let m = [mon(0, 0, 2880, 1620, 1.5)];
        let (x, y) = position_with(Some((2700, 1600)), false, (600, 750), &m, 0).unwrap();
        assert_eq!((x, y), (2880 - 600 - 15, 1600 - 36 - 15 - 750));
        // em lógicos o mesmo ponto cairia fora do monitor
        assert_ne!(popup_position(Some((2700, 1600)), (600, 750), &m, 0), Some((x, y)));
    }

    #[test]
    fn escala_2() {
        let m = [mon(0, 0, 3840, 2160, 2.0)];
        assert_eq!(popup_position(Some((1800, 1070)), (800, 1000), &m, 0), Some((3020, 1072)));
    }

    #[test]
    fn monitor_a_direita_e_acima() {
        let m = [mon(0, 0, 1920, 1080, 1.0), mon(1920, -1080, 1920, 1080, 1.0)];
        // clique na bandeja do monitor de cima (y negativo), no canto direito
        assert_eq!(popup_position(Some((3800, -10)), (400, 500), &m, 0), Some((3430, -544)));
        // sem âncora, usa o monitor do reserva (o de cima), no canto inferior direito
        assert_eq!(popup_position(None, (400, 500), &m, 1), Some((3430, -10 - 500)));
    }

    #[test]
    fn painel_em_cima_abre_para_baixo() {
        let m = [mon(0, 0, 1920, 1080, 1.0)];
        assert_eq!(popup_position(Some((100, 10)), (400, 500), &m, 0), Some((10, 44)));
        // a 1,5: âncora (100, 10) lógica = (150, 15) física; margens de 15 e 36
        let m15 = [mon(0, 0, 2880, 1620, 1.5)];
        assert_eq!(popup_position(Some((100, 10)), (600, 750), &m15, 0), Some((15, 66)));
    }

    #[test]
    fn sem_ancora_vai_para_o_canto_do_monitor_de_reserva() {
        // encostado no canto inferior direito, a `GAP` das bordas
        let m = [mon(0, 0, 1920, 1080, 1.0)];
        assert_eq!(popup_position(None, (400, 500), &m, 0), Some((1920 - 10 - 400, 1080 - 10 - 500)));
        let m15 = [mon(0, 0, 2880, 1620, 1.5)];
        assert_eq!(popup_position(None, (600, 750), &m15, 0), Some((2880 - 15 - 600, 1620 - 15 - 750)));
    }

    #[test]
    fn ancora_fora_de_todos_os_monitores_usa_o_reserva_e_fica_dentro() {
        let m = [mon(0, 0, 1920, 1080, 1.0), mon(1920, 0, 1920, 1080, 1.0)];
        let (x, y) = popup_position(Some((5000, 5000)), (400, 500), &m, 1).unwrap();
        assert!((1920..=3840 - 400).contains(&x) && (0..=1080 - 500).contains(&y));
    }

    #[test]
    fn sem_monitores_nao_posiciona() {
        assert_eq!(popup_position(Some((1, 1)), (400, 500), &[], 0), None);
        assert_eq!(popup_position(None, (400, 500), &[], 0), None);
    }

    #[test]
    fn janela_maior_que_o_monitor_nao_quebra() {
        let m = [mon(0, 0, 300, 200, 1.0)];
        let (x, y) = popup_position(Some((100, 100)), (400, 500), &m, 0).unwrap();
        // não cabe: fica no começo, a `GAP` da borda (nos dois eixos)
        assert_eq!((x, y), (10, 10));
    }

    #[test]
    fn painel_a_esquerda_abre_para_a_direita() {
        // painel vertical de 48 px à esquerda, clique no meio dele
        let m = [mon_work(48, 0, 1872, 1080)];
        assert_eq!(popup_position(Some((24, 540)), (400, 500), &m, 0), Some((24 + 24 + 10, 540 - 250)));
        // sem área útil conhecida: a borda mais próxima do clique decide igual
        let m = [mon(0, 0, 1920, 1080, 1.0)];
        assert_eq!(popup_position(Some((24, 540)), (400, 500), &m, 0), Some((58, 290)));
    }

    #[test]
    fn painel_a_direita_abre_para_a_esquerda() {
        let m = [mon_work(0, 0, 1872, 1080)];
        assert_eq!(popup_position(Some((1896, 300)), (400, 500), &m, 0), Some((1896 - 24 - 10 - 400, 300 - 250)));
        // perto do topo: não sobe além da área útil
        assert_eq!(popup_position(Some((1896, 20)), (400, 500), &m, 0), Some((1462, 10)));
    }

    #[test]
    fn painel_grosso_embaixo_nao_e_coberto() {
        // painel de 80 px: o clique em 1040 menos `PANEL_HALF` ainda cairia dentro dele
        let m = [mon_work(0, 0, 1920, 1000)];
        let (_, y) = popup_position(Some((1800, 1040)), (400, 500), &m, 0).unwrap();
        assert_eq!(y, 1000 - 10 - 500);
    }

    #[test]
    fn clique_no_canto_segue_o_painel_da_area_util() {
        // o clique (1918, 1060) está mais perto da borda direita, mas o painel é o de baixo
        let m = [mon_work(0, 0, 1920, 1032)];
        let (x, y) = popup_position(Some((1918, 1060)), (400, 500), &m, 0).unwrap();
        assert_eq!((x, y), (1510, 1032 - 10 - 500));
    }

    #[test]
    fn sem_ancora_painel_a_esquerda_vai_para_o_canto_inferior_esquerdo() {
        let m = [mon_work(48, 0, 1872, 1080)];
        assert_eq!(popup_position(None, (400, 500), &m, 0), Some((58, 570)));
        // painel em cima: canto superior direito, abaixo dele
        let m = [mon_work(0, 32, 1920, 1048)];
        assert_eq!(popup_position(None, (400, 500), &m, 0), Some((1510, 42)));
    }

    #[test]
    fn area_util_recortada_por_monitor() {
        let left = Rect { x: 0, y: 0, w: 1920, h: 1080 };
        let right = Rect { x: 1920, y: 0, w: 1920, h: 1080 };
        // X11: uma área para a tela virtual inteira, painel embaixo
        let desk = Rect { x: 0, y: 0, w: 3840, h: 1032 };
        assert_eq!(useful_part(left, desk), Some(Rect { x: 0, y: 0, w: 1920, h: 1032 }));
        assert_eq!(useful_part(right, desk), Some(Rect { x: 1920, y: 0, w: 1920, h: 1032 }));
        // área que mal toca o monitor não vale para ele
        let other = Rect { x: 1800, y: 0, w: 2040, h: 1080 };
        assert_eq!(useful_part(left, other), None);
        assert_eq!(useful_part(left, Rect { x: 5000, y: 0, w: 10, h: 10 }), None);
    }
}
