//! Plays a file or URL in a window and logs what happens.
//!
//! ```sh
//! cargo run --release -p gpui-av-avfoundation --example play -- clips/h264-1920x1080-60fps.mp4
//! cargo run --release -p gpui-av-avfoundation --example play -- \
//!     --header "Authorization: Bearer token" --quit-after 30 http://127.0.0.1:8000/clip.mp4
//! ```
//!
//! Options: `--header "Name: value"` (repeatable), `--loop`, `--muted`,
//! `--quit-after <seconds>` (prints a summary and exits), `--seek-test`
//! (seeks to the middle after 3 s and times it).

#[cfg(target_os = "macos")]
fn main() {
    macos::main();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("this example plays through AVFoundation and runs on macOS only");
}

#[cfg(target_os = "macos")]
mod macos {
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use gpui::{
        App, AppContext, Bounds, Context, Entity, IntoElement, ParentElement, Render, Styled,
        Subscription, Window, WindowBounds, WindowOptions, div, px, rgb, size,
    };
    use gpui_av::{PlaybackState, PlayerEvent, Source, VideoPlayer, video};
    use gpui_av_avfoundation::AvFoundationBackend;

    struct Options {
        source: Source,
        looping: bool,
        muted: bool,
        quit_after: Option<Duration>,
        seek_test: bool,
    }

    fn options() -> Result<Options, String> {
        let mut args = std::env::args().skip(1);
        let mut headers = Vec::new();
        let (mut looping, mut muted, mut quit_after, mut seek_test) = (false, false, None, false);
        let mut target = None;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--header" => {
                    let header = args.next().ok_or("--header needs \"Name: value\"")?;
                    let (name, value) = header
                        .split_once(':')
                        .ok_or("a header looks like \"Name: value\"")?;
                    headers.push((name.trim().to_string(), value.trim().to_string()));
                }
                "--loop" => looping = true,
                "--muted" => muted = true,
                "--seek-test" => seek_test = true,
                "--quit-after" => {
                    let seconds: f64 = args
                        .next()
                        .and_then(|seconds| seconds.parse().ok())
                        .ok_or("--quit-after needs a number of seconds")?;
                    quit_after = Some(Duration::from_secs_f64(seconds));
                }
                _ if target.is_none() => target = Some(arg),
                _ => return Err(format!("unexpected argument {arg}")),
            }
        }
        let target = target.ok_or("usage: play [options] <file or URL>")?;
        let mut source = if target.starts_with("http://") || target.starts_with("https://") {
            Source::url(target)
        } else {
            Source::file(target)
        };
        for (name, value) in headers {
            source = source.header(name, value);
        }
        Ok(Options {
            source,
            looping,
            muted,
            quit_after,
            seek_test,
        })
    }

    struct PlayerView {
        player: Entity<VideoPlayer>,
        opened: Instant,
        first_frame: Option<Duration>,
        playing_since: Option<(Instant, u64)>,
        seek_started: Option<Instant>,
        seek_time: Option<Duration>,
        last_log: Instant,
        renders: u64,
        _events: Subscription,
    }

    impl PlayerView {
        fn new(options: &Options, cx: &mut Context<Self>) -> Self {
            let opened = Instant::now();
            let player = cx.new(|cx| VideoPlayer::new(options.source.clone(), cx));
            if let PlaybackState::Failed(error) = player.read(cx).state() {
                println!("[  0.000s] state Failed({error:?})");
            }
            player.update(cx, |player, cx| {
                player.set_looping(options.looping, cx);
                player.set_muted(options.muted, cx);
                player.play();
            });
            let events = cx.subscribe(&player, |this, player, event: &PlayerEvent, cx| {
                let at = this.opened.elapsed().as_secs_f64();
                match event {
                    PlayerEvent::StateChanged(state) => {
                        println!("[{at:7.3}s] state {state:?}");
                        if *state == PlaybackState::Playing && this.playing_since.is_none() {
                            let presented = player.read(cx).stats().frames_presented;
                            this.playing_since = Some((Instant::now(), presented));
                        }
                    }
                    PlayerEvent::InfoChanged => {
                        println!("[{at:7.3}s] info {:?}", player.read(cx).info());
                    }
                    PlayerEvent::Seeked(position) => {
                        if let Some(started) = this.seek_started.take() {
                            this.seek_time = Some(started.elapsed());
                        }
                        println!(
                            "[{at:7.3}s] seeked to {:.3}s in {:?}",
                            position.as_secs_f64(),
                            this.seek_time
                        );
                    }
                    _ => {}
                }
                cx.notify();
            });
            if options.seek_test {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_secs(3)).await;
                    let _ = this.update(cx, |this, cx| {
                        let middle = this.player.read(cx).duration().unwrap_or_default() / 2;
                        println!("seeking to {:.3}s", middle.as_secs_f64());
                        this.seek_started = Some(Instant::now());
                        this.player.update(cx, |player, _| player.seek(middle));
                    });
                })
                .detach();
            }
            if let Some(after) = options.quit_after {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(after).await;
                    let _ = this.update(cx, |this, cx| {
                        this.summary(cx);
                        cx.quit();
                    });
                })
                .detach();
            }
            Self {
                player,
                opened,
                first_frame: None,
                playing_since: None,
                seek_started: None,
                seek_time: None,
                last_log: Instant::now(),
                renders: 0,
                _events: events,
            }
        }

        fn summary(&self, cx: &App) {
            let player = self.player.read(cx);
            let stats = player.stats();
            let rate = self.playing_since.map(|(since, presented)| {
                (stats.frames_presented - presented) as f64 / since.elapsed().as_secs_f64()
            });
            let total = stats.frames_presented + stats.frames_dropped;
            let dropped_share = if total == 0 {
                0.0
            } else {
                stats.frames_dropped as f64 * 100.0 / total as f64
            };
            println!("--- summary ---");
            println!("state: {:?}", player.state());
            println!("time to first frame: {:?}", self.first_frame);
            println!(
                "frames presented: {}, dropped: {} ({dropped_share:.2}%)",
                stats.frames_presented, stats.frames_dropped
            );
            if let Some(rate) = rate {
                println!("frames per second while playing: {rate:.2}");
            }
            if let Some(seek) = self.seek_time {
                println!("seek settled in: {seek:?}");
            }
        }
    }

    impl Render for PlayerView {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let (stats, position) = {
                let player = self.player.read(cx);
                (player.stats(), player.position())
            };
            if self.first_frame.is_none() && stats.frames_presented > 0 {
                let first = self.opened.elapsed();
                self.first_frame = Some(first);
                println!("first frame after {first:?}");
            }
            self.renders += 1;
            if self.last_log.elapsed() >= Duration::from_secs(1) {
                println!(
                    "position {:7.3}s  presented {:6}  dropped {:4}  window frames/s {:5.1}",
                    position.as_secs_f64(),
                    stats.frames_presented,
                    stats.frames_dropped,
                    self.renders as f64 / self.last_log.elapsed().as_secs_f64()
                );
                self.last_log = Instant::now();
                self.renders = 0;
            }
            div()
                .size_full()
                .bg(rgb(0x000000))
                .child(video(&self.player).size_full())
        }
    }

    pub fn main() {
        let options = match options() {
            Ok(options) => options,
            Err(message) => {
                eprintln!("{message}");
                std::process::exit(2);
            }
        };
        gpui_platform::application().run(move |cx: &mut App| {
            gpui_av::set_default_backend(Rc::new(AvFoundationBackend::new()), cx);
            let bounds = Bounds::centered(None, size(px(1280.), px(720.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..WindowOptions::default()
                },
                |_, cx| cx.new(|cx| PlayerView::new(&options, cx)),
            )
            .expect("open a window");
            cx.activate(true);
        });
    }
}
