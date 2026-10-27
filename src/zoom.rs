//! Zooming the site. Zoom magnifies the page, as pinching a phone's screen
//! does, rather than laying it out again with bigger text: it's the same
//! page, drawn bigger, and it scrolls sideways as well as up and down.
//!
//! A page that frames other pages can be zoomed into forever. Once a frame
//! covers the whole view, the browser goes into it: it shows the framed page
//! in place of the site, magnified so that it looks just as it did. The page
//! around it is out of sight, so it isn't run or laid out any more, and the
//! frames inside the framed page get room to load more frames. Zooming or
//! scrolling out past the framed page's edges comes back out of the frame.

use crate::layout::FramedPage;

/// How much one notch of the mouse wheel, or Ctrl+=, zooms in.
pub const STEP: f32 = 1.25;
/// The most a page can be magnified. Going into frames resets it, so this
/// only stops zooming in on a page, or a part of one, without frames.
const MAX: f32 = 32.0;

/// How the site is zoomed. Offsets are in physical pixels, as scrolling is.
#[derive(Clone, Debug, PartialEq)]
pub struct Zoom {
    /// The frames the view has gone into, outermost first.
    pub entered: Vec<Entered>,
    /// How many times its own size the page shown is drawn: the site, or
    /// the innermost frame gone into.
    pub magnification: f32,
    /// How far the page shown is scrolled to the right. Up and down is the
    /// site's frame's own scrolling.
    pub x: f32,
}

/// A frame the view has gone into.
#[derive(Clone, Debug, PartialEq)]
pub struct Entered {
    /// The page in the frame, and where its state is kept.
    pub location: String,
    pub state_key: String,
    /// The page's width in its own logical pixels, which it keeps when shown
    /// in place of the site.
    pub width: f32,
    /// Where the page's top-left corner is on the page around it, and how
    /// big its pixels are there, in that page's logical pixels.
    pub at: (f32, f32),
    pub scale: f32,
}

impl Default for Zoom {
    fn default() -> Zoom {
        Zoom { entered: Vec::new(), magnification: 1.0, x: 0.0 }
    }
}

impl Zoom {
    /// Magnifies by `factor`, keeping what's at `point` (relative to the
    /// page's frame) where it is. `y` is how far the frame is scrolled
    /// down; returns how far it should be.
    pub fn zoom_at(&mut self, factor: f32, (px, py): (f32, f32), y: f32) -> f32 {
        // The site can't be made smaller than its frame, but a frame gone
        // into can: the view comes back out of it.
        let min = if self.entered.is_empty() { 1.0 } else { 0.01 };
        let magnification = (self.magnification * factor).clamp(min, MAX);
        let f = magnification / self.magnification;
        self.magnification = magnification;
        self.x = (self.x + px) * f - px;
        (y + py) * f - py
    }

    /// Goes into `frame`, which `scale` physical pixels to a logical one
    /// draw as they are. `y` is how far the frame is scrolled; returns how
    /// far the frame showing the framed page should be.
    pub fn enter(&mut self, frame: &FramedPage, scale: f32, y: f32) -> f32 {
        let pixel = scale * self.magnification;
        self.entered.push(Entered {
            location: frame.location.to_string(),
            state_key: frame.state_key.to_string(),
            width: (frame.area.x1 - frame.area.x0) / (pixel * frame.scale),
            at: (frame.area.x0 / pixel, frame.area.y0 / pixel),
            scale: frame.scale,
        });
        self.magnification *= frame.scale;
        self.x -= frame.area.x0;
        y - frame.area.y0
    }

    /// Comes out of the innermost frame gone into, if any, as `enter`
    /// went in.
    pub fn leave(&mut self, scale: f32, y: f32) -> Option<f32> {
        let entered = self.entered.pop()?;
        self.magnification /= entered.scale;
        let pixel = scale * self.magnification;
        self.x += entered.at.0 * pixel;
        Some(y + entered.at.1 * pixel)
    }

    /// The width of the page shown, in its own logical pixels, if it keeps
    /// one: a frame gone into does, and the site fills its frame.
    pub fn width(&self) -> Option<f32> {
        self.entered.last().map(|entered| entered.width)
    }

    /// How many times its own size the site looks, frames gone into
    /// included, as text: "250%", "×40", or for the very big, "×3.1e15".
    pub fn label(&self) -> String {
        let log = self.entered.iter().fold(f64::from(self.magnification).log10(), |log, e| log - f64::from(e.scale).log10());
        let exponent = log.floor();
        match log {
            ..1.0 => format!("{:.0}%", 10f64.powf(log) * 100.0),
            ..4.0 => format!("×{:.0}", 10f64.powf(log)),
            _ => format!("×{:.1}e{exponent}", 10f64.powf(log - exponent)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Clip;

    fn frame(area: Clip, scale: f32) -> FramedPage {
        FramedPage { area, scale, location: "inner.sx".into(), state_key: "/0/1".into() }
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer_still() {
        let mut zoom = Zoom::default();
        let y = zoom.zoom_at(2.0, (100.0, 50.0), 30.0);
        // What was 100 + 0 across and 50 + 30 down is twice as far now.
        assert_eq!((zoom.x, y), (100.0, 110.0));
        assert_eq!(zoom.label(), "200%");
        zoom.zoom_at(0.1, (0.0, 0.0), 0.0);
        assert_eq!(zoom.magnification, 1.0, "the site is never smaller than its frame");
    }

    #[test]
    fn going_into_a_frame_and_out_again_comes_back_to_the_same_place() {
        let mut zoom = Zoom { entered: Vec::new(), magnification: 4.0, x: 900.0 };
        // At twice the window's scale and four times magnified, a frame at
        // half scale 800 physical pixels across and down, 1000 wide.
        let y = zoom.enter(&frame(Clip::rect(800.0, 800.0, 1000.0, 600.0), 0.5), 2.0, 850.0);
        assert_eq!((zoom.x, y, zoom.magnification), (100.0, 50.0, 2.0));
        assert_eq!(zoom.entered[0].width, 250.0, "1000 physical pixels at 2 × 2 to a logical one");
        assert_eq!(zoom.entered[0].at, (100.0, 100.0));
        assert_eq!(zoom.width(), Some(250.0));
        assert_eq!(zoom.label(), "400%", "it looks no different");

        assert_eq!(zoom.leave(2.0, y), Some(850.0));
        assert_eq!(zoom, Zoom { entered: Vec::new(), magnification: 4.0, x: 900.0 });
        assert_eq!(zoom.leave(2.0, 0.0), None);
    }

    #[test]
    fn labels_stay_short_however_far_in() {
        let entered = Entered { location: String::new(), state_key: String::new(), width: 0.0, at: (0.0, 0.0), scale: 0.5 };
        let zoom = Zoom { entered: vec![entered; 60], magnification: 1.5, x: 0.0 };
        assert_eq!(zoom.label(), "×1.7e18");
        let zoom = Zoom { entered: vec![], magnification: 12.5, x: 0.0 };
        assert_eq!(zoom.label(), "×13");
    }
}
