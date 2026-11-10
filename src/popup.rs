//! Comportamento de popup de bandeja: abrir junto do ícone, fora da barra de tarefas.
//!
//! O Wayland não deixa um app posicionar a própria janela. Por isso, quando há XWayland
//! (`$DISPLAY`), o hub roda como cliente X11 — o mesmo vale direto em sessões X11 (XFCE,
//! Cinnamon, MATE). Com `CYBERHUB_NATIVE_WAYLAND=1` ele fica no Wayland nativo: sem
//! moldura e fechando ao perder o foco, mas onde o compositor quiser.

use slint::winit_030::{SlintEvent, WinitWindowAccessor, winit};
use winit::dpi::PhysicalPosition;

/// Distância entre o popup e o ponto clicado / bordas da tela.
const GAP: i32 = 10;
/// Metade de um painel típico: o clique cai no meio dele.
const PANEL_HALF: i32 = 24;

/// Escolhe o backend (winit + renderizador por software) e, se possível, o X11.
/// Devolve `true` quando o popup pode ser posicionado.
pub fn select_backend() -> anyhow::Result<bool> {
    let native_wayland = std::env::var_os("CYBERHUB_NATIVE_WAYLAND").is_some();
    let x11 = std::env::var_os("DISPLAY").is_some() && !native_wayland;
    let mut selector = slint::BackendSelector::new().backend_name("winit-software".into());
    if x11 {
        use winit::platform::x11::EventLoopBuilderExtX11;
        let mut builder = winit::event_loop::EventLoop::<SlintEvent>::with_user_event();
        builder.with_x11();
        selector = selector.with_winit_event_loop_builder(builder);
    }
    selector.select()?;
    Ok(x11)
}

/// Põe a janela junto de `anchor` (ponto clicado na bandeja, em pixels da tela).
/// Sem âncora, vai para o canto inferior direito, onde costuma ficar a bandeja.
pub fn place(window: &slint::Window, anchor: Option<(i32, i32)>) {
    window.with_winit_window(|w| {
        let size = w.outer_size();
        let (pw, ph) = (size.width as i32, size.height as i32);

        // monitor que contém o ponto (ou o atual)
        let monitor = anchor
            .and_then(|(ax, ay)| {
                w.available_monitors().find(|m| {
                    let (p, s) = (m.position(), m.size());
                    ax >= p.x && ax < p.x + s.width as i32 && ay >= p.y && ay < p.y + s.height as i32
                })
            })
            .or_else(|| w.current_monitor())
            .or_else(|| w.primary_monitor());
        let Some(m) = monitor else { return };
        let (mx, my) = (m.position().x, m.position().y);
        let (mw, mh) = (m.size().width as i32, m.size().height as i32);

        let (ax, ay) = anchor.unwrap_or((mx + mw - GAP, my + mh - GAP));
        let x = (ax - pw / 2).clamp(mx + GAP, (mx + mw - pw - GAP).max(mx));
        // painel embaixo (clique na metade de baixo): abre para cima; senão para baixo
        let y = if ay > my + mh / 2 { ay - PANEL_HALF - GAP - ph } else { ay + PANEL_HALF + GAP };
        let y = y.clamp(my, (my + mh - ph).max(my));
        w.set_outer_position(PhysicalPosition::new(x, y));
    });
}

/// Pede o foco (o popup precisa dele para fechar quando o usuário clica fora).
pub fn focus(window: &slint::Window) {
    window.with_winit_window(|w| w.focus_window());
}

/// Tira a janela da barra de tarefas e do alternador e a mantém por cima (EWMH, X11).
pub fn skip_taskbar(window: &slint::Window) {
    let done = window.with_winit_window(|w| {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let Ok(handle) = w.window_handle() else { return };
        let id = match handle.as_raw() {
            RawWindowHandle::Xlib(h) => h.window as u32,
            RawWindowHandle::Xcb(h) => h.window.get(),
            _ => return, // Wayland nativo: não há como pedir isso
        };
        if std::env::var_os("CYBERHUB_DEBUG").is_some() {
            eprintln!("cyberhub: estados EWMH na janela 0x{id:x}");
        }
        if let Err(e) = x11::set_states(id) {
            eprintln!("cyberhub: não consegui tirar da barra de tarefas: {e}");
        }
    });
    if done.is_none() && std::env::var_os("CYBERHUB_DEBUG").is_some() {
        eprintln!("cyberhub: janela winit ainda não existe");
    }
}

mod x11 {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        AtomEnum, ClientMessageEvent, ConnectionExt, EventMask, PropMode,
    };
    use x11rb::wrapper::ConnectionExt as _;

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
