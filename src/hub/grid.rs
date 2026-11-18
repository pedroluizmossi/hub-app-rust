//! Panel grid and the edit mode (moving and resizing the apps).

use slint::{ModelRc, VecModel};

use super::Hub;
use crate::core::layout::{self, Rect};
use crate::core::miniapp::AppRef;
use crate::mascot::render;
use crate::tr;
use crate::ui::{Ghost, GridCell};

/// An app being moved or resized in edit mode.
pub(super) struct Drag {
    id: String,
    /// "move", "r" (width), "b" (height) or "rb" (both).
    mode: String,
    /// Cell grabbed inside the app (when moving).
    off: (i32, i32),
    /// Where it would land and whether the others find room.
    target: Rect,
    ok: bool,
}

impl Hub {
    /// Places the enabled apps on the grid (each where the config says).
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
        let tag = if n == 1 { tr!("// 1 APP · {} × {}", cols, rows) } else { tr!("// {} APPS · {} × {}", n, cols, rows) };
        self.ui.set_grid_tag(tag.into());
        self.ui.set_any_enabled(!self.registry.enabled().is_empty());
        self.ui.set_columns(cols as i32);
        self.ui.set_rows(rows as i32);
        self.ui.set_cells(ModelRc::new(VecModel::from(cells)));
    }

    /// Saves a new grid arrangement (already validated by `layout::settle`).
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

    /// Shows (or hides) the drag preview.
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

    /// Grid and app (index and rectangle) for the edit mode calculations.
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
        let t = drag_target(&d.mode, d.off, col, row, rects[i], cols, rows);
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

    /// Arrows: move one cell; with Shift, change the size.
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

    /// Columns and rows of the grid; refused if some app would be left without room.
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

/// Where an app being dragged would land, with the pointer on cell (`col`, `row`). Moving
/// keeps the cell grabbed (`off`) under the pointer and the app inside the grid; resizing
/// ("r", "b", "rb") pulls the right/bottom edge to the pointer, never below 1×1 (the grid
/// edge is checked later by `layout::settle`).
fn drag_target(mode: &str, off: (i32, i32), col: i32, row: i32, r: Rect, cols: u32, rows: u32) -> Rect {
    if mode == "move" {
        let x = (col - off.0).clamp(0, cols.saturating_sub(r.w) as i32) as u32;
        let y = (row - off.1).clamp(0, rows.saturating_sub(r.h) as i32) as u32;
        Rect::new(x, y, r.w, r.h)
    } else {
        let w = if mode.contains('r') { (col - r.x as i32 + 1).max(1) as u32 } else { r.w };
        let h = if mode.contains('b') { (row - r.y as i32 + 1).max(1) as u32 } else { r.h };
        Rect::new(r.x, r.y, w, h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_keeps_the_grab_offset_inside_the_grid() {
        let r = Rect::new(1, 1, 2, 2);
        // grabbed by its second column: the pointer on column 4 puts it at 3
        assert_eq!(drag_target("move", (1, 0), 4, 1, r, 6, 4), Rect::new(3, 1, 2, 2));
        // past the right/bottom edge: clamped so it still fits
        assert_eq!(drag_target("move", (0, 0), 9, 9, r, 6, 4), Rect::new(4, 2, 2, 2));
        // past the left/top edge
        assert_eq!(drag_target("move", (1, 1), 0, 0, r, 6, 4), Rect::new(0, 0, 2, 2));
    }

    #[test]
    fn resize_never_goes_below_one_cell() {
        let r = Rect::new(2, 1, 2, 2);
        assert_eq!(drag_target("r", (0, 0), 4, 0, r, 6, 4), Rect::new(2, 1, 3, 2));
        assert_eq!(drag_target("b", (0, 0), 0, 3, r, 6, 4), Rect::new(2, 1, 2, 3));
        assert_eq!(drag_target("rb", (0, 0), 0, 0, r, 6, 4), Rect::new(2, 1, 1, 1));
        assert_eq!(drag_target("rb", (0, 0), 3, 2, r, 6, 4), Rect::new(2, 1, 2, 2));
    }
}
