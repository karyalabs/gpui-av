//! HTTP sources end to end, without a window: a headless GPUI app plays
//! clips through the public API (`VideoPlayer` with `AvFoundationBackend`)
//! from a local server, and checks where the request headers went.
//!
//! It runs on the main thread (`harness = false`), as AVFoundation needs.
//! It writes its own short H.264 clip with Apple's encoder, so it needs no
//! media and no display, and runs in CI on macOS. With `GPUI_AV_CLIPS` set
//! to the tier A clips folder it also plays the whole corpus over HTTP and
//! prints timings.
//!
//! Routes, like `scripts/serve.py`:
//! - `/private/NAME` needs the token (401 without).
//! - `/moved/NAME` redirects to `/private/NAME` on the same origin.
//! - `/hop/NAME` needs the token and redirects to `/open/NAME` on another
//!   host (`127.0.0.1` and `localhost` swap).
//! - `/open/NAME` answers 400 when `Authorization` is present, as a signed
//!   storage URL does: a clip through `/hop/` plays only if the header was
//!   dropped.
//! - `/drop/NAME` needs the token and cuts every range after the first.
//! - `/list.m3u8` needs the token and answers an HLS playlist.

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    macos::main();
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::Path;
    use std::ptr::NonNull;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use block2::RcBlock;
    use gpui::{App, AppContext, AsyncApp, Entity};
    use gpui_av::{ErrorKind, PlaybackState, PlayerEvent, Source, VideoPlayer};
    use gpui_av_avfoundation::AvFoundationBackend;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_av_foundation::{
        AVAssetWriter, AVAssetWriterInput, AVAssetWriterInputPixelBufferAdaptor, AVFileTypeMPEG4,
        AVMediaTypeVideo, AVVideoCodecKey, AVVideoCodecTypeH264, AVVideoHeightKey, AVVideoWidthKey,
    };
    use objc2_core_media::{CMTime, kCMTimeZero};
    use objc2_core_video::{
        CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetBaseAddress,
        CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight, CVPixelBufferLockBaseAddress,
        CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress, kCVPixelFormatType_32BGRA,
    };
    use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};

    const TOKEN: &str = "Bearer test-token";

    // ---------------------------------------------------------------------------
    // The server.

    #[derive(Clone, Debug)]
    struct Logged {
        path: String,
        authorized: bool,
        range: Option<String>,
    }

    struct Server {
        port: u16,
        log: Arc<Mutex<Vec<Logged>>>,
    }

    impl Server {
        fn start(files: HashMap<String, Arc<Vec<u8>>>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let port = listener.local_addr().expect("address").port();
            let log = Arc::new(Mutex::new(Vec::new()));
            let (files, served) = (Arc::new(files), log.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let (files, log) = (files.clone(), served.clone());
                    std::thread::spawn(move || {
                        let _ = serve(stream, port, &files, &log);
                    });
                }
            });
            Self { port, log }
        }

        fn url(&self, route: &str, name: &str) -> String {
            format!("http://127.0.0.1:{}/{route}/{name}", self.port)
        }

        /// What was requested since the last call.
        fn take_log(&self) -> Vec<Logged> {
            std::mem::take(&mut *self.log.lock().expect("log"))
        }
    }

    fn serve(
        stream: TcpStream,
        port: u16,
        files: &HashMap<String, Arc<Vec<u8>>>,
        log: &Mutex<Vec<Logged>>,
    ) -> std::io::Result<()> {
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut writer = stream;
        let mut ranges_on_drop = 0;
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
            log.lock().expect("log").push(Logged {
                path: path.clone(),
                authorized: auth.is_some(),
                range: range.clone(),
            });
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
                .unwrap_or(("", path.trim_start_matches('/')));
            match route {
                "private" | "hop" | "drop" if !authorized => {
                    empty(&mut writer, "401 Unauthorized", "")?;
                    continue;
                }
                "" if name == "list.m3u8" => {
                    if !authorized {
                        empty(&mut writer, "401 Unauthorized", "")?;
                        continue;
                    }
                    let body = "#EXTM3U\n#EXT-X-ENDLIST\n";
                    write!(
                        writer,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.apple.mpegurl\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    )?;
                    continue;
                }
                "moved" => {
                    empty(
                        &mut writer,
                        "302 Found",
                        &format!("Location: /private/{name}\r\n"),
                    )?;
                    continue;
                }
                "hop" => {
                    let other = if host.starts_with("127.0.0.1") {
                        "localhost"
                    } else {
                        "127.0.0.1"
                    };
                    let location = format!("http://{other}:{port}/open/{name}");
                    empty(
                        &mut writer,
                        "302 Found",
                        &format!("Location: {location}\r\n"),
                    )?;
                    continue;
                }
                "open" if auth.is_some() => {
                    empty(&mut writer, "400 Bad Request", "")?;
                    continue;
                }
                "private" | "open" | "drop" => {}
                _ => {
                    empty(&mut writer, "404 Not Found", "")?;
                    continue;
                }
            }
            let Some(file) = files.get(name) else {
                empty(&mut writer, "404 Not Found", "")?;
                continue;
            };
            let size = file.len() as u64;
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
            let length = end - start + 1;
            write!(
                writer,
                "Accept-Ranges: bytes\r\nContent-Type: video/mp4\r\nContent-Length: {length}\r\n\r\n"
            )?;
            let body = &file[start as usize..=end as usize];
            if route == "drop" {
                ranges_on_drop += 1;
                if ranges_on_drop > 1 || start > 0 {
                    // Part of the body, then the connection goes.
                    writer.write_all(&body[..body.len().min(4096)])?;
                    return Ok(());
                }
            }
            writer.write_all(body)?;
        }
    }

    fn parse_range(range: &str, size: u64) -> Option<(u64, u64)> {
        let (start, end) = range.strip_prefix("bytes=")?.split_once('-')?;
        let last = size.checked_sub(1)?;
        let start: u64 = start.parse().ok()?;
        let end = end.parse::<u64>().map_or(last, |end| end.min(last));
        (start <= end).then_some((start, end))
    }

    // ---------------------------------------------------------------------------
    // A clip, written by AVAssetWriter.

    /// Writes `seconds` of `width`x`height` H.264 at `fps` to `path`, the index
    /// first. Each frame is a flat colour that changes with time.
    fn write_clip(path: &Path, width: usize, height: usize, fps: i32, seconds: i32) {
        let _ = std::fs::remove_file(path);
        // SAFETY: AVAssetWriter calls with arguments of the documented types;
        // the pixel buffers are locked while written.
        unsafe {
            let url =
                NSURL::fileURLWithPath(&NSString::from_str(path.to_str().expect("utf-8 path")));
            let writer = AVAssetWriter::assetWriterWithURL_fileType_error(
                &url,
                AVFileTypeMPEG4.expect("mp4"),
            )
            .expect("an asset writer");
            writer.setShouldOptimizeForNetworkUse(true);
            let width_number = NSNumber::new_usize(width);
            let height_number = NSNumber::new_usize(height);
            let keys = [
                AVVideoCodecKey.expect("codec key"),
                AVVideoWidthKey.expect("width key"),
                AVVideoHeightKey.expect("height key"),
            ];
            let values: [&AnyObject; 3] = [
                AVVideoCodecTypeH264.expect("h264"),
                &width_number,
                &height_number,
            ];
            let settings = NSDictionary::from_slices(&keys, &values);
            let input = AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
                AVMediaTypeVideo.expect("video"),
                Some(&settings),
            );
            input.setExpectsMediaDataInRealTime(false);
            let adaptor = AVAssetWriterInputPixelBufferAdaptor::assetWriterInputPixelBufferAdaptorWithAssetWriterInput_sourcePixelBufferAttributes(&input, None);
            writer.addInput(&input);
            assert!(writer.startWriting(), "start writing: {:?}", writer.error());
            writer.startSessionAtSourceTime(kCMTimeZero);
            for frame in 0..(fps * seconds) {
                while !input.isReadyForMoreMediaData() {
                    std::thread::sleep(Duration::from_millis(1));
                }
                let mut buffer: *mut CVPixelBuffer = std::ptr::null_mut();
                CVPixelBufferCreate(
                    None,
                    width,
                    height,
                    kCVPixelFormatType_32BGRA,
                    None,
                    NonNull::from(&mut buffer),
                );
                let buffer = Retained::<CVPixelBuffer>::from_raw(buffer).expect("a pixel buffer");
                CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags(0));
                let base = CVPixelBufferGetBaseAddress(&buffer).cast::<u8>();
                let row = CVPixelBufferGetBytesPerRow(&buffer);
                let shade = (frame * 255 / (fps * seconds)) as u8;
                std::ptr::write_bytes(base, shade, row * CVPixelBufferGetHeight(&buffer));
                CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags(0));
                let time = CMTime::new(i64::from(frame), fps);
                assert!(adaptor.appendPixelBuffer_withPresentationTime(&buffer, time));
            }
            input.markAsFinished();
            let done = Arc::new(AtomicBool::new(false));
            let finished = done.clone();
            writer.finishWritingWithCompletionHandler(&RcBlock::new(move || {
                finished.store(true, Ordering::Release);
            }));
            let started = Instant::now();
            while !done.load(Ordering::Acquire) {
                assert!(
                    started.elapsed() < Duration::from_secs(30),
                    "the writer hung"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                writer.error().is_none(),
                "writing failed: {:?}",
                writer.error()
            );
        }
    }

    fn test_clip() -> Arc<Vec<u8>> {
        let path = std::env::temp_dir().join(format!("gpui-av-http-{}.mp4", std::process::id()));
        write_clip(&path, 320, 180, 30, 4);
        let bytes = std::fs::read(&path).expect("the clip");
        let _ = std::fs::remove_file(&path);
        Arc::new(bytes)
    }

    // ---------------------------------------------------------------------------
    // Playing.

    #[derive(Debug, Default)]
    struct Played {
        /// From creating the player to `Playing`.
        playing: Option<Duration>,
        /// From asking for a seek to `Seeked`.
        seek: Option<Duration>,
        /// Whether the position moved on after the seek.
        advanced: bool,
        error: Option<(ErrorKind, String)>,
    }

    /// Plays `url`, muted, until it plays (or fails); with `seek`, seeks there
    /// and waits for `Seeked` and for the position to move on.
    async fn play(
        url: &str,
        headers: &[(&str, &str)],
        seek: Option<Duration>,
        cx: &mut AsyncApp,
    ) -> Played {
        play_until(url, headers, seek, false, cx).await
    }

    /// As [`play`]; with `until_failed`, keeps going after `Playing` until
    /// the media fails (a file cut short plays to the cut first) or 15 s
    /// pass.
    async fn play_until(
        url: &str,
        headers: &[(&str, &str)],
        seek: Option<Duration>,
        until_failed: bool,
        cx: &mut AsyncApp,
    ) -> Played {
        let mut source = Source::url(url);
        for (name, value) in headers {
            source = source.header(*name, *value);
        }
        let opened = Instant::now();
        let seeked = Rc::new(RefCell::new(None::<Instant>));
        let player: Entity<VideoPlayer> = cx.new(|cx| {
            let mut player =
                VideoPlayer::with_backend(source, Rc::new(AvFoundationBackend::new()), cx);
            player.set_muted(true, cx);
            player.play();
            player
        });
        let seen = seeked.clone();
        let _events = cx.update(|cx| {
            cx.subscribe(&player, move |_, event: &PlayerEvent, _| {
                if let PlayerEvent::Seeked(_) = event {
                    seen.borrow_mut().get_or_insert_with(Instant::now);
                }
            })
        });
        let mut played = Played::default();
        let state = |cx: &mut AsyncApp| player.read_with(cx, |player, _| player.state().clone());
        let position = |cx: &mut AsyncApp| player.read_with(cx, |player, _| player.position());
        let started = loop {
            match state(cx) {
                PlaybackState::Playing => break true,
                PlaybackState::Failed(error) => {
                    played.error = Some((error.kind(), error.message().to_string()));
                    break false;
                }
                _ if opened.elapsed() > Duration::from_secs(20) => break false,
                _ => {
                    cx.background_executor()
                        .timer(Duration::from_millis(2))
                        .await
                }
            }
        };
        if started {
            played.playing = Some(opened.elapsed());
        }
        if started && until_failed {
            let waiting = Instant::now();
            while waiting.elapsed() < Duration::from_secs(15) {
                if let PlaybackState::Failed(error) = state(cx) {
                    played.error = Some((error.kind(), error.message().to_string()));
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(5))
                    .await;
            }
        }
        if let (true, Some(target)) = (started, seek) {
            let asked = Instant::now();
            player.update(cx, |player, _| player.seek(target));
            while seeked.borrow().is_none() && asked.elapsed() < Duration::from_secs(10) {
                if let PlaybackState::Failed(error) = state(cx) {
                    played.error = Some((error.kind(), error.message().to_string()));
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(2))
                    .await;
            }
            played.seek = seeked.borrow().map(|at| at - asked);
            // The picture and sound go on from there.
            let after = position(cx);
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            played.advanced = position(cx) > after;
            if let PlaybackState::Failed(error) = state(cx) {
                played.error = Some((error.kind(), error.message().to_string()));
            }
        }
        cx.update(|_| drop(player));
        played
    }

    // ---------------------------------------------------------------------------
    // The checks.

    struct Report {
        failures: Vec<String>,
    }

    impl Report {
        fn check(&mut self, name: &str, ok: bool, detail: impl std::fmt::Debug) {
            println!("{} {name}", if ok { "ok  " } else { "FAIL" });
            if !ok {
                println!("     {detail:?}");
                self.failures.push(name.to_string());
            }
        }
    }

    const WITH_TOKEN: &[(&str, &str)] = &[("Authorization", TOKEN)];
    const OTHER_HEADER: &[(&str, &str)] = &[("X-Other", "1")];

    async fn checks(report: &mut Report, cx: &mut AsyncApp) {
        let server = Server::start(HashMap::from([("clip.mp4".to_string(), test_clip())]));
        let second = Some(Duration::from_secs(2));

        let played = play(&server.url("private", "clip.mp4"), WITH_TOKEN, second, cx).await;
        let log = server.take_log();
        report.check(
            "with the header, the clip plays and seeks",
            played.error.is_none()
                && played.playing.is_some()
                && played.seek.is_some()
                && played.advanced,
            &played,
        );
        report.check(
            "every request carried the header and a range",
            !log.is_empty()
                && log
                    .iter()
                    .all(|request| request.authorized && request.range.is_some()),
            &log,
        );

        let played = play(&server.url("private", "clip.mp4"), OTHER_HEADER, None, cx).await;
        server.take_log();
        report.check(
            "without the header: Unauthorized",
            played.error.as_ref().map(|error| error.0) == Some(ErrorKind::Unauthorized),
            &played,
        );

        let played = play(&server.url("private", "nothing.mp4"), WITH_TOKEN, None, cx).await;
        server.take_log();
        report.check(
            "a missing file: NotFound",
            played.error.as_ref().map(|error| error.0) == Some(ErrorKind::NotFound),
            &played,
        );

        let played = play(&server.url("moved", "clip.mp4"), WITH_TOKEN, second, cx).await;
        let log = server.take_log();
        let private: Vec<_> = log
            .iter()
            .filter(|request| request.path.starts_with("/private/"))
            .collect();
        report.check(
            "a same-origin redirect keeps the header",
            played.error.is_none()
                && played.seek.is_some()
                && !private.is_empty()
                && private.iter().all(|request| request.authorized),
            (&played, &log),
        );

        let played = play(&server.url("hop", "clip.mp4"), WITH_TOKEN, second, cx).await;
        let log = server.take_log();
        let open: Vec<_> = log
            .iter()
            .filter(|request| request.path.starts_with("/open/"))
            .collect();
        report.check(
            "a cross-origin redirect drops the header, keeps the range",
            played.error.is_none()
                && played.seek.is_some()
                && !open.is_empty()
                && open
                    .iter()
                    .all(|request| !request.authorized && request.range.is_some()),
            (&played, &log),
        );

        let started = Instant::now();
        let played = play(
            &server.url("drop", "clip.mp4"),
            WITH_TOKEN,
            Some(Duration::from_secs(3)),
            cx,
        )
        .await;
        server.take_log();
        report.check(
            "a connection dropped mid-range does not hang",
            started.elapsed() < Duration::from_secs(25),
            (&played, started.elapsed()),
        );

        let played = play(
            &format!("http://127.0.0.1:{}/list.m3u8", server.port),
            WITH_TOKEN,
            None,
            cx,
        )
        .await;
        server.take_log();
        report.check(
            "a playlist with headers: Unsupported",
            played.error.as_ref().map(|error| error.0) == Some(ErrorKind::Unsupported),
            &played,
        );
    }

    /// The tier A corpus over HTTP: every route with the token, and
    /// `/private/` without it.
    async fn corpus(dir: &Path, report: &mut Report, cx: &mut AsyncApp) {
        let manifest = std::fs::read_to_string(dir.join("../corpus/tier-a.json"))
            .or_else(|_| std::fs::read_to_string("corpus/tier-a.json"))
            .or_else(|_| std::fs::read_to_string("../../corpus/tier-a.json"))
            .expect("corpus/tier-a.json");
        let clips = manifest_clips(&manifest);
        let mut files = HashMap::new();
        for clip in &clips {
            if let Ok(bytes) = std::fs::read(dir.join(&clip.file)) {
                files.insert(clip.file.clone(), Arc::new(bytes));
            }
        }
        let server = Server::start(files);
        println!("\nclip | route | playing ms | seek ms | error");
        for clip in &clips {
            let seek = clip
                .duration
                .filter(|_| clip.video && clip.expect == "plays")
                .map(|seconds| Duration::from_secs_f64(seconds / 2.0));
            for (route, headers) in [
                ("private", WITH_TOKEN),
                ("moved", WITH_TOKEN),
                ("hop", WITH_TOKEN),
                ("private", OTHER_HEADER),
            ] {
                let must_fail = clip.expect != "plays";
                let played =
                    play_until(&server.url(route, &clip.file), headers, seek, must_fail, cx).await;
                let ms = |value: Option<Duration>| {
                    value.map_or("-".to_string(), |value| {
                        format!("{:.0}", value.as_secs_f64() * 1000.0)
                    })
                };
                let token = !std::ptr::eq(headers, OTHER_HEADER);
                println!(
                    "{} | {route}{} | {} | {} | {}",
                    clip.file,
                    if token { "" } else { ", no token" },
                    ms(played.playing),
                    ms(played.seek),
                    played
                        .error
                        .as_ref()
                        .map_or(String::new(), |error| format!("{:?}: {}", error.0, error.1)),
                );
                let ok = if !token {
                    played.error.as_ref().map(|error| error.0) == Some(ErrorKind::Unauthorized)
                } else if clip.expect == "plays" {
                    played.error.is_none()
                        && played.playing.is_some()
                        && (seek.is_none() || played.seek.is_some())
                } else {
                    played.error.is_some()
                };
                if !ok {
                    report
                        .failures
                        .push(format!("{} {route} token={token}", clip.file));
                }
            }
        }
        let log = server.take_log();
        let leaked = log
            .iter()
            .filter(|request| request.path.starts_with("/open/") && request.authorized)
            .count();
        let missing = log
            .iter()
            .filter(|request| request.path.starts_with("/private/") && !request.authorized)
            .count();
        println!(
            "requests {}, /open/ with the header {leaked}, /private/ without it {missing} (the no-token runs)",
            log.len()
        );
        report.check("the header never reached /open/", leaked == 0, leaked);
    }

    struct ManifestClip {
        file: String,
        expect: String,
        video: bool,
        duration: Option<f64>,
    }

    /// `file`, `expect`, `video_codec` and `duration` of each clip, without a
    /// JSON dependency.
    fn manifest_clips(text: &str) -> Vec<ManifestClip> {
        let field = |object: &str, name: &str| -> Option<String> {
            let at = object.find(&format!("\"{name}\""))?;
            let rest = object[at..].split_once(':')?.1.trim_start();
            match rest.strip_prefix('"') {
                Some(quoted) => Some(quoted.split('"').next()?.to_string()),
                None => Some(rest.split([',', '\n', '}']).next()?.trim().to_string()),
            }
        };
        text.split('{')
            .skip(2)
            .filter_map(|object| {
                Some(ManifestClip {
                    file: field(object, "file")?,
                    expect: field(object, "expect").unwrap_or_else(|| "plays".into()),
                    video: field(object, "video_codec").is_some_and(|codec| codec != "null"),
                    duration: field(object, "duration").and_then(|seconds| seconds.parse().ok()),
                })
            })
            .collect()
    }

    pub fn main() {
        gpui_platform::headless().run(move |cx: &mut App| {
            cx.spawn(async move |cx| {
                let mut report = Report {
                    failures: Vec::new(),
                };
                checks(&mut report, cx).await;
                if let Some(dir) = std::env::var_os("GPUI_AV_CLIPS") {
                    corpus(Path::new(&dir), &mut report, cx).await;
                }
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
}
