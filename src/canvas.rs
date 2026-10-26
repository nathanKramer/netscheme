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

/// The subpaths of an SVG path's `d` data, each flattened into points and
/// marked if it was closed. Each point is passed through `at` first, and
/// `pixels` is how many pixels a unit of the path covers, which decides
/// how finely curves are flattened. Like SVG, it stops at the first thing
/// it can't read, keeping what came before.
pub fn path(d: &str, pixels: f32, at: impl Fn(Point) -> Point) -> Vec<(Contour, bool)> {
    let mut reader = PathReader { rest: d };
    let mut subpaths = Vec::new();
    let mut points: Contour = Vec::new();
    let (mut pen, mut start) = ((0.0, 0.0), (0.0, 0.0));
    // The last control point of a curve, for `S` and `T` to reflect.
    let mut control: Option<Point> = None;
    let mut command = None;
    let finish = |points: &mut Contour, closed: bool, subpaths: &mut Vec<(Contour, bool)>| {
        if points.len() > 1 {
            subpaths.push((points.iter().map(|&p| at(p)).collect(), closed));
        }
        points.clear();
    };
    loop {
        match reader.command() {
            Some(c) => command = Some(c),
            None if reader.at_end() => break,
            // More numbers repeat the command; after a move, they're lines.
            None => match command {
                Some('M') => command = Some('L'),
                Some('m') => command = Some('l'),
                Some('Z' | 'z') | None => break,
                _ => {}
            },
        }
        let Some(c) = command else { break };
        let relative = c.is_ascii_lowercase();
        let offset = |p: Point, pen: Point| if relative { (p.0 + pen.0, p.1 + pen.1) } else { p };
        let mut next_control = None;
        let ok = (|| -> Option<()> {
            match c.to_ascii_uppercase() {
                'M' => {
                    let p = offset(reader.point()?, pen);
                    finish(&mut points, false, &mut subpaths);
                    (pen, start) = (p, p);
                    points.push(p);
                }
                'L' => {
                    pen = offset(reader.point()?, pen);
                    points.push(pen);
                }
                'H' => {
                    let x = reader.number()?;
                    pen.0 = if relative { pen.0 + x } else { x };
                    points.push(pen);
                }
                'V' => {
                    let y = reader.number()?;
                    pen.1 = if relative { pen.1 + y } else { y };
                    points.push(pen);
                }
                'C' | 'S' => {
                    let c1 = match c.to_ascii_uppercase() {
                        'C' => offset(reader.point()?, pen),
                        _ => control.map_or(pen, |c| (2.0 * pen.0 - c.0, 2.0 * pen.1 - c.1)),
                    };
                    let (c2, end) = (offset(reader.point()?, pen), offset(reader.point()?, pen));
                    curve(&mut points, pixels, [pen, c1, c2, end]);
                    (pen, next_control) = (end, Some(c2));
                }
                'Q' | 'T' => {
                    let c1 = match c.to_ascii_uppercase() {
                        'Q' => offset(reader.point()?, pen),
                        _ => control.map_or(pen, |c| (2.0 * pen.0 - c.0, 2.0 * pen.1 - c.1)),
                    };
                    let end = offset(reader.point()?, pen);
                    // The same curve as a cubic one.
                    let third = |a: Point, b: Point| (a.0 + (b.0 - a.0) * 2.0 / 3.0, a.1 + (b.1 - a.1) * 2.0 / 3.0);
                    curve(&mut points, pixels, [pen, third(pen, c1), third(end, c1), end]);
                    (pen, next_control) = (end, Some(c1));
                }
                'A' => {
                    let radii = reader.point()?;
                    let rotation = reader.number()?;
                    let (large, sweep) = (reader.flag()?, reader.flag()?);
                    let end = offset(reader.point()?, pen);
                    arc(&mut points, pixels, pen, radii, rotation, large, sweep, end);
                    pen = end;
                }
                'Z' => {
                    finish(&mut points, true, &mut subpaths);
                    pen = start;
                    points.push(pen);
                }
                _ => return None,
            }
            Some(())
        })();
        if ok.is_none() {
            break;
        }
        // `S` only reflects a curve's control point, and `T` a quadratic one's.
        control = next_control;
        if c.eq_ignore_ascii_case(&'Z') {
            command = None;
        }
    }
    finish(&mut points, false, &mut subpaths);
    subpaths
}

/// Adds a cubic Bézier curve, without its first point, to `points`.
fn curve(points: &mut Contour, pixels: f32, [a, b, c, d]: [Point; 4]) {
    let length = [(a, b), (b, c), (c, d)].iter().map(|(p, q)| (q.0 - p.0).hypot(q.1 - p.1)).sum::<f32>() * pixels;
    let steps = (length * 4.0).sqrt().ceil().clamp(1.0, 256.0) as usize;
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        let u = 1.0 - t;
        let (wa, wb, wc, wd) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
        points.push((wa * a.0 + wb * b.0 + wc * c.0 + wd * d.0, wa * a.1 + wb * b.1 + wc * c.1 + wd * d.1));
    }
}

/// Adds an SVG elliptical arc from `from` to `to`, without its first point,
/// to `points`, as the SVG specification's appendix on arcs works it out.
#[allow(clippy::too_many_arguments)]
fn arc(points: &mut Contour, pixels: f32, from: Point, (rx, ry): Point, rotation: f32, large: bool, sweep: bool, to: Point) {
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if from == to {
        return;
    }
    if rx == 0.0 || ry == 0.0 {
        points.push(to);
        return;
    }
    let (sin, cos) = rotation.to_radians().sin_cos();
    let (hx, hy) = ((from.0 - to.0) / 2.0, (from.1 - to.1) / 2.0);
    let (x1, y1) = (cos * hx + sin * hy, -sin * hx + cos * hy);
    // Radii too small to reach are scaled up until they just do.
    let reach = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
    if reach > 1.0 {
        (rx, ry) = (rx * reach.sqrt(), ry * reach.sqrt());
    }
    let (rx2, ry2) = (rx * rx, ry * ry);
    let sign = if large == sweep { -1.0 } else { 1.0 };
    let k = sign * ((rx2 * ry2 - rx2 * y1 * y1 - ry2 * x1 * x1) / (rx2 * y1 * y1 + ry2 * x1 * x1)).max(0.0).sqrt();
    let (cx1, cy1) = (k * rx * y1 / ry, -k * ry * x1 / rx);
    let (cx, cy) = (cos * cx1 - sin * cy1 + (from.0 + to.0) / 2.0, sin * cx1 + cos * cy1 + (from.1 + to.1) / 2.0);
    let angle = |(ux, uy): Point, (vx, vy): Point| (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
    let u = ((x1 - cx1) / rx, (y1 - cy1) / ry);
    let start = angle((1.0, 0.0), u);
    let mut sweep_angle = angle(u, ((-x1 - cx1) / rx, (-y1 - cy1) / ry));
    if !sweep && sweep_angle > 0.0 {
        sweep_angle -= TAU;
    } else if sweep && sweep_angle < 0.0 {
        sweep_angle += TAU;
    }
    // As finely as `circle` divides a circle this size.
    let sides = 7.0 * (rx.max(ry) * pixels).sqrt();
    let steps = (sides * sweep_angle.abs() / TAU).ceil().clamp(1.0, 512.0) as usize;
    for i in 1..=steps {
        let (s, c) = (start + sweep_angle * i as f32 / steps as f32).sin_cos();
        points.push((cx + rx * c * cos - ry * s * sin, cy + rx * c * sin + ry * s * cos));
    }
    // Land exactly on the end, whatever rounding did.
    if let Some(last) = points.last_mut() {
        *last = to;
    }
}

/// Reads the commands and numbers of SVG path data.
struct PathReader<'a> {
    rest: &'a str,
}

impl PathReader<'_> {
    fn skip_separators(&mut self) {
        self.rest = self.rest.trim_start_matches(|c: char| c.is_whitespace() || c == ',');
    }

    fn at_end(&mut self) -> bool {
        self.skip_separators();
        self.rest.is_empty()
    }

    fn command(&mut self) -> Option<char> {
        self.skip_separators();
        let c = self.rest.chars().next().filter(|c| "MmLlHhVvCcSsQqTtAaZz".contains(*c))?;
        self.rest = &self.rest[1..];
        Some(c)
    }

    /// A number such as `-1.5`, `.5` or `2e3`. In `1.5.5` there are two.
    fn number(&mut self) -> Option<f32> {
        self.skip_separators();
        let bytes = self.rest.as_bytes();
        let mut end = 0;
        if matches!(bytes.first(), Some(b'+' | b'-')) {
            end += 1;
        }
        let digits = |end: &mut usize| {
            let from = *end;
            while bytes.get(*end).is_some_and(u8::is_ascii_digit) {
                *end += 1;
            }
            *end > from
        };
        let mut any = digits(&mut end);
        if bytes.get(end) == Some(&b'.') {
            end += 1;
            any |= digits(&mut end);
        }
        if !any {
            return None;
        }
        if matches!(bytes.get(end), Some(b'e' | b'E')) {
            let mut exponent = end + 1;
            if matches!(bytes.get(exponent), Some(b'+' | b'-')) {
                exponent += 1;
            }
            if digits(&mut exponent) {
                end = exponent;
            }
        }
        let n = self.rest[..end].parse().ok()?;
        self.rest = &self.rest[end..];
        Some(n)
    }

    fn point(&mut self) -> Option<Point> {
        Some((self.number()?, self.number()?))
    }

    /// An arc's flag, a single `0` or `1`, which needn't be followed by a separator.
    fn flag(&mut self) -> Option<bool> {
        self.skip_separators();
        let flag = match self.rest.as_bytes().first()? {
            b'0' => false,
            b'1' => true,
            _ => return None,
        };
        self.rest = &self.rest[1..];
        Some(flag)
    }
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
    fn paths_read_svg_path_data() {
        let square = path("M0 0 H10 V10 h-10 z", 1.0, |p| p);
        assert_eq!(square, [(vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)], true)]);

        let two = path("M0,0L1-1m5,5l1.5.5", 1.0, |p| p);
        assert_eq!(two, [(vec![(0.0, 0.0), (1.0, -1.0)], false), (vec![(6.0, 4.0), (7.5, 4.5)], false)], "relative moves, packed numbers");

        let moved = path("M1 1 L2 2", 1.0, |(x, y)| (x * 10.0, y * 10.0 + 5.0));
        assert_eq!(moved, [(vec![(10.0, 15.0), (20.0, 25.0)], false)], "points pass through `at`");

        assert_eq!(path("M0 0 L5 5 L oops", 1.0, |p| p).len(), 1, "keeps what came before an error");
    }

    #[test]
    fn path_curves_and_arcs_end_where_they_should() {
        let curved = path("M0 0 C0 10 10 10 10 0 S20 -10 20 0 Q25 5 30 0 T40 0", 4.0, |p| p);
        let points = &curved[0].0;
        assert_eq!(points.last(), Some(&(40.0, 0.0)));
        assert!(points.len() > 10, "curves are flattened into many points");
        assert!(points.iter().any(|p| p.1 > 7.0) && points.iter().any(|p| p.1 < -7.0), "and bend both ways");

        // Material's map pin: a teardrop with a round hole cut by an arc.
        let pin = path(
            "M12 2C8.13 2 5 5.13 5 9c0 5.25 7 13 7 13s7-7.75 7-13c0-3.87-3.13-7-7-7zm0 9.5a2.5 2.5 0 0 1 0-5 2.5 2.5 0 0 1 0 5z",
            4.0,
            |p| p,
        );
        assert_eq!(pin.len(), 2);
        let hole = &pin[1].0;
        assert!(hole.iter().all(|&(x, y)| ((x - 12.0).hypot(y - 9.0) - 2.5).abs() < 0.01), "the arcs make a circle");
        assert!(area(&pin[0].0).signum() != area(hole).signum(), "the hole goes the other way round");
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
