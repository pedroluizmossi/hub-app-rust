//! Grade do painel e o modo de edição (mover e redimensionar os apps).

use slint::{ModelRc, VecModel};

use super::Hub;
use crate::core::layout::{self, Rect};
use crate::core::miniapp::AppRef;
use crate::mascot::render;
use crate::ui::{Ghost, GridCell};

/// Um app sendo movido ou redimensionado no modo de edição.
pub(super) struct Drag {
    id: String,
    /// "move", "r" (largura), "b" (altura) ou "rb" (as duas).
    mode: String,
    /// Célula agarrada dentro do app (ao mover).
    off: (i32, i32),
    /// Onde ele iria parar e se os outros acham lugar.
    target: Rect,
    ok: bool,
}

impl Hub {
    /// Posiciona os apps ligados na grade (cada um onde a config diz).
    pub(super) fn rebuild_grid(&self) {
        {
            let mut cfg = self.host.config.borrow_mut();
            if cfg.arrange() {
                cfg.save();
            }
        }
        let page = self.current_page();
        let cfg = self.host.config.borrow();
        let (cols, rows) = (cfg.pages[page].columns, cfg.pages[page].rows);
        let rects = cfg.rects(page);
        let apps: Vec<AppRef> = cfg.page_apps(page).iter().filter_map(|id| self.registry.find(id)).collect();
        let cells: Vec<GridCell> = apps
            .iter()
            .zip(&rects)
            .map(|(a, r)| {
                let a = a.borrow();
                let m = a.manifest();
                GridCell {
                    id: m.id.clone().into(),
                    name: m.name.clone().into(),
                    col: r.x as i32,
                    row: r.y as i32,
                    w: r.w as i32,
                    h: r.h as i32,
                    theme: render::colors(self.host.theme_for(m)),
                }
            })
            .collect();
        let n = cells.len();
        let fits = |c: u32, r: u32| {
            (1..=layout::MAX_COLS).contains(&c) && (1..=layout::MAX_ROWS).contains(&r) && layout::settle(&rects, None, c, r).is_some()
        };
        self.ui.set_cols_down_ok(fits(cols.wrapping_sub(1), rows));
        self.ui.set_cols_up_ok(fits(cols + 1, rows));
        self.ui.set_rows_down_ok(fits(cols, rows.wrapping_sub(1)));
        self.ui.set_rows_up_ok(fits(cols, rows + 1));
        self.ui.set_grid_tag(format!("// {n} {} · {cols} × {rows}", if n == 1 { "APP" } else { "APPS" }).into());
        self.ui.set_columns(cols as i32);
        self.ui.set_rows(rows as i32);
        self.ui.set_cells(ModelRc::new(VecModel::from(cells)));
    }

    /// Grava um novo arranjo da grade (já validado por `layout::settle`).
    pub(super) fn apply_layout(&self, cols: u32, rows: u32, rects: Vec<Rect>) {
        let page = self.current_page();
        {
            let mut cfg = self.host.config.borrow_mut();
            cfg.pages[page].columns = cols;
            cfg.pages[page].rows = rows;
            let ids = cfg.page_apps(page);
            for (id, r) in ids.into_iter().zip(rects) {
                cfg.layout.insert(id, r);
            }
            cfg.save();
        }
        self.rebuild_grid();
    }

    /// Mostra (ou esconde) a prévia do arrasto.
    pub(super) fn set_ghost(&self, drag: Option<&Drag>) {
        self.ui.set_ghost(match drag {
            Some(d) => Ghost {
                visible: true,
                id: d.id.clone().into(),
                col: d.target.x as i32,
                row: d.target.y as i32,
                w: d.target.w as i32,
                h: d.target.h as i32,
                ok: d.ok,
            },
            None => Ghost::default(),
        });
    }

    /// Grade e app (índice e retângulo) para os cálculos do modo de edição.
    pub(super) fn grid_of(&self, id: &str) -> Option<(u32, u32, Vec<Rect>, usize)> {
        let page = self.current_page();
        let cfg = self.host.config.borrow();
        let i = cfg.page_apps(page).iter().position(|e| e == id)?;
        Some((cfg.pages[page].columns, cfg.pages[page].rows, cfg.rects(page), i))
    }

    pub(super) fn edit_begin(&self, id: &str, mode: &str, col: i32, row: i32) {
        let Some((_, _, rects, i)) = self.grid_of(id) else { return };
        let r = rects[i];
        let drag = Drag { id: id.into(), mode: mode.into(), off: (col - r.x as i32, row - r.y as i32), target: r, ok: true };
        self.set_ghost(Some(&drag));
        *self.drag.borrow_mut() = Some(drag);
    }

    pub(super) fn edit_drag(&self, col: i32, row: i32) {
        let mut slot = self.drag.borrow_mut();
        let Some(d) = slot.as_mut() else { return };
        let Some((cols, rows, rects, i)) = self.grid_of(&d.id) else { return };
        let r = rects[i];
        let t = if d.mode == "move" {
            let x = (col - d.off.0).clamp(0, (cols - r.w) as i32) as u32;
            let y = (row - d.off.1).clamp(0, (rows - r.h) as i32) as u32;
            Rect::new(x, y, r.w, r.h)
        } else {
            let w = if d.mode.contains('r') { (col - r.x as i32 + 1).max(1) as u32 } else { r.w };
            let h = if d.mode.contains('b') { (row - r.y as i32 + 1).max(1) as u32 } else { r.h };
            Rect::new(r.x, r.y, w, h)
        };
        if t == d.target {
            return;
        }
        d.target = t;
        d.ok = layout::settle(&rects, Some((i, t)), cols, rows).is_some();
        self.set_ghost(Some(d));
    }

    pub(super) fn edit_end(&self) {
        let Some(d) = self.drag.borrow_mut().take() else { return };
        self.set_ghost(None);
        if !d.ok {
            return;
        }
        let Some((cols, rows, rects, i)) = self.grid_of(&d.id) else { return };
        if rects[i] == d.target {
            return;
        }
        if let Some(out) = layout::settle(&rects, Some((i, d.target)), cols, rows) {
            self.apply_layout(cols, rows, out);
        }
    }

    /// Setas: move uma célula; com Shift, muda o tamanho.
    pub(super) fn edit_key(&self, id: &str, dx: i32, dy: i32, resize: bool) {
        let Some((cols, rows, rects, i)) = self.grid_of(id) else { return };
        let r = rects[i];
        let (x, y, w, h) = if resize {
            (r.x as i32, r.y as i32, (r.w as i32 + dx).max(1), (r.h as i32 + dy).max(1))
        } else {
            (r.x as i32 + dx, r.y as i32 + dy, r.w as i32, r.h as i32)
        };
        if x < 0 || y < 0 {
            return;
        }
        let t = Rect::new(x as u32, y as u32, w as u32, h as u32);
        if let Some(out) = layout::settle(&rects, Some((i, t)), cols, rows) {
            self.apply_layout(cols, rows, out);
        }
    }

    /// Colunas e linhas da grade; recusa se algum app ficaria sem lugar.
    pub(super) fn grid_size(&self, cols: i32, rows: i32) {
        if !(1..=layout::MAX_COLS as i32).contains(&cols) || !(1..=layout::MAX_ROWS as i32).contains(&rows) {
            return;
        }
        let (cols, rows) = (cols as u32, rows as u32);
        let page = self.current_page();
        let rects = self.host.config.borrow().rects(page);
        if let Some(out) = layout::settle(&rects, None, cols, rows) {
            self.apply_layout(cols, rows, out);
        }
    }
}
