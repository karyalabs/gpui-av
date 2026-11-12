use gpui::{
    App, Bounds, ContentMask, DevicePixels, Element, ElementId, Entity, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, ObjectFit, Pixels, Refineable, Size, Style,
    StyleRefinement, Styled, Window, size,
};

use crate::{Placement, VideoPlayer};

/// Draws a [`VideoPlayer`]'s picture. Size it like any other element:
///
/// ```ignore
/// video(&player).object_fit(ObjectFit::Contain).size_full()
/// ```
pub fn video(player: &Entity<VideoPlayer>) -> Video {
    Video {
        player: player.clone(),
        object_fit: ObjectFit::Contain,
        style: StyleRefinement::default(),
    }
}

/// The element [`video()`] returns.
pub struct Video {
    player: Entity<VideoPlayer>,
    object_fit: ObjectFit,
    style: StyleRefinement,
}

impl Video {
    /// How the picture fits the element's bounds. The default is
    /// [`ObjectFit::Contain`]: the whole picture, letterboxed.
    pub fn object_fit(mut self, object_fit: ObjectFit) -> Self {
        self.object_fit = object_fit;
        self
    }
}

/// Where a picture of `natural_size` goes inside `bounds`.
fn fit(object_fit: &ObjectFit, bounds: Bounds<Pixels>, natural_size: Size<u32>) -> Bounds<Pixels> {
    let to_device = |pixels: u32| DevicePixels(i32::try_from(pixels).unwrap_or(i32::MAX));
    object_fit.get_bounds(
        bounds,
        size(
            to_device(natural_size.width),
            to_device(natural_size.height),
        ),
    )
}

impl Element for Video {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.refine(&self.style);
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (natural_size, wants_frames) = {
            let player = self.player.read(cx);
            (player.natural_size(), player.wants_frames())
        };
        // Nothing to place until the size of the picture is known.
        let Some(natural_size) = natural_size.filter(|size| size.width > 0 && size.height > 0)
        else {
            return;
        };
        if bounds.size.width <= Pixels::ZERO || bounds.size.height <= Pixels::ZERO {
            return;
        }
        let placement = Placement {
            bounds: fit(&self.object_fit, bounds, natural_size),
            clip: bounds,
        };
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            self.player.update(cx, |player, cx| {
                player.paint(placement, window, cx);
            });
        });
        if wants_frames {
            window.request_animation_frame();
        }
    }
}

impl IntoElement for Video {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Styled for Video {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

#[cfg(test)]
mod tests {
    use gpui::{point, px};

    use super::*;

    fn square(side: f32) -> Bounds<Pixels> {
        Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(side), px(side)),
        }
    }

    #[test]
    fn contain_letterboxes_a_wide_picture() {
        let placed = fit(&ObjectFit::Contain, square(200.), size(1920, 1080));
        assert_eq!(placed.size, size(px(200.), px(112.5)));
        assert_eq!(placed.origin, point(px(0.), px(43.75)));
    }

    #[test]
    fn cover_overflows_the_bounds() {
        let placed = fit(&ObjectFit::Cover, square(90.), size(1920, 1080));
        assert_eq!(placed.size, size(px(160.), px(90.)));
        assert_eq!(placed.origin, point(px(-35.), px(0.)));
    }
}
