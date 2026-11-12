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

/// Where a window's canvas is in the browser's viewport: the CSS position
/// of its top left corner, and how many CSS pixels one of GPUI's logical
/// pixels covers. That is 1 unless the canvas is stretched: its backing
/// store was clamped to the GPU's largest texture, or the page scales it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Canvas {
    pub origin: Point<f32>,
    pub scale: Point<f32>,
}

impl Layout {
    /// The layout for a placement in a window drawn on `canvas`. `None`
    /// when nothing of the picture is visible.
    pub fn new(placement: &Placement, canvas: Canvas) -> Option<Self> {
        let Bounds { origin, size } = placement.bounds;
        let (scale_x, scale_y) = (canvas.scale.x, canvas.scale.y);
        let (width, height) = (
            f32::from(size.width) * scale_x,
            f32::from(size.height) * scale_y,
        );
        let visible = placement.bounds.intersect(&placement.clip);
        if width <= 0.0
            || height <= 0.0
            || visible.size.width <= Pixels::ZERO
            || visible.size.height <= Pixels::ZERO
        {
            return None;
        }
        let cut = |amount: Pixels, scale: f32| (f32::from(amount) * scale).max(0.0);
        Some(Self {
            left: canvas.origin.x + f32::from(origin.x) * scale_x,
            top: canvas.origin.y + f32::from(origin.y) * scale_y,
            width,
            height,
            inset: [
                cut(visible.top() - placement.bounds.top(), scale_y),
                cut(placement.bounds.right() - visible.right(), scale_x),
                cut(placement.bounds.bottom() - visible.bottom(), scale_y),
                cut(visible.left() - placement.bounds.left(), scale_x),
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

    fn canvas_at(x: f32, y: f32) -> Canvas {
        Canvas {
            origin: point(x, y),
            scale: point(1.0, 1.0),
        }
    }

    fn bounds(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
    }

    #[test]
    fn a_picture_inside_its_clip_is_not_cut() {
        let placement = Placement {
            bounds: bounds(100.0, 50.0, 640.0, 360.0),
            clip: bounds(100.0, 0.0, 640.0, 460.0),
        };
        let layout = Layout::new(&placement, canvas_at(8.0, 16.0)).unwrap();
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
        let layout = Layout::new(&placement, canvas_at(0.0, 0.0)).unwrap();
        assert_eq!(layout.inset, [20.0, 40.0, 0.0, 40.0]);
    }

    #[test]
    fn a_stretched_canvas_stretches_the_layout() {
        // Each logical pixel covers two CSS pixels across and down.
        let placement = Placement {
            bounds: bounds(100.0, 50.0, 320.0, 180.0),
            clip: bounds(100.0, 60.0, 320.0, 170.0),
        };
        let canvas = Canvas {
            origin: point(10.0, 0.0),
            scale: point(2.0, 2.0),
        };
        assert_eq!(
            Layout::new(&placement, canvas).unwrap(),
            Layout {
                left: 210.0,
                top: 100.0,
                width: 640.0,
                height: 360.0,
                inset: [20.0, 0.0, 0.0, 0.0],
            }
        );
    }

    #[test]
    fn a_picture_scrolled_out_of_view_has_no_layout() {
        let placement = Placement {
            bounds: bounds(0.0, 500.0, 320.0, 180.0),
            clip: bounds(0.0, 0.0, 320.0, 400.0),
        };
        assert_eq!(Layout::new(&placement, canvas_at(0.0, 0.0)), None);
        let empty = Placement {
            bounds: bounds(0.0, 0.0, 0.0, 180.0),
            clip: bounds(0.0, 0.0, 320.0, 400.0),
        };
        assert_eq!(Layout::new(&empty, canvas_at(0.0, 0.0)), None);
    }
}
