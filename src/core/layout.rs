//! Panel grid: each app takes a rectangle of cells in a grid of columns × rows.
//!
//! In edit mode the user drags and resizes an app; the others stay where they are if they
//! fit and, if not, go to the first free spot (`settle`).

use serde::{Deserialize, Serialize};

/// Grid limits.
pub const MAX_COLS: u32 = 6;
pub const MAX_ROWS: u32 = 6;

/// Position and size of an app, in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Rect { x, y, w, h }
    }
}

/// Taken cells of a grid.
struct Occupancy {
    cols: u32,
    rows: u32,
    used: Vec<bool>,
}

impl Occupancy {
    fn new(cols: u32, rows: u32) -> Self {
        Occupancy { cols, rows, used: vec![false; (cols * rows) as usize] }
    }

    fn fits(&self, r: Rect) -> bool {
        if r.w == 0 || r.h == 0 || r.x + r.w > self.cols || r.y + r.h > self.rows {
            return false;
        }
        (r.y..r.y + r.h).all(|y| (r.x..r.x + r.w).all(|x| !self.used[(y * self.cols + x) as usize]))
    }

    fn mark(&mut self, r: Rect) {
        for y in r.y..r.y + r.h {
            for x in r.x..r.x + r.w {
                self.used[(y * self.cols + x) as usize] = true;
            }
        }
    }

    /// First free spot (row by row) for a `w`×`h` app.
    fn first_free(&self, w: u32, h: u32) -> Option<Rect> {
        if w > self.cols || h > self.rows {
            return None;
        }
        (0..=self.rows - h)
            .flat_map(|y| (0..=self.cols - w).map(move |x| Rect::new(x, y, w, h)))
            .find(|r| self.fits(*r))
    }
}

/// Arranges `items` in a `cols`×`rows` grid. `fixed` = (index, rectangle) that does not
/// move (the app the user is dragging). The others keep their spot if they fit (pushed
/// inside the grid when it shrinks); those that don't fit go, in order, to the first free
/// spot. Returns `None` if any of them fits nowhere.
pub fn settle(items: &[Rect], fixed: Option<(usize, Rect)>, cols: u32, rows: u32) -> Option<Vec<Rect>> {
    let mut occ = Occupancy::new(cols, rows);
    let mut out = items.to_vec();
    if let Some((i, r)) = fixed {
        if i >= items.len() || !occ.fits(r) {
            return None;
        }
        occ.mark(r);
        out[i] = r;
    }
    let mut pending = Vec::new();
    for (i, r) in items.iter().enumerate() {
        if fixed.is_some_and(|(f, _)| f == i) {
            continue;
        }
        let (w, h) = (r.w.clamp(1, cols), r.h.clamp(1, rows));
        let r = Rect::new(r.x.min(cols - w), r.y.min(rows - h), w, h);
        if occ.fits(r) {
            occ.mark(r);
            out[i] = r;
        } else {
            pending.push((i, r));
        }
    }
    for (i, r) in pending {
        let free = occ.first_free(r.w, r.h)?;
        occ.mark(free);
        out[i] = free;
    }
    Some(out)
}

/// Adds `want` next to `items` (already arranged): it stays where asked if it fits;
/// otherwise it goes to the first free spot with the same size, then as 1×1; with none,
/// the grid gets more rows. Returns the spot and the number of rows.
pub fn add(items: &[Rect], want: Rect, cols: u32, rows: u32) -> (Rect, u32) {
    let (w, h) = (want.w.clamp(1, cols), want.h.max(1));
    let mut rows = rows.max(1);
    loop {
        let mut occ = Occupancy::new(cols, rows);
        let ok = items.iter().all(|r| {
            let fits = occ.fits(*r);
            if fits {
                occ.mark(*r);
            }
            fits
        });
        if ok {
            let asked = Rect::new(want.x, want.y, w, h);
            if occ.fits(asked) {
                return (asked, rows);
            }
            if let Some(r) = occ.first_free(w, h.min(rows)).or_else(|| occ.first_free(1, 1)) {
                return (r, rows);
            }
        }
        rows += 1;
    }
}

/// Packs the apps in order, filling row by row (used to migrate the old config, which
/// only kept width and height). Returns the positions and the rows used.
pub fn pack(items: &[(u32, u32)], columns: u32) -> (Vec<Rect>, u32) {
    let cols = columns.max(1) as usize;
    let mut used: Vec<Vec<bool>> = Vec::new();
    let mut out = Vec::with_capacity(items.len());
    for &(w, h) in items {
        let (w, h) = (w.clamp(1, cols as u32) as usize, h.max(1) as usize);
        let mut row = 0;
        'search: loop {
            for col in 0..=cols - w {
                let free = (row..row + h).all(|r| (col..col + w).all(|c| !used.get(r).is_some_and(|line| line[c])));
                if free {
                    while used.len() < row + h {
                        used.push(vec![false; cols]);
                    }
                    for line in used.iter_mut().skip(row).take(h) {
                        line[col..col + w].fill(true);
                    }
                    out.push(Rect::new(col as u32, row as u32, w as u32, h as u32));
                    break 'search;
                }
            }
            row += 1;
        }
    }
    (out, used.len() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: u32, y: u32, w: u32, h: u32) -> Rect {
        Rect::new(x, y, w, h)
    }

    #[test]
    fn pack_side_by_side_and_stacked() {
        assert_eq!(pack(&[(1, 1), (1, 1)], 2), (vec![r(0, 0, 1, 1), r(1, 0, 1, 1)], 1));
        assert_eq!(pack(&[(2, 1), (2, 1)], 2), (vec![r(0, 0, 2, 1), r(0, 1, 2, 1)], 2));
    }

    #[test]
    fn pack_fills_holes_and_limits_width() {
        assert_eq!(pack(&[(1, 2), (1, 1), (1, 1)], 2), (vec![r(0, 0, 1, 2), r(1, 0, 1, 1), r(1, 1, 1, 1)], 2));
        assert_eq!(pack(&[(5, 1)], 3), (vec![r(0, 0, 3, 1)], 1));
        assert_eq!(pack(&[], 2), (vec![], 0));
    }

    #[test]
    fn settle_keeps_what_fits() {
        let items = [r(0, 0, 2, 1), r(0, 1, 2, 1), r(2, 0, 1, 2)];
        assert_eq!(settle(&items, None, 3, 2), Some(items.to_vec()));
    }

    #[test]
    fn settle_pushes_colliding_apps_to_a_free_spot() {
        // docker (0,0,2,1) and vitals (2,0,1,2); moving vitals to (0,0) 1×1 pushes docker
        let items = [r(0, 0, 2, 1), r(2, 0, 1, 2)];
        let out = settle(&items, Some((1, r(0, 0, 1, 1))), 3, 2).unwrap();
        assert_eq!(out[1], r(0, 0, 1, 1));
        assert_eq!(out[0], r(1, 0, 2, 1));
    }

    #[test]
    fn settle_refuses_when_it_does_not_fit() {
        let items = [r(0, 0, 2, 2), r(2, 0, 1, 2)];
        // docker taking the whole grid: vitals has nowhere to go
        assert_eq!(settle(&items, Some((0, r(0, 0, 3, 2))), 3, 2), None);
        // outside the grid
        assert_eq!(settle(&items, Some((1, r(2, 1, 1, 2))), 3, 2), None);
    }

    #[test]
    fn settle_shrinks_the_grid() {
        // from 3 to 2 columns: vitals in column 2 moves down to the first free spot
        let items = [r(0, 0, 2, 1), r(2, 0, 1, 1)];
        assert_eq!(settle(&items, None, 2, 2), Some(vec![r(0, 0, 2, 1), r(0, 1, 1, 1)]));
        // from 2 rows to 1 with two full-width apps: does not fit
        assert_eq!(settle(&[r(0, 0, 2, 1), r(0, 1, 2, 1)], None, 2, 1), None);
    }

    #[test]
    fn add_stays_put_uses_a_free_spot_shrinks_or_grows() {
        assert_eq!(add(&[r(0, 0, 1, 1)], r(1, 0, 1, 1), 2, 1), (r(1, 0, 1, 1), 1));
        // asked spot taken: first free spot
        assert_eq!(add(&[r(0, 0, 1, 1)], r(0, 0, 1, 1), 2, 1), (r(1, 0, 1, 1), 1));
        // 2×1 does not fit beside it; 1×1 does
        assert_eq!(add(&[r(0, 0, 1, 1)], r(0, 0, 2, 1), 2, 1), (r(1, 0, 1, 1), 1));
        // full grid: gets a new row
        assert_eq!(add(&[r(0, 0, 2, 1)], r(0, 0, 2, 1), 2, 1), (r(0, 1, 2, 1), 2));
    }
}
