//! Pure grid layout math: equal-height thumbnails with per-window widths
//! proportional to the window's aspect ratio, packed into centered rows.
//! All coordinates are bottom-left origin (AppKit style), matching how the
//! overlay compositor draws.

use crate::config::Config;
use crate::state::Item;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
    Next,
    Prev,
}

#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn contains(&self, px: f64, py: f64) -> bool {
        px >= self.x && px <= self.x + self.w && py >= self.y && py <= self.y + self.h
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ItemRect {
    pub thumb: Rect,
    pub label: Rect,
}

#[derive(Clone, Debug)]
pub struct Layout {
    pub total_w: f64,
    pub total_h: f64,
    /// Row-major item indices.
    pub rows: Vec<Vec<usize>>,
    pub rects: Vec<ItemRect>,
}

const PAD: f64 = 20.0;
const GAP: f64 = 12.0;
const LABEL_H: f64 = 24.0;
const MIN_ITEM_W: f64 = 90.0;

pub fn compute(items: &[Item], screen_w: f64, screen_h: f64, cfg: &Config) -> Layout {
    if items.is_empty() {
        return Layout { total_w: 360.0, total_h: 84.0, rows: Vec::new(), rects: Vec::new() };
    }
    let max_w = (screen_w * cfg.max_width_frac).max(420.0);
    let max_h = screen_h * 0.86;
    let mut h = cfg.thumb_height.max(80.0);
    loop {
        let rows = pack(items, max_w - PAD * 2.0, h);
        let n = rows.len() as f64;
        let total_h = PAD * 2.0 + n * (h + LABEL_H) + (n - 1.0) * GAP;
        if total_h <= max_h || h <= 70.0 {
            return build(items, rows, max_w, h);
        }
        h *= 0.92;
    }
}

fn item_w(item: &Item, h: f64, avail: f64) -> f64 {
    (h * item.aspect).clamp(MIN_ITEM_W, avail)
}

fn pack(items: &[Item], avail: f64, h: f64) -> Vec<Vec<usize>> {
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut cur_w: f64 = 0.0;
    for (i, item) in items.iter().enumerate() {
        let w = item_w(item, h, avail);
        if !cur.is_empty() && cur_w + GAP + w > avail {
            rows.push(std::mem::take(&mut cur));
            cur_w = 0.0;
        }
        cur.push(i);
        cur_w = if cur_w == 0.0 { w } else { cur_w + GAP + w };
    }
    if !cur.is_empty() {
        rows.push(cur);
    }
    rows
}

fn build(items: &[Item], rows: Vec<Vec<usize>>, max_w: f64, h: f64) -> Layout {
    let n_rows = rows.len();
    let total_h =
        PAD * 2.0 + n_rows as f64 * (h + LABEL_H) + n_rows.saturating_sub(1) as f64 * GAP;
    let avail = max_w - PAD * 2.0;
    let mut rects: Vec<Option<ItemRect>> = vec![None; items.len()];
    for (r, row) in rows.iter().enumerate() {
        let widths: Vec<f64> = row.iter().map(|&i| item_w(&items[i], h, avail)).collect();
        let row_w: f64 = widths.iter().sum::<f64>() + GAP * row.len().saturating_sub(1) as f64;
        let mut x = (max_w - row_w) / 2.0;
        // Bottom-left origin: top row has the largest y. Position cards so the
        // bottom row's label bottom sits `PAD` above the overlay's bottom edge,
        // symmetric with the top row's card top (`PAD` below the overlay's top).
        let rows_below = (n_rows - 1 - r) as f64;
        let thumb_y = PAD + LABEL_H + rows_below * (h + LABEL_H + GAP);
        for (c, &i) in row.iter().enumerate() {
            let w = widths[c];
            rects[i] = Some(ItemRect {
                thumb: Rect { x, y: thumb_y, w, h },
                label: Rect { x, y: thumb_y - LABEL_H, w, h: LABEL_H },
            });
            x += w + GAP;
        }
    }
    Layout {
        total_w: max_w,
        total_h,
        rows,
        rects: rects.into_iter().map(|r| r.unwrap()).collect(),
    }
}

impl Layout {
    pub fn pos_of(&self, idx: usize) -> (usize, usize) {
        for (r, row) in self.rows.iter().enumerate() {
            if let Some(c) = row.iter().position(|&x| x == idx) {
                return (r, c);
            }
        }
        (0, 0)
    }

    /// Move from `idx` in direction `dir`, wrapping at grid edges.
    pub fn nav(&self, idx: usize, dir: Dir, wrap: bool) -> usize {
        if self.rects.is_empty() {
            return idx;
        }
        let n = self.rects.len();
        match dir {
            Dir::Next => (idx + 1) % n,
            Dir::Prev => (idx + n - 1) % n,
            Dir::Left | Dir::Right => {
                let (r, c) = self.pos_of(idx);
                let row = &self.rows[r];
                if row.len() <= 1 {
                    return idx;
                }
                let nc = match dir {
                    Dir::Left => {
                        if c == 0 && !wrap {
                            return idx;
                        }
                        (c + row.len() - 1) % row.len()
                    }
                    _ => {
                        if c + 1 == row.len() && !wrap {
                            return idx;
                        }
                        (c + 1) % row.len()
                    }
                };
                row[nc]
            }
            Dir::Up | Dir::Down => {
                let (r, _c) = self.pos_of(idx);
                let n_rows = self.rows.len();
                let nr = match dir {
                    Dir::Up => {
                        if r == 0 {
                            if wrap { n_rows - 1 } else { r }
                        } else {
                            r - 1
                        }
                    }
                    _ => {
                        if r + 1 == n_rows {
                            if wrap { 0 } else { r }
                        } else {
                            r + 1
                        }
                    }
                };
                if nr == r {
                    return idx;
                }
                self.nearest_in_row(nr, idx)
            }
        }
    }

    /// Pick the item in `row` whose center is horizontally closest to `idx`'s center.
    fn nearest_in_row(&self, row: usize, idx: usize) -> usize {
        let cur = &self.rects[idx].thumb;
        let cx = cur.x + cur.w / 2.0;
        self.rows[row]
            .iter()
            .min_by_key(|&&i| {
                let r = &self.rects[i].thumb;
                let c = r.x + r.w / 2.0;
                ((c - cx) * 1000.0) as i64
            })
            .copied()
            .unwrap_or(idx)
    }

    pub fn hit(&self, px: f64, py: f64) -> Option<usize> {
        self.rects.iter().position(|r| r.thumb.contains(px, py))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Item;

    fn item(id: u32, aspect: f64) -> Item {
        Item { id, pid: 1, owner: "t".into(), title: "t".into(), aspect, x: 0.0, y: 0.0, w: aspect * 100.0, h: 100.0, n_same_pid: 1 }
    }

    #[test]
    fn pack_respects_width() {
        // Default thumb height is 200 → w = 320 per window; avail = 760
        // → 320*2+12=652 fits, 3 wide (984) doesn't → 2 rows.
        let items = vec![item(1, 1.6), item(2, 1.6), item(3, 1.6)];
        let l = compute(&items, 1000.0, 800.0, &crate::config::Config::default());
        assert_eq!(l.rows.len(), 2);
        assert_eq!(l.rows[0].len(), 2);
        assert_eq!(l.rows[1].len(), 1);
    }

    #[test]
    fn pack_multiple_rows() {
        let items = vec![item(1, 1.6), item(2, 1.6), item(3, 1.6), item(4, 1.6), item(5, 1.6)];
        let l = compute(&items, 800.0, 800.0, &crate::config::Config::default());
        // avail = 800*0.8-40 = 600; per item 240; 2 per row → 3 rows
        assert_eq!(l.rows.len(), 3);
        assert_eq!(l.rows[0].len(), 2);
        assert_eq!(l.rows[1].len(), 2);
        assert_eq!(l.rows[2].len(), 1);
    }

    #[test]
    fn nav_wraps_edges() {
        let items = vec![item(1, 1.6), item(2, 1.6), item(3, 1.6), item(4, 1.6), item(5, 1.6)];
        let l = compute(&items, 800.0, 800.0, &crate::config::Config::default());
        // row0 = [0,1]; at 0 press Left → wraps to 1
        assert_eq!(l.nav(0, Dir::Left, true), 1);
        // at 1 press Right → wraps to 0
        assert_eq!(l.nav(1, Dir::Right, true), 0);
        // at 0 (row0) press Up → wraps to last row's nearest (row2 = [4])
        assert_eq!(l.nav(0, Dir::Up, true), 4);
        // at 4 (row2, centered) press Down → wraps to row0; item 4's center is
        // equidistant from items 0 and 1, so the tie resolves to the first.
        assert_eq!(l.nav(4, Dir::Down, true), 0);
        // global next/prev
        assert_eq!(l.nav(4, Dir::Next, true), 0);
        assert_eq!(l.nav(0, Dir::Prev, true), 4);
        // no wrap mode keeps position
        assert_eq!(l.nav(0, Dir::Left, false), 0);
        assert_eq!(l.nav(0, Dir::Up, false), 0);
    }

    #[test]
    fn hit_testing() {
        let items = vec![item(1, 1.6), item(2, 1.6)];
        let l = compute(&items, 800.0, 800.0, &crate::config::Config::default());
        let r = &l.rects[0].thumb;
        assert_eq!(l.hit(r.x + 1.0, r.y + 1.0), Some(0));
        // far outside
        assert_eq!(l.hit(0.0, 0.0), None);
    }
}
