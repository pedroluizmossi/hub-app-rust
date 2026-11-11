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

/// Monitor em pixels físicos (como o winit informa) e o fator de escala dele.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Monitor {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub scale: f64,
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
    let (ax, ay) = match (anchor, found) {
        (_, Some((_, p))) => p,
        (Some(a), None) => to_physical(a, m.scale),
        // sem âncora: canto inferior direito do monitor, onde costuma ficar a bandeja
        (None, None) => (m.x + m.w - gap, m.y + m.h - gap),
    };

    let x = (ax - pw / 2).clamp(m.x + gap, (m.x + m.w - pw - gap).max(m.x + gap));
    // painel embaixo (clique na metade de baixo): abre para cima; senão para baixo
    let y = if ay > m.y + m.h / 2 { ay - half - gap - ph } else { ay + half + gap };
    let y = y.clamp(m.y, (m.y + m.h - ph).max(m.y));
    Some((x, y))
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
        let monitors: Vec<Monitor> = handles
            .iter()
            .map(|m| {
                let (p, s) = (m.position(), m.size());
                Monitor { x: p.x, y: p.y, w: s.width as i32, h: s.height as i32, scale: m.scale_factor() }
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
            eprintln!("miniutils: não deu para redimensionar: {e}");
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
        if std::env::var_os("MINIUTILS_DEBUG").is_some() {
            eprintln!("miniutils: estados EWMH na janela 0x{id:x}");
        }
        if let Err(e) = x11::set_states(id) {
            eprintln!("miniutils: não consegui tirar da barra de tarefas: {e}");
        }
    });
    if done.is_none() && std::env::var_os("MINIUTILS_DEBUG").is_some() {
        eprintln!("miniutils: janela winit ainda não existe");
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

    pub fn set_states(window: u32) -> anyhow::Result<()> {
        let (conn, screen) = x11rb::connect(None)?;
        let root = conn.setup().roots[screen].root;
        let atom = |name: &str| -> anyhow::Result<u32> { Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom) };
        let net_wm_state = atom("_NET_WM_STATE")?;
        let states = [
            atom("_NET_WM_STATE_SKIP_TASKBAR")?,
            atom("_NET_WM_STATE_SKIP_PAGER")?,
            atom("_NET_WM_STATE_ABOVE")?,
        ];
        // janela ainda não gerenciada (propriedade vazia): o gerenciador lê ao mapear.
        // Depois disso a propriedade é do gerenciador e não pode ser sobrescrita.
        let current = conn.get_property(false, window, net_wm_state, AtomEnum::ATOM, 0, 32)?.reply()?;
        if current.value_len == 0 {
            conn.change_property32(PropMode::REPLACE, window, net_wm_state, AtomEnum::ATOM, &states)?;
        }
        // janela já mapeada: o pedido vai por mensagem à raiz (1 = adicionar)
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
        Monitor { x, y, w, h, scale }
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
        // sem âncora, usa o monitor do reserva (o de cima)
        assert_eq!(popup_position(None, (400, 500), &m, 1), Some((3430, -544)));
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
        let m = [mon(0, 0, 1920, 1080, 1.0)];
        assert_eq!(popup_position(None, (400, 500), &m, 0), Some((1510, 536)));
        let m15 = [mon(0, 0, 2880, 1620, 1.5)];
        assert_eq!(popup_position(None, (600, 750), &m15, 0), Some((2265, 804)));
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
        assert_eq!((x, y), (10, 0));
    }
}
