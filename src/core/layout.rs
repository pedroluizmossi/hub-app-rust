//! Grade do painel: cada app ocupa um retângulo de células numa grade de colunas × linhas.
//!
//! No modo de edição o usuário arrasta e redimensiona um app; os outros ficam onde estão
//! se couberem e, se não, vão para a primeira vaga livre (`settle`).

use serde::{Deserialize, Serialize};

/// Limites da grade.
pub const MAX_COLS: u32 = 6;
pub const MAX_ROWS: u32 = 6;

/// Posição e tamanho de um app, em células.
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

/// Células ocupadas de uma grade.
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

    /// Primeira vaga (linha a linha) para um app de `w`×`h`.
    fn first_free(&self, w: u32, h: u32) -> Option<Rect> {
        if w > self.cols || h > self.rows {
            return None;
        }
        (0..=self.rows - h)
            .flat_map(|y| (0..=self.cols - w).map(move |x| Rect::new(x, y, w, h)))
            .find(|r| self.fits(*r))
    }
}

/// Arruma `items` numa grade `cols`×`rows`. `fixed` = (índice, retângulo) que não se
/// move (o app que o usuário está arrastando). Os demais mantêm o lugar se couberem
/// (empurrados para dentro da grade quando ela encolhe); os que não cabem vão, na ordem,
/// para a primeira vaga livre. Devolve `None` se algum não couber em lugar nenhum.
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

/// Acrescenta `want` ao lado de `items` (já arrumados): fica onde pediu se couber; senão
/// vai para a primeira vaga com o mesmo tamanho, depois como 1×1; sem nenhuma, a grade
/// ganha linhas. Devolve o lugar e o número de linhas.
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

/// Empacota os apps na ordem, preenchendo linha a linha (usado para migrar a config
/// antiga, que só guardava largura e altura). Devolve as posições e as linhas usadas.
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
    fn pack_lado_a_lado_e_empilhado() {
        assert_eq!(pack(&[(1, 1), (1, 1)], 2), (vec![r(0, 0, 1, 1), r(1, 0, 1, 1)], 1));
        assert_eq!(pack(&[(2, 1), (2, 1)], 2), (vec![r(0, 0, 2, 1), r(0, 1, 2, 1)], 2));
    }

    #[test]
    fn pack_preenche_buracos_e_limita_largura() {
        assert_eq!(pack(&[(1, 2), (1, 1), (1, 1)], 2), (vec![r(0, 0, 1, 2), r(1, 0, 1, 1), r(1, 1, 1, 1)], 2));
        assert_eq!(pack(&[(5, 1)], 3), (vec![r(0, 0, 3, 1)], 1));
        assert_eq!(pack(&[], 2), (vec![], 0));
    }

    #[test]
    fn settle_mantem_quem_cabe() {
        let items = [r(0, 0, 2, 1), r(0, 1, 2, 1), r(2, 0, 1, 2)];
        assert_eq!(settle(&items, None, 3, 2), Some(items.to_vec()));
    }

    #[test]
    fn settle_empurra_quem_colide_para_a_vaga_livre() {
        // docker (0,0,2,1) e vitals (2,0,1,2); mover vitals para (0,0) 1×1 empurra o docker
        let items = [r(0, 0, 2, 1), r(2, 0, 1, 2)];
        let out = settle(&items, Some((1, r(0, 0, 1, 1))), 3, 2).unwrap();
        assert_eq!(out[1], r(0, 0, 1, 1));
        assert_eq!(out[0], r(1, 0, 2, 1));
    }

    #[test]
    fn settle_recusa_quando_nao_cabe() {
        let items = [r(0, 0, 2, 2), r(2, 0, 1, 2)];
        // docker ocupando a grade toda: vitals não tem onde ficar
        assert_eq!(settle(&items, Some((0, r(0, 0, 3, 2))), 3, 2), None);
        // fora da grade
        assert_eq!(settle(&items, Some((1, r(2, 1, 1, 2))), 3, 2), None);
    }

    #[test]
    fn settle_encolhe_a_grade() {
        // de 3 para 2 colunas: o vitals da coluna 2 desce para a primeira vaga
        let items = [r(0, 0, 2, 1), r(2, 0, 1, 1)];
        assert_eq!(settle(&items, None, 2, 2), Some(vec![r(0, 0, 2, 1), r(0, 1, 1, 1)]));
        // de 2 linhas para 1 com dois apps de largura toda: não cabe
        assert_eq!(settle(&[r(0, 0, 2, 1), r(0, 1, 2, 1)], None, 2, 1), None);
    }

    #[test]
    fn add_fica_no_lugar_usa_vaga_encolhe_ou_cresce() {
        assert_eq!(add(&[r(0, 0, 1, 1)], r(1, 0, 1, 1), 2, 1), (r(1, 0, 1, 1), 1));
        // lugar pedido ocupado: primeira vaga
        assert_eq!(add(&[r(0, 0, 1, 1)], r(0, 0, 1, 1), 2, 1), (r(1, 0, 1, 1), 1));
        // 2×1 não cabe ao lado; 1×1 cabe
        assert_eq!(add(&[r(0, 0, 1, 1)], r(0, 0, 2, 1), 2, 1), (r(1, 0, 1, 1), 1));
        // grade cheia: ganha uma linha
        assert_eq!(add(&[r(0, 0, 2, 1)], r(0, 0, 2, 1), 2, 1), (r(0, 1, 2, 1), 2));
    }
}
