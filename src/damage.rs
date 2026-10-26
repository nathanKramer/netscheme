//! Works out which parts of the window changed between two frames, so
//! that only those are painted again. Layout is redone in full, as it's
//! cheap; painting is what's slow. Two display lists are compared item by
//! item, keeping their order, since an item drawn later covers one drawn
//! earlier. When one scrolling layer, the page or a fixed-height frame,
//! has scrolled, its pixels are moved and only the strip it uncovers is
//! painted, as browsers do.

use std::collections::HashMap;

use crate::layout::{Clip, DisplayItem};

/// Scrolls past this many changed items, the whole window is painted again.
const MAX_CHANGES: usize = 400;

/// Above this many separate areas, they're painted as one.
const MAX_AREAS: usize = 12;

/// A display list in window coordinates, as painted.
pub struct Frame {
    pub items: Vec<DisplayItem>,
    /// How far the page, under `None`, and each fixed-height frame is
    /// scrolled, with the area it's shown in.
    pub layers: HashMap<Option<usize>, (f32, Clip)>,
    pub screen: Clip,
}

/// Pixels in `area` move down by `dy`, which may be negative.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Blit {
    pub area: Clip,
    pub dy: i32,
}

/// How to bring a window showing `old` up to date with `new`.
#[derive(Debug, PartialEq)]
pub struct Changes {
    pub blit: Option<Blit>,
    /// Areas to paint again, after the blit, in whole pixels.
    pub areas: Vec<Clip>,
}

/// An item that draws something, with the clip it's drawn in and the layer it scrolls with.
struct Drawn<'a> {
    item: &'a DisplayItem,
    clip: Clip,
    layer: Option<usize>,
}

/// The items of `frame` that draw something, and which frame layer each layer is inside.
fn drawn(frame: &Frame) -> (Vec<Drawn<'_>>, HashMap<usize, Option<usize>>) {
    let (mut clip, mut layer) = (frame.screen, None);
    let mut parents = HashMap::new();
    let mut out = Vec::with_capacity(frame.items.len());
    for item in &frame.items {
        match item {
            DisplayItem::Clip(c) => clip = c.intersect(frame.screen),
            DisplayItem::Layer(next) => {
                if let Some(id) = next {
                    parents.entry(*id).or_insert(layer);
                }
                layer = *next;
            }
            item => out.push(Drawn { item, clip, layer }),
        }
    }
    (out, parents)
}

/// What changed from `old` to `new`, or `None` if so much did that the
/// whole window should be painted. `bounds` gives the area an item may
/// touch.
pub fn changes(old: &Frame, new: &Frame, bounds: impl Fn(&DisplayItem) -> Clip) -> Option<Changes> {
    if old.screen != new.screen {
        return None;
    }
    let screen = new.screen;
    let (old_drawn, old_parents) = drawn(old);
    let (new_drawn, new_parents) = drawn(new);

    // A blit is possible when just one layer scrolled, and its area stayed put.
    let mut scrolled = Vec::new();
    for (layer, &(scroll, area)) in &new.layers {
        let Some(&(old_scroll, old_area)) = old.layers.get(layer) else { continue };
        if old_scroll != scroll {
            if old_area != area {
                return None;
            }
            scrolled.push((*layer, old_scroll - scroll, area));
        }
    }
    // Pixels move whole, so what's blitted is the whole pixels inside the
    // layer's area, and the part-pixels at its edges are painted again.
    let inner = |a: Clip| Clip { x0: a.x0.ceil(), y0: a.y0.ceil(), x1: a.x1.floor(), y1: a.y1.floor() };
    let (blit, edges) = match scrolled[..] {
        [] => (None, None),
        [(layer, dy, area)] if !inner(area).is_empty() => {
            (Some((layer, Blit { area: inner(area), dy: dy.round() as i32 })), Some(area.round_out()))
        }
        _ => return None,
    };
    // Whether items in `layer` move with the blit: it's the scrolled layer or inside it.
    let moves = |mut layer: Option<usize>, parents: &HashMap<usize, Option<usize>>| {
        let Some((scrolled, _)) = blit else { return false };
        loop {
            if layer == scrolled {
                return true;
            }
            match layer {
                Some(id) => layer = parents.get(&id).copied().flatten(),
                None => return false,
            }
        }
    };
    // Moved items are compared only where moved pixels landed: the strip
    // they uncover is painted anyway, and the area's own clip stays put.
    let landed = blit.map(|(_, b)| b.area.intersect(b.area.shifted(b.dy as f32)));
    // Every empty clip is the same, so items scrolled out of sight still match.
    let key = |clip: Clip| if clip.is_empty() { Clip::rect(0.0, 0.0, 0.0, 0.0) } else { clip };

    // Where the old items are now, after the blit, with the clip to compare.
    let old_items: Vec<(DisplayItem, Clip, Clip)> = old_drawn
        .iter()
        .map(|d| match (blit, landed) {
            (Some((_, b)), Some(landed)) if moves(d.layer, &old_parents) => {
                let clip = d.clip.shifted(b.dy as f32).intersect(b.area);
                (d.item.clone().shifted(b.dy as f32), clip, key(clip.intersect(landed)))
            }
            _ => (d.item.clone(), d.clip, d.clip),
        })
        .collect();
    let new_keys: Vec<Clip> = new_drawn
        .iter()
        .map(|d| match landed {
            Some(landed) if moves(d.layer, &new_parents) => key(d.clip.intersect(landed)),
            _ => d.clip,
        })
        .collect();
    let same = |i: usize, j: usize| old_items[i].0 == *new_drawn[j].item && old_items[i].2 == new_keys[j];
    let (removed, added) = diff(old_items.len(), new_drawn.len(), same, MAX_CHANGES)?;

    let mut areas = Vec::new();
    let mut damage = |area: Clip| {
        let area = area.round_out().intersect(screen);
        if !area.is_empty() {
            areas.push(area);
        }
    };
    for i in removed {
        damage(bounds(&old_items[i].0).intersect(old_items[i].1));
    }
    for j in added {
        damage(bounds(new_drawn[j].item).intersect(new_drawn[j].clip));
    }
    if let (Some((_, blit)), Some(outer)) = (blit, edges) {
        let Blit { area, dy } = blit;
        let dy = dy as f32;
        // Side by side, so they don't merge into one big area.
        damage(Clip { y1: area.y0, ..outer });
        damage(Clip { y0: area.y1, ..outer });
        damage(Clip { x1: area.x0, y0: area.y0, y1: area.y1, ..outer });
        damage(Clip { x0: area.x1, y0: area.y0, y1: area.y1, ..outer });
        // The strip the moved pixels uncover, which is all of the area if they moved out of it.
        damage(
            match dy > 0.0 {
                true => Clip { y1: area.y0 + dy, ..area },
                false => Clip { y0: area.y1 + dy, ..area },
            }
            .intersect(area),
        );
        // Items that stay put but were drawn in the area moved with it, so
        // they're painted again where they were and where they went. Those
        // under an opaque box that fills the area are hidden by it anyway.
        let first_moving = new_drawn.iter().position(|d| moves(d.layer, &new_parents)).unwrap_or(new_drawn.len());
        let covered = new_drawn[..first_moving].iter().rposition(|d| match d.item {
            DisplayItem::Rect { x, y, w, h, color, radius } => {
                // As painted, in whole pixels.
                let r = Clip::rect(*x, *y, *w, *h).intersect(d.clip);
                let painted = Clip { x0: r.x0.round(), y0: r.y0.round(), x1: r.x1.round(), y1: r.y1.round() };
                color.is_opaque() && *radius == 0.0 && painted.covers(area)
            }
            _ => false,
        });
        let skip = covered.map_or(0, |i| i + 1);
        let staying = old_drawn.iter().skip(skip).filter(|d| !moves(d.layer, &old_parents));
        for d in staying.chain(new_drawn.iter().skip(skip).filter(|d| !moves(d.layer, &new_parents))) {
            let part = bounds(d.item).intersect(d.clip).intersect(area);
            if !part.is_empty() {
                damage(part);
                damage(part.shifted(dy).intersect(area));
            }
        }
    }
    Some(Changes { blit: blit.map(|(_, b)| b), areas: merge(areas) })
}

/// Joins areas whose union is no bigger than the two of them, and makes
/// many into one. Others are left apart, even overlapping, as painting a
/// pixel twice is cheaper than painting pixels that didn't change.
pub fn merge(mut areas: Vec<Clip>) -> Vec<Clip> {
    let size = |c: Clip| (c.x1 - c.x0).max(0.0) * (c.y1 - c.y0).max(0.0);
    let mut merged: Vec<Clip> = Vec::new();
    while let Some(mut area) = areas.pop() {
        // Joining two can make the result worth joining with one already merged.
        while let Some(i) = merged.iter().position(|&m| size(m.union(area)) <= size(m) + size(area)) {
            area = area.union(merged.swap_remove(i));
        }
        merged.push(area);
    }
    if merged.len() > MAX_AREAS {
        let all = merged.iter().copied().reduce(Clip::union);
        return all.into_iter().collect();
    }
    merged
}

/// The shortest way to turn a list of `n` items into one of `m`, by
/// Myers's algorithm: which items of the first are removed, and which of
/// the second are added. Items `i` and `j` are the same if `same(i, j)`.
/// `None` if it takes more than `limit` changes.
fn diff(n: usize, m: usize, same: impl Fn(usize, usize) -> bool, limit: usize) -> Option<(Vec<usize>, Vec<usize>)> {
    // Items the same at the start and end aren't searched.
    let mut start = 0;
    while start < n && start < m && same(start, start) {
        start += 1;
    }
    let mut end = 0;
    while end < n - start && end < m - start && same(n - 1 - end, m - 1 - end) {
        end += 1;
    }
    let (n0, m0) = (n - start - end, m - start - end);
    let same = |i: usize, j: usize| same(start + i, start + j);
    let (n, m) = (n0 as isize, m0 as isize);
    let max = (n + m).min(limit as isize);
    let offset = max + 1;
    let mut v = vec![0isize; 2 * offset as usize + 1];
    let mut trace = Vec::new();
    let mut reached = None;
    'search: for d in 0..=max {
        trace.push(v.clone());
        for k in (-d..=d).step_by(2) {
            let at = |k: isize| (k + offset) as usize;
            let mut x = if k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]) { v[at(k + 1)] } else { v[at(k - 1)] + 1 };
            let mut y = x - k;
            while x < n && y < m && same(x as usize, y as usize) {
                (x, y) = (x + 1, y + 1);
            }
            v[at(k)] = x;
            if x >= n && y >= m {
                reached = Some(d);
                break 'search;
            }
        }
    }
    reached?;
    let (mut removed, mut added) = (Vec::new(), Vec::new());
    let (mut x, mut y) = (n, m);
    for (d, v) in trace.iter().enumerate().rev() {
        let d = d as isize;
        let at = |k: isize| (k + offset) as usize;
        let k = x - y;
        let previous = if k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]) { k + 1 } else { k - 1 };
        let px = v[at(previous)];
        let py = px - previous;
        while x > px && y > py {
            (x, y) = (x - 1, y - 1);
        }
        if d > 0 {
            if x == px {
                added.push(start + py as usize);
            } else {
                removed.push(start + px as usize);
            }
        }
        (x, y) = (px, py);
    }
    Some((removed, added))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Color;

    fn edits(a: &str, b: &str) -> (Vec<usize>, Vec<usize>) {
        let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
        let (mut removed, mut added) = diff(a.len(), b.len(), |i, j| a[i] == b[j], 100).unwrap();
        removed.sort();
        added.sort();
        (removed, added)
    }

    #[test]
    fn diffs_find_the_fewest_changes() {
        assert_eq!(edits("abc", "abc"), (vec![], vec![]));
        assert_eq!(edits("abc", "axc"), (vec![1], vec![1]));
        let (removed, added) = edits("abcabba", "cbabac");
        assert_eq!(removed.len() + added.len(), 5, "Myers's own example takes five changes");
        let kept = |s: &str, gone: &[usize]| s.chars().enumerate().filter(|(i, _)| !gone.contains(i)).map(|(_, c)| c).collect::<String>();
        assert_eq!(kept("abcabba", &removed), kept("cbabac", &added), "what's left of each is the same");
        assert_eq!(edits("ab", "ba").0.len() + edits("ab", "ba").1.len(), 2, "swapping two is a change");
        assert_eq!(edits("", "ab"), (vec![], vec![0, 1]));
        assert!(diff(3, 3, |_, _| false, 2).is_none(), "too many changes");
    }

    fn rect(x: f32, y: f32, w: f32, h: f32, shade: u8) -> DisplayItem {
        DisplayItem::Rect { x, y, w, h, color: Color(shade, shade, shade, 255), radius: 0.0 }
    }

    fn bounds(item: &DisplayItem) -> Clip {
        match item {
            DisplayItem::Rect { x, y, w, h, .. } => Clip::rect(*x, *y, *w, *h),
            _ => Clip::NONE,
        }
    }

    fn frame(items: Vec<DisplayItem>, layers: &[(Option<usize>, f32, Clip)]) -> Frame {
        let layers = layers.iter().map(|&(layer, scroll, area)| (layer, (scroll, area))).collect();
        Frame { items, layers, screen: Clip::rect(0.0, 0.0, 100.0, 100.0) }
    }

    #[test]
    fn a_changed_item_damages_where_it_was_and_is() {
        let old = frame(vec![rect(0.0, 0.0, 100.0, 100.0, 0), rect(10.0, 10.0, 10.0, 10.0, 1)], &[]);
        let new = frame(vec![rect(0.0, 0.0, 100.0, 100.0, 0), rect(10.0, 10.0, 10.0, 10.0, 2)], &[]);
        let changes = changes(&old, &new, bounds).unwrap();
        assert_eq!(changes, Changes { blit: None, areas: vec![Clip::rect(10.0, 10.0, 10.0, 10.0)] });

        let moved = frame(vec![rect(0.0, 0.0, 100.0, 100.0, 0), rect(50.0, 50.0, 10.0, 10.0, 1)], &[]);
        assert_eq!(super::changes(&old, &moved, bounds).unwrap().areas.len(), 2, "the old place and the new");
        assert_eq!(super::changes(&old, &old, bounds).unwrap().areas, vec![], "nothing changed");
    }

    #[test]
    fn areas_merge_only_when_that_paints_no_more() {
        let strip = Clip::rect(0.0, 90.0, 100.0, 10.0);
        let column = Clip::rect(0.0, 0.0, 1.0, 100.0);
        assert_eq!(merge(vec![strip, column]).len(), 2, "a thin column doesn't make the strip the whole window");
        let inside = Clip::rect(10.0, 92.0, 5.0, 5.0);
        assert_eq!(merge(vec![strip, inside]), vec![strip], "one inside another");
        let next = Clip::rect(0.0, 80.0, 100.0, 10.0);
        assert_eq!(merge(vec![strip, next]), vec![Clip::rect(0.0, 80.0, 100.0, 20.0)], "side by side");
    }

    #[test]
    fn a_change_of_clip_damages_what_it_clips() {
        let item = rect(0.0, 0.0, 50.0, 50.0, 1);
        let old = frame(vec![DisplayItem::Clip(Clip::rect(0.0, 0.0, 10.0, 10.0)), item.clone()], &[]);
        let new = frame(vec![DisplayItem::Clip(Clip::rect(0.0, 0.0, 20.0, 20.0)), item], &[]);
        assert_eq!(changes(&old, &new, bounds).unwrap().areas, vec![Clip::rect(0.0, 0.0, 20.0, 20.0)]);
    }

    #[test]
    fn scrolling_a_frame_moves_its_pixels_and_paints_the_strip() {
        let area = Clip::rect(0.0, 20.0, 100.0, 80.0);
        let content = |scroll: f32| {
            vec![
                rect(0.0, 0.0, 100.0, 20.0, 9),
                DisplayItem::Clip(area),
                rect(0.0, 20.0, 100.0, 80.0, 0),
                DisplayItem::Layer(Some(0)),
                rect(10.0, 30.0 - scroll, 10.0, 10.0, 1),
                rect(10.0, 130.0 - scroll, 10.0, 10.0, 2),
                DisplayItem::Layer(None),
                DisplayItem::Clip(Clip::NONE),
            ]
        };
        let old = frame(content(0.0), &[(None, 0.0, Clip::rect(0.0, 0.0, 100.0, 100.0)), (Some(0), 0.0, area)]);
        let new = frame(content(30.0), &[(None, 0.0, Clip::rect(0.0, 0.0, 100.0, 100.0)), (Some(0), 30.0, area)]);
        let changes = changes(&old, &new, bounds).unwrap();
        assert_eq!(changes.blit, Some(Blit { area, dy: -30 }));
        assert_eq!(changes.areas, vec![Clip::rect(0.0, 70.0, 100.0, 30.0)], "only the uncovered strip");
    }
}
