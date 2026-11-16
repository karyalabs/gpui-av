//! The Media Foundation backend end to end, without a window: a headless
//! GPUI app plays a clip through the public API, muted, and checks what it
//! reports. Windows only.
//!
//! The clip comes from `GPUI_AV_TEST_CLIP` (an MP4 with H.264 and AAC, at
//! least 4 s long; the Windows workflow makes one with ffmpeg). Without it
//! the test says so and passes, so `cargo test` works on any machine.

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() {
    windows_test::main();
}

#[cfg(windows)]
mod windows_test {
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use gpui::{App, AppContext, AsyncApp, Entity};
    use gpui_av::{Backend, ErrorKind, PlaybackState, PlayerEvent, Source, TrackKind, VideoPlayer};
    use gpui_av_mediafoundation::MediaFoundationBackend;

    struct Report {
        failures: Vec<String>,
    }

    impl Report {
        fn check(&mut self, name: &str, ok: bool, detail: impl std::fmt::Debug) {
            println!("{} {name}: {detail:?}", if ok { "ok  " } else { "FAIL" });
            if !ok {
                self.failures.push(name.to_string());
            }
        }
    }

    fn backend() -> Rc<dyn Backend> {
        Rc::new(MediaFoundationBackend::new())
    }

    async fn wait(cx: &mut AsyncApp, millis: u64) {
        cx.background_executor()
            .timer(Duration::from_millis(millis))
            .await;
    }

    /// Waits up to `limit` for the player to reach a state `done` accepts.
    async fn wait_for(
        player: &Entity<VideoPlayer>,
        limit: Duration,
        done: impl Fn(&PlaybackState) -> bool,
        cx: &mut AsyncApp,
    ) -> PlaybackState {
        let started = Instant::now();
        loop {
            let state = player.read_with(cx, |player, _| player.state().clone());
            if done(&state) || started.elapsed() > limit {
                return state;
            }
            wait(cx, 10).await;
        }
    }

    fn failure_kind(state: &PlaybackState) -> Option<ErrorKind> {
        match state {
            PlaybackState::Failed(error) => Some(error.kind()),
            _ => None,
        }
    }

    async fn checks(clip: PathBuf, report: &mut Report, cx: &mut AsyncApp) {
        // Playing a file.
        let opened = Instant::now();
        let player = cx.new(|cx| {
            let mut player = VideoPlayer::with_backend(Source::file(&clip), backend(), cx);
            player.set_muted(true, cx);
            player.play();
            player
        });
        let seeks = Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = seeks.clone();
        let _events = cx.update(|cx| {
            cx.subscribe(&player, move |_, event: &PlayerEvent, _| {
                if let PlayerEvent::Seeked(position) = event {
                    seen.borrow_mut().push(*position);
                }
            })
        });
        let state = wait_for(
            &player,
            Duration::from_secs(15),
            |state| matches!(state, PlaybackState::Playing | PlaybackState::Failed(_)),
            cx,
        )
        .await;
        report.check(
            "a file plays",
            state == PlaybackState::Playing,
            (&state, opened.elapsed()),
        );
        let info = player.read_with(cx, |player, _| player.info().clone());
        report.check(
            "media info: video, audio, size and length",
            info.has_video
                && info.has_audio == Some(true)
                && info.natural_size.is_some()
                && info
                    .duration
                    .is_some_and(|length| length >= Duration::from_secs(4)),
            &info,
        );
        let before = player.read_with(cx, |player, _| player.position());
        wait(cx, 700).await;
        let after = player.read_with(cx, |player, _| player.position());
        report.check(
            "the position moves on",
            after > before + Duration::from_millis(300),
            (before, after),
        );
        let buffered = player.read_with(cx, |player, _| player.buffered());
        report.check(
            "a playing file reports what is buffered",
            !buffered.is_empty(),
            &buffered,
        );

        player.update(cx, |player, _| player.seek(Duration::from_secs(2)));
        let started = Instant::now();
        while seeks.borrow().is_empty() && started.elapsed() < Duration::from_secs(5) {
            wait(cx, 10).await;
        }
        let landed = seeks.borrow().first().copied();
        report.check(
            "a seek reports where it landed",
            landed.is_some_and(|position| {
                position >= Duration::from_millis(1900) && position <= Duration::from_millis(2600)
            }),
            (landed, started.elapsed()),
        );

        player.update(cx, |player, _| player.pause());
        let state = wait_for(
            &player,
            Duration::from_secs(5),
            |state| *state == PlaybackState::Paused,
            cx,
        )
        .await;
        report.check("pause", state == PlaybackState::Paused, &state);
        cx.update(|_| drop(player));

        // Failures.
        let missing = clip.with_file_name("does-not-exist.mp4");
        let player = cx.new(|cx| VideoPlayer::with_backend(Source::file(missing), backend(), cx));
        let state = wait_for(
            &player,
            Duration::from_secs(10),
            |state| matches!(state, PlaybackState::Failed(_)),
            cx,
        )
        .await;
        report.check(
            "a missing file: NotFound",
            failure_kind(&state) == Some(ErrorKind::NotFound),
            &state,
        );

        let junk = std::env::temp_dir().join("gpui-av-junk.mp4");
        let bytes: Vec<u8> = (0..65536u32)
            .map(|value| (value.wrapping_mul(2_654_435_761) >> 24) as u8)
            .collect();
        if let Err(error) = std::fs::write(&junk, bytes) {
            report.check("write a junk file", false, error);
        }
        let player = cx.new(|cx| VideoPlayer::with_backend(Source::file(&junk), backend(), cx));
        let state = wait_for(
            &player,
            Duration::from_secs(10),
            |state| matches!(state, PlaybackState::Failed(_)),
            cx,
        )
        .await;
        report.check(
            "random bytes: Unsupported or Decode",
            matches!(
                failure_kind(&state),
                Some(ErrorKind::Unsupported | ErrorKind::Decode)
            ),
            &state,
        );

        // Two audio tracks to choose between.
        let tracks = clip.with_file_name("h264-1280x720-tracks.mp4");
        if tracks.exists() {
            let player = cx.new(|cx| {
                let mut player = VideoPlayer::with_backend(Source::file(&tracks), backend(), cx);
                player.set_muted(true, cx);
                player.play();
                player
            });
            wait_for(
                &player,
                Duration::from_secs(10),
                |state| matches!(state, PlaybackState::Playing | PlaybackState::Failed(_)),
                cx,
            )
            .await;
            let info = player.read_with(cx, |player, _| player.info().clone());
            let languages: Vec<_> = info
                .tracks
                .iter()
                .filter(|track| track.kind == TrackKind::Audio)
                .map(|track| track.language.clone())
                .collect();
            report.check(
                "two audio tracks are listed, the first on",
                languages.len() == 2 && info.audio_track == Some(0),
                (&info.tracks, info.audio_track),
            );
            player.update(cx, |player, _| player.select_audio_track(1));
            let started = Instant::now();
            let mut chosen = None;
            while started.elapsed() < Duration::from_secs(5) {
                chosen = player.read_with(cx, |player, _| player.info().audio_track);
                if chosen == Some(1) {
                    break;
                }
                wait(cx, 10).await;
            }
            report.check(
                "the second audio track can be chosen",
                chosen == Some(1),
                chosen,
            );
            // Media Foundation's MP4 source hands on no 3GPP text stream
            // (seen on Windows Server 2025), so the MP4's subtitles are
            // noted, not required; the Matroska clip below must show them.
            let text_tracks = text_track_count(&player, cx);
            println!("info subtitle tracks listed in the MP4: {text_tracks}");
            cx.update(|_| drop(player));
        }

        // Subtitles inside the media: "The first line" from 1 s to 4 s.
        let subtitles = clip.with_file_name("h264-1280x720-subtitles.mkv");
        if subtitles.exists() {
            let player = cx.new(|cx| {
                let mut player = VideoPlayer::with_backend(Source::file(&subtitles), backend(), cx);
                player.set_muted(true, cx);
                player.play();
                player
            });
            wait_for(
                &player,
                Duration::from_secs(10),
                |state| matches!(state, PlaybackState::Playing | PlaybackState::Failed(_)),
                cx,
            )
            .await;
            let started = Instant::now();
            let mut text_tracks = 0;
            while started.elapsed() < Duration::from_secs(3) && text_tracks == 0 {
                text_tracks = text_track_count(&player, cx);
                wait(cx, 50).await;
            }
            report.check(
                "the subtitle track is listed",
                text_tracks == 1,
                text_tracks,
            );
            player.update(cx, |player, _| player.select_text_track(Some(0)));
            let started = Instant::now();
            let mut text = None;
            while started.elapsed() < Duration::from_secs(8) {
                text = player.read_with(cx, |player, _| player.text());
                if text.is_some() {
                    break;
                }
                wait(cx, 50).await;
            }
            report.check(
                "subtitles show once a text track is on",
                text.as_deref() == Some("The first line"),
                &text,
            );
            cx.update(|_| drop(player));
        }

        // A file cut off inside its media data may play up to the cut, so
        // give it the clip's length to fail.
        let truncated = clip.with_file_name("broken-truncated.mp4");
        if truncated.exists() {
            let player = cx.new(|cx| {
                let mut player = VideoPlayer::with_backend(Source::file(&truncated), backend(), cx);
                player.set_muted(true, cx);
                player.play();
                player
            });
            let state = wait_for(
                &player,
                Duration::from_secs(30),
                |state| matches!(state, PlaybackState::Failed(_) | PlaybackState::Ended),
                cx,
            )
            .await;
            report.check(
                "a truncated file: Decode",
                failure_kind(&state) == Some(ErrorKind::Decode),
                &state,
            );
        }

        // Hosted runners have no sound device, so this one is reported, not
        // checked.
        let audio_only = clip.with_file_name("aac-audio-only.m4a");
        if audio_only.exists() {
            let player = cx.new(|cx| {
                let mut player =
                    VideoPlayer::with_backend(Source::file(&audio_only), backend(), cx);
                player.set_muted(true, cx);
                player.play();
                player
            });
            let state = wait_for(
                &player,
                Duration::from_secs(10),
                |state| matches!(state, PlaybackState::Playing | PlaybackState::Failed(_)),
                cx,
            )
            .await;
            println!("info an audio-only file: {state:?}");
        }

        http_checks(&clip, report, cx).await;
    }

    /// URLs with request headers, through the backend's own byte stream:
    /// a token the server needs, a wrong one, and a redirect to another
    /// origin, which must not get the token.
    async fn http_checks(clip: &Path, report: &mut Report, cx: &mut AsyncApp) {
        let Ok(bytes) = std::fs::read(clip) else {
            report.check("read the clip", false, clip);
            return;
        };
        let mut files = HashMap::from([("clip.mp4".to_string(), bytes)]);
        let remuxed = hls_files(&clip.with_file_name("h264-1280x720-subtitles.mkv"));
        if let Some(remuxed) = &remuxed {
            files.extend(remuxed.clone());
        }
        let folders = ["hls-ts", "hls-fmp4"].map(|folder| {
            let files = folder_files(&clip.with_file_name(folder), folder);
            (folder, files)
        });
        for (_, found) in &folders {
            if let Some(found) = found {
                files.extend(found.clone());
            }
        }
        let server = Server::start(files);
        // (name, route, file, token, expected failure, whether it must pass)
        let mut cases = vec![
            (
                "a URL with a token plays",
                "private",
                "clip.mp4",
                Some(TOKEN),
                None,
                true,
            ),
            (
                "a wrong token fails as unauthorized",
                "private",
                "clip.mp4",
                Some("Bearer wrong"),
                Some(ErrorKind::Unauthorized),
                true,
            ),
            (
                "a redirect to another origin drops the token",
                "hop",
                "clip.mp4",
                Some(TOKEN),
                None,
                true,
            ),
            (
                "a missing file fails as not found",
                "missing",
                "clip.mp4",
                Some(TOKEN),
                Some(ErrorKind::NotFound),
                true,
            ),
        ];
        if folders[0].1.is_some() {
            // MPEG-TS segments, what most servers send: these must play.
            cases.extend([
                (
                    "HLS (TS) with a token plays",
                    "private",
                    "hls-ts/index.m3u8",
                    Some(TOKEN),
                    None,
                    true,
                ),
                (
                    "HLS (TS) through a redirect to another origin drops the token",
                    "hop",
                    "hls-ts/index.m3u8",
                    Some(TOKEN),
                    None,
                    true,
                ),
                (
                    "HLS (TS) with a wrong token fails as unauthorized",
                    "private",
                    "hls-ts/index.m3u8",
                    Some("Bearer wrong"),
                    Some(ErrorKind::Unauthorized),
                    true,
                ),
            ]);
        } else {
            report.check("find the HLS (TS) clip", false, "no hls-ts folder");
        }
        // Fragmented MP4 segments, with and without the proxy: for the
        // record, as the engine's HLS may not take them at all.
        for file in ["hls-fmp4/index.m3u8", "hls/index.m3u8"] {
            cases.push((
                "HLS (fMP4) with a token",
                "private",
                file,
                Some(TOKEN),
                None,
                true,
            ));
            cases.push(("HLS (fMP4) with no headers", "open", file, None, None, true));
        }
        for (name, route, file, token, expect, required) in cases {
            let mut source = Source::url(server.url(route, file));
            if let Some(token) = token {
                source = source.header("Authorization", token);
            }
            let opened = Instant::now();
            let player = cx.new(|cx| {
                let mut player = VideoPlayer::with_backend(source, backend(), cx);
                player.set_muted(true, cx);
                player.play();
                player
            });
            let state = wait_for(
                &player,
                Duration::from_secs(15),
                |state| matches!(state, PlaybackState::Playing | PlaybackState::Failed(_)),
                cx,
            )
            .await;
            let ok = match expect {
                None => state == PlaybackState::Playing,
                Some(kind) => failure_kind(&state) == Some(kind),
            };
            if required {
                report.check(name, ok, (&state, opened.elapsed()));
            } else {
                println!(
                    "info {name} ({file}): {state:?} after {:?}",
                    opened.elapsed()
                );
            }
            if required && expect.is_none() && ok {
                let target = Duration::from_secs(2);
                player.update(cx, |player, _| player.seek(target));
                wait(cx, 1500).await;
                let position = player.read_with(cx, |player, _| player.position());
                report.check(
                    &format!("{name}: it seeks over HTTP"),
                    position >= target,
                    position,
                );
            }
            cx.update(|_| drop(player));
        }
        let leaked = server
            .log
            .lock()
            .map(|log| {
                log.iter()
                    .filter(|(path, authorized)| path.starts_with("/open/") && *authorized)
                    .count()
            })
            .unwrap_or_default();
        report.check(
            "the token never reached the other origin",
            leaked == 0,
            leaked,
        );
    }

    const TOKEN: &str = "Bearer test-token";

    /// Every file in `folder`, named `prefix/NAME`.
    fn folder_files(folder: &Path, prefix: &str) -> Option<HashMap<String, Vec<u8>>> {
        let mut files = HashMap::new();
        for entry in std::fs::read_dir(folder).ok()? {
            let path = entry.ok()?.path();
            let name = path.file_name()?.to_string_lossy().into_owned();
            files.insert(format!("{prefix}/{name}"), std::fs::read(&path).ok()?);
        }
        Some(files)
    }

    /// HLS (fragmented MP4) of a Matroska clip, made by the remuxer, as
    /// `hls/index.m3u8`, `hls/init.mp4` and `hls/N.m4s`.
    fn hls_files(clip: &Path) -> Option<HashMap<String, Vec<u8>>> {
        let file = std::fs::File::open(clip).ok()?;
        let mut remuxer = gpui_av_remux::Remuxer::open(std::io::BufReader::new(file)).ok()?;
        let mut files = HashMap::new();
        files.insert(
            "hls/index.m3u8".to_string(),
            remuxer
                .hls_playlist("init.mp4", |index| format!("{index}.m4s"))
                .into_bytes(),
        );
        files.insert("hls/init.mp4".to_string(), remuxer.init_segment());
        for index in 0..remuxer.segments().len() {
            files.insert(
                format!("hls/{index}.m4s"),
                remuxer.media_segment(index).ok()?,
            );
        }
        Some(files)
    }

    /// A local server with each file at `/private/NAME` (needs the token),
    /// `/hop/NAME` (needs it, and redirects to `/open/NAME` on the other
    /// name for this machine) and `/open/NAME` (refuses any
    /// `Authorization`, as a signed storage URL does). It logs each path
    /// and whether `Authorization` came with it.
    struct Server {
        port: u16,
        log: Arc<Mutex<Vec<(String, bool)>>>,
    }

    impl Server {
        fn start(files: HashMap<String, Vec<u8>>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let port = listener.local_addr().expect("address").port();
            let log = Arc::new(Mutex::new(Vec::new()));
            let (files, served) = (Arc::new(files), log.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let (files, log) = (files.clone(), served.clone());
                    std::thread::spawn(move || {
                        if let Err(error) = serve(stream, port, &files, &log) {
                            println!("info the test server: {error}");
                        }
                    });
                }
            });
            Self { port, log }
        }

        fn url(&self, route: &str, name: &str) -> String {
            format!("http://127.0.0.1:{}/{route}/{name}", self.port)
        }
    }

    fn serve(
        stream: TcpStream,
        port: u16,
        files: &HashMap<String, Vec<u8>>,
        log: &Mutex<Vec<(String, bool)>>,
    ) -> std::io::Result<()> {
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut writer = stream;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Ok(());
            }
            let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
            let (mut range, mut auth, mut host) = (None, None, String::new());
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header)? == 0 {
                    return Ok(());
                }
                let header = header.trim_end();
                if header.is_empty() {
                    break;
                }
                if let Some((name, value)) = header.split_once(':') {
                    let value = value.trim().to_string();
                    match name.trim().to_ascii_lowercase().as_str() {
                        "range" => range = Some(value),
                        "authorization" => auth = Some(value),
                        "host" => host = value,
                        _ => {}
                    }
                }
            }
            eprintln!(
                "server: GET {path} range {range:?} token {}",
                auth.is_some()
            );
            if let Ok(mut log) = log.lock() {
                log.push((path.clone(), auth.is_some()));
            }
            let empty = |writer: &mut TcpStream, status: &str, extra: &str| {
                write!(
                    writer,
                    "HTTP/1.1 {status}\r\n{extra}Content-Length: 0\r\n\r\n"
                )
            };
            let authorized = auth.as_deref() == Some(TOKEN);
            let (route, name) = path
                .trim_start_matches('/')
                .split_once('/')
                .unwrap_or(("", ""));
            match route {
                "private" | "hop" | "missing" if !authorized => {
                    empty(&mut writer, "401 Unauthorized", "")?;
                    continue;
                }
                "hop" => {
                    let other = if host.starts_with("127.0.0.1") {
                        "localhost"
                    } else {
                        "127.0.0.1"
                    };
                    let location = format!("Location: http://{other}:{port}/open/{name}\r\n");
                    empty(&mut writer, "302 Found", &location)?;
                    continue;
                }
                "open" if auth.is_some() => {
                    empty(&mut writer, "400 Bad Request", "")?;
                    continue;
                }
                "private" | "open" => {}
                _ => {
                    empty(&mut writer, "404 Not Found", "")?;
                    continue;
                }
            }
            let Some(clip) = files.get(name) else {
                empty(&mut writer, "404 Not Found", "")?;
                continue;
            };
            let size = clip.len() as u64;
            let (start, end) = range
                .as_deref()
                .and_then(|range| parse_range(range, size))
                .unwrap_or((0, size.saturating_sub(1)));
            if range.is_some() {
                write!(
                    writer,
                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{size}\r\n"
                )?;
            } else {
                write!(writer, "HTTP/1.1 200 OK\r\n")?;
            }
            let content_type = match name.rsplit('.').next() {
                Some("m3u8") => "application/vnd.apple.mpegurl",
                Some("ts") => "video/mp2t",
                Some("mkv") => "video/x-matroska",
                _ => "video/mp4",
            };
            write!(
                writer,
                "Accept-Ranges: bytes\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",
                end - start + 1
            )?;
            writer.write_all(&clip[start as usize..=end as usize])?;
        }
    }

    fn parse_range(range: &str, size: u64) -> Option<(u64, u64)> {
        let (start, end) = range.strip_prefix("bytes=")?.split_once('-')?;
        let last = size.checked_sub(1)?;
        let start: u64 = start.parse().ok()?;
        let end = end.parse::<u64>().map_or(last, |end| end.min(last));
        (start <= end).then_some((start, end))
    }

    pub fn main() {
        let Some(clip) = std::env::var_os("GPUI_AV_TEST_CLIP") else {
            println!("skipped: set GPUI_AV_TEST_CLIP to an MP4 with H.264 and AAC");
            return;
        };
        let clip = PathBuf::from(clip);
        gpui_platform::headless().run(move |cx: &mut App| {
            cx.spawn(async move |cx| {
                let mut report = Report {
                    failures: Vec::new(),
                };
                checks(clip, &mut report, cx).await;
                // GPUI's quit ends the process with success whatever
                // happened, so report and exit with the right code here.
                if report.failures.is_empty() {
                    println!("all passed");
                    std::process::exit(0);
                }
                println!("failed: {:#?}", report.failures);
                std::process::exit(1);
            })
            .detach();
        });
        println!("the app quit before the checks finished");
        std::process::exit(1);
    }

    fn text_track_count(player: &Entity<VideoPlayer>, cx: &mut AsyncApp) -> usize {
        player.read_with(cx, |player, _| {
            player
                .info()
                .tracks
                .iter()
                .filter(|track| track.kind == TrackKind::Text)
                .count()
        })
    }
}
