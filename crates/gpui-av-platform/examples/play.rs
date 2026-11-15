//! The smallest gpui-av app: one window that plays a file or a URL.
//!
//! ```sh
//! cargo run -p gpui-av-platform --example play -- film.mp4
//! cargo run -p gpui-av-platform --example play -- https://example.com/film.mkv
//! ```
//!
//! Click the picture to pause and play again.

use gpui::{
    App, AppContext, Bounds, Context, Entity, IntoElement, ParentElement, Render, Styled,
    Window, WindowBounds, WindowOptions, div, prelude::*, px, rgb, size,
};
use gpui_av::{ObjectFit, PlaybackState, Source, VideoPlayer, video};

struct Player {
    player: Entity<VideoPlayer>,
}

impl Render for Player {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.player.read(cx).state().clone();
        let status = match &state {
            PlaybackState::Failed(error) => Some(format!("Cannot play this: {error}")),
            PlaybackState::Loading => Some("Loading".to_string()),
            _ => None,
        };
        div()
            .id("player")
            .size_full()
            .bg(rgb(0x000000))
            .text_color(rgb(0xffffff))
            .child(video(&self.player).object_fit(ObjectFit::Contain).size_full())
            .children(status.map(|status| div().absolute().top_4().left_4().child(status)))
            .on_click(cx.listener(|this, _, _, cx| {
                this.player.update(cx, |player, _| {
                    if matches!(player.state(), PlaybackState::Playing) {
                        player.pause();
                    } else {
                        player.play();
                    }
                });
            }))
    }
}

fn main() {
    let Some(target) = std::env::args().nth(1) else {
        eprintln!("usage: play <file or URL>");
        std::process::exit(2);
    };
    gpui_platform::application().run(move |cx: &mut App| {
        // Once, at startup: this platform's backends, tried in order.
        gpui_av_platform::init(cx);
        let source = if target.starts_with("http://") || target.starts_with("https://") {
            Source::url(target)
        } else {
            Source::file(target)
        };
        let bounds = Bounds::centered(None, size(px(960.), px(540.)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..WindowOptions::default()
            },
            |_, cx| {
                let player = cx.new(|cx| VideoPlayer::new(source, cx));
                player.update(cx, |player, _| player.play());
                // Redraw when the player's state changes.
                cx.new(|cx| {
                    cx.observe(&player, |_, _, cx| cx.notify()).detach();
                    Player { player }
                })
            },
        );
        if let Err(error) = opened {
            eprintln!("cannot open a window: {error}");
            cx.quit();
        }
        cx.on_window_closed(|cx, _| cx.quit()).detach();
    });
}
