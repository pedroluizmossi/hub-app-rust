//! A janela do hub como popup da bandeja: abrir, esconder, fixar, fade e tamanho.

use std::time::{Duration, Instant};

use slint::{ComponentHandle, Timer};

use super::{FRAME_COLD, FRAME_HOT, Hub};
use crate::instance;
use crate::popup;
use crate::tray::Tray;

impl Hub {
    /// Mostra o popup junto de `anchor` (ponto clicado na bandeja).
    pub(super) fn show(&self, anchor: Option<(i32, i32)>) {
        let anchor = anchor.or(self.last_anchor.get());
        self.last_anchor.set(anchor);
        self.had_focus.set(false);
        // reabrindo: começa transparente e aparece com fade (a 1ª vez a janela ainda não existe)
        let fade_in = self.positionable && popup::has_window(self.ui.window());
        if fade_in {
            self.fade_gen.set(self.fade_gen.get() + 1); // cancela um fade de saída pendente
            popup::set_opacity(self.ui.window(), 0.0);
        }
        if self.ui.show().is_err() {
            return;
        }
        self.schedule_backdrop();
        if fade_in {
            self.fade(0.0, 1.0, false);
        }
        self.set_shown(true);
        let window = self.ui.window();
        if self.positionable {
            popup::skip_taskbar(window);
            popup::place(window, anchor);
        }
        popup::focus(window);
        // o gerenciador de janelas às vezes reposiciona ao mapear: reaplica a posição
        if self.positionable {
            let weak = self.ui.as_weak();
            Timer::single_shot(Duration::from_millis(60), move || {
                if let Some(ui) = weak.upgrade() {
                    popup::place(ui.window(), anchor);
                }
            });
        }
    }

    /// Pedido de outra instância: abre o popup (na aba, se houver). Já aberto, só traz para frente.
    pub(super) fn show_from_command(&self, tab: &str) {
        if !tab.is_empty() {
            self.open_tab(tab);
            self.rebuild_lists();
        }
        if self.shown.get() {
            popup::focus(self.ui.window());
        } else {
            self.show(None);
        }
    }

    /// Atende as outras instâncias (ver `instance.rs`) pela thread da UI.
    pub fn serve_commands(&self, server: &instance::Server) {
        let weak = self.ui.as_weak();
        let served = server.serve(move |cmd| {
            let instance::Command::Show(tab) = cmd;
            let tab = tab.unwrap_or_default();
            let _ = weak.upgrade_in_event_loop(move |ui| ui.invoke_show_requested(tab.into()));
        });
        if let Err(e) = served {
            eprintln!("miniutils: não consegui atender outras instâncias ({e})");
        }
    }

    /// A janela abriu ou se escondeu. Escondida, os provedores caros coletam mais devagar
    /// e a animação para; ao abrir, cada app pode se atualizar (ver `MiniApp::window_shown`).
    pub(super) fn set_shown(&self, on: bool) {
        if self.shown.replace(on) == on {
            return;
        }
        self.host.bus.set_visible(on);
        if on {
            self.update_clock();
            self.anim.restart();
            for app in self.registry.enabled() {
                app.borrow_mut().window_shown(&self.ui);
            }
        } else {
            self.anim.stop();
        }
    }

    /// Há um ícone visível na bandeja agora (o painel pode subir depois do app).
    pub(super) fn has_tray(&self) -> bool {
        self.tray.borrow().as_ref().is_some_and(Tray::online)
    }

    /// Guarda o tamanho atual da janela, se mudou. Em pixels lógicos, para não mudar de
    /// tamanho quando a escala da tela muda.
    pub(super) fn remember_size(&self) {
        let window = self.ui.window();
        let size = window.size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        let scale = window.scale_factor();
        let logical = ((size.width as f32 / scale).round() as u32, (size.height as f32 / scale).round() as u32);
        let mut cfg = self.host.config.borrow_mut();
        if cfg.window != Some(logical) {
            cfg.window = Some(logical);
            cfg.save();
        }
    }

    /// Muda o intervalo do quadro conforme o mouse está (ou não) sobre a janela.
    pub(super) fn set_hovered(&self, on: bool) {
        if self.hovered.replace(on) != on {
            self.anim.set_interval(if on { FRAME_HOT } else { FRAME_COLD });
        }
    }

    pub(super) fn hide(&self) {
        self.remember_size();
        // ao esconder não chega `CursorLeft`: a próxima vez que aparecer, o `CursorEntered` decide
        self.set_hovered(false);
        if !self.has_tray() {
            // sem bandeja não teria como voltar: só minimiza
            self.ui.window().set_minimized(true);
            return;
        }
        self.set_shown(false);
        if self.positionable {
            self.fade(1.0, 0.0, true); // some com fade em vez de piscar
        } else {
            let _ = self.ui.hide();
        }
    }

    /// Anima a opacidade da janela de `from` a `to` em ~110 ms; `then_hide` esconde no fim.
    pub(super) fn fade(&self, from: f32, to: f32, then_hide: bool) {
        const STEPS: u32 = 6;
        let id = self.fade_gen.get() + 1;
        self.fade_gen.set(id);
        for i in 1..=STEPS {
            let weak = self.ui.as_weak();
            let current = self.fade_gen.clone();
            Timer::single_shot(Duration::from_millis(18 * i as u64), move || {
                if current.get() != id {
                    return; // outro fade começou
                }
                let Some(ui) = weak.upgrade() else { return };
                let t = i as f32 / STEPS as f32;
                popup::set_opacity(ui.window(), from + (to - from) * t);
                if i == STEPS && then_hide {
                    let _ = ui.hide();
                }
            });
        }
    }

    /// Liga/desliga o modo fixar (alfinete, clique do meio ou menu da bandeja).
    pub(super) fn set_pinned(&self, on: bool) {
        self.pinned.set(on);
        self.ui.set_pinned(on);
        if let Some(tray) = self.tray.borrow().as_ref() {
            tray.set_pinned(on);
        }
    }

    /// Um arrasto de borda está em curso (ou acabou há pouco).
    pub(super) fn resizing(&self) -> bool {
        self.resize_until.get().is_some_and(|t| Instant::now() < t)
    }

    pub(super) fn toggle(&self, anchor: Option<(i32, i32)>) {
        // clicar no ícone tira o foco do popup antes do clique chegar aqui: se ele acabou
        // de se esconder por isso, o clique era para fechar, não para reabrir
        let just_hidden = self.focus_hidden_at.take().is_some_and(|t| t.elapsed() < Duration::from_millis(400));
        if self.shown.get() {
            self.hide();
        } else if !just_hidden {
            self.show(anchor);
        }
    }
}
