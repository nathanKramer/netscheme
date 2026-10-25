//! The geometry of a canvas's shapes. Every shape, outlines included, is
//! drawn as a filled outline: a list of contours filled by the nonzero
//! rule. Contours here all go the same way round, so overlapping ones
//! merge, and one going the other way cuts a hole.

use std::f32::consts::TAU;

pub type Point = (f32, f32);
pub type Contour = Vec<Point>;

/// A rectangle between two opposite corners.
pub fn rect((x0, y0): Point, (x1, y1): Point) -> Contour {
    let (x0, x1, y0, y1) = (x0.min(x1), x0.max(x1), y0.min(y1), y0.max(y1));
    vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
}

/// A rectangle's edge, `width` thick and centred on it.
pub fn rect_outline(a: Point, b: Point, width: f32) -> Vec<Contour> {
    let (x0, x1, y0, y1) = (a.0.min(b.0), a.0.max(b.0), a.1.min(b.1), a.1.max(b.1));
    let h = width / 2.0;
    let outer = rect((x0 - h, y0 - h), (x1 + h, y1 + h));
    if x1 - x0 <= width || y1 - y0 <= width {
        return vec![outer];
    }
    let mut inner = rect((x0 + h, y0 + h), (x1 - h, y1 - h));
    inner.reverse();
    vec![outer, inner]
}

/// A circle, as a polygon with enough sides that its edge is within about
/// a tenth of a pixel of round.
pub fn circle((cx, cy): Point, r: f32) -> Contour {
    let r = r.max(0.0);
    let sides = (7.0 * r.sqrt()).ceil().clamp(8.0, 512.0) as usize;
    (0..sides)
        .map(|i| {
            let angle = i as f32 * TAU / sides as f32;
            (cx + r * angle.cos(), cy + r * angle.sin())
        })
        .collect()
}

/// A circle's edge, `width` thick and centred on it.
pub fn circle_outline(centre: Point, r: f32, width: f32) -> Vec<Contour> {
    let outer = circle(centre, r + width / 2.0);
    if r <= width / 2.0 {
        return vec![outer];
    }
    let mut inner = circle(centre, r - width / 2.0);
    inner.reverse();
    vec![outer, inner]
}

/// A line `width` thick through `points`, back to the first if `closed`.
/// Each segment is a rectangle, and a circle at each joint rounds the corners.
pub fn stroke(points: &[Point], closed: bool, width: f32) -> Vec<Contour> {
    let h = width / 2.0;
    let n = points.len();
    let segments = if closed && n > 2 { n } else { n.saturating_sub(1) };
    let mut contours = Vec::new();
    for i in 0..segments {
        let (a, b) = (points[i], points[(i + 1) % n]);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let length = (dx * dx + dy * dy).sqrt();
        if length == 0.0 {
            continue;
        }
        let (nx, ny) = (-dy / length * h, dx / length * h);
        contours.push(vec![(a.0 - nx, a.1 - ny), (b.0 - nx, b.1 - ny), (b.0 + nx, b.1 + ny), (a.0 + nx, a.1 + ny)]);
    }
    let joints = if closed && n > 2 { 0..n } else { 1..n.saturating_sub(1) };
    contours.extend(joints.map(|i| circle(points[i], h)));
    contours
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Twice the signed area: positive for the way round these contours go.
    fn area(contour: &[Point]) -> f32 {
        (0..contour.len())
            .map(|i| {
                let ((ax, ay), (bx, by)) = (contour[i], contour[(i + 1) % contour.len()]);
                ax * by - bx * ay
            })
            .sum()
    }

    #[test]
    fn shapes_go_the_same_way_round_and_holes_the_other() {
        assert!(area(&rect((10.0, 10.0), (0.0, 0.0))) > 0.0, "whichever corners are given");
        assert!(area(&circle((5.0, 5.0), 3.0)) > 0.0);
        for contour in stroke(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)], false, 2.0) {
            assert!(area(&contour) > 0.0, "{contour:?}");
        }
        let ring = circle_outline((0.0, 0.0), 10.0, 2.0);
        assert!(area(&ring[0]) > 0.0 && area(&ring[1]) < 0.0);
        assert_eq!(circle_outline((0.0, 0.0), 1.0, 4.0).len(), 1, "too thick to have a hole");
    }

    #[test]
    fn strokes_join_their_segments() {
        let open = stroke(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)], false, 2.0);
        assert_eq!(open.len(), 3, "two segments and the joint between them");
        let closed = stroke(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)], true, 2.0);
        assert_eq!(closed.len(), 6, "three segments and three joints");
        assert!(stroke(&[(1.0, 1.0)], false, 2.0).is_empty());
        assert!(stroke(&[(1.0, 1.0), (1.0, 1.0)], false, 2.0).is_empty(), "a segment with no length");
    }
}
