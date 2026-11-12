//! Where the `<video>` element goes on the page.

use gpui::{Bounds, Pixels, Point};
use gpui_av::Placement;

/// The element's box in the browser's viewport, in CSS pixels, and how much
/// of each side is cut away.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Layout {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
    /// What to cut from the top, right, bottom and left.
    pub inset: [f32; 4],
}

impl Layout {
    /// The layout for a placement in a window whose canvas has its top left
    /// corner at `canvas` in the viewport. `None` when nothing of the
    /// picture is visible. GPUI's logical pixels are CSS pixels.
    pub fn new(placement: &Placement, canvas: Point<f32>) -> Option<Self> {
        let Bounds { origin, size } = placement.bounds;
        let (width, height) = (f32::from(size.width), f32::from(size.height));
        let visible = placement.bounds.intersect(&placement.clip);
        if width <= 0.0
            || height <= 0.0
            || visible.size.width <= Pixels::ZERO
            || visible.size.height <= Pixels::ZERO
        {
            return None;
        }
        let cut = |amount: Pixels| f32::from(amount).max(0.0);
        Some(Self {
            left: canvas.x + f32::from(origin.x),
            top: canvas.y + f32::from(origin.y),
            width,
            height,
            inset: [
                cut(visible.top() - placement.bounds.top()),
                cut(placement.bounds.right() - visible.right()),
                cut(placement.bounds.bottom() - visible.bottom()),
                cut(visible.left() - placement.bounds.left()),
            ],
        })
    }

    /// The CSS declarations that put the element there.
    pub fn css(&self) -> String {
        let [top, right, bottom, left] = self.inset;
        format!(
            "display:block;left:{}px;top:{}px;width:{}px;height:{}px;\
             clip-path:inset({top}px {right}px {bottom}px {left}px);",
            self.left, self.top, self.width, self.height
        )
    }
}

#[cfg(test)]
mod tests {
    use gpui::{point, px, size};

    use super::*;

    fn bounds(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
    }

    #[test]
    fn a_picture_inside_its_clip_is_not_cut() {
        let placement = Placement {
            bounds: bounds(100.0, 50.0, 640.0, 360.0),
            clip: bounds(100.0, 0.0, 640.0, 460.0),
        };
        let layout = Layout::new(&placement, point(8.0, 16.0)).unwrap();
        assert_eq!(
            layout,
            Layout {
                left: 108.0,
                top: 66.0,
                width: 640.0,
                height: 360.0,
                inset: [0.0; 4],
            }
        );
        assert_eq!(
            layout.css(),
            "display:block;left:108px;top:66px;width:640px;height:360px;\
             clip-path:inset(0px 0px 0px 0px);"
        );
    }

    #[test]
    fn a_picture_larger_than_its_clip_is_cut_on_each_side() {
        // Object fit "cover": the picture overflows left and right. A
        // scrolling parent hides its top 20 pixels as well.
        let placement = Placement {
            bounds: bounds(-40.0, 0.0, 480.0, 270.0),
            clip: bounds(0.0, 20.0, 400.0, 250.0),
        };
        let layout = Layout::new(&placement, point(0.0, 0.0)).unwrap();
        assert_eq!(layout.inset, [20.0, 40.0, 0.0, 40.0]);
    }

    #[test]
    fn a_picture_scrolled_out_of_view_has_no_layout() {
        let placement = Placement {
            bounds: bounds(0.0, 500.0, 320.0, 180.0),
            clip: bounds(0.0, 0.0, 320.0, 400.0),
        };
        assert_eq!(Layout::new(&placement, point(0.0, 0.0)), None);
        let empty = Placement {
            bounds: bounds(0.0, 0.0, 0.0, 180.0),
            clip: bounds(0.0, 0.0, 320.0, 400.0),
        };
        assert_eq!(Layout::new(&empty, point(0.0, 0.0)), None);
    }
}
