//! Remuxes real files. Ignored by default: set `GPUI_AV_REMUX_CLIPS` to a
//! folder of `.mkv` and `.webm` files (scripts/make-tier-b.sh makes some)
//! and run `cargo test -p gpui-av-remux -- --ignored`. Each file's output
//! is written beside it as `<name>.remux/` (an HLS playlist, its init and
//! media segments, and all of them joined as `whole.mp4`) for a player or
//! ffprobe to check.

use std::fs;
use std::io::BufReader;
use std::path::PathBuf;

use std::io::{Read, Write};
use std::net::TcpStream;

use gpui_av_remux::{HlsServer, Remuxer};

/// A plain HTTP/1.0 GET, returning the status line and the body.
fn get(url: &str) -> (String, Vec<u8>) {
    let rest = url.strip_prefix("http://").expect("an http URL");
    let (host, path) = rest.split_at(rest.find('/').expect("a path"));
    let mut stream = TcpStream::connect(host).expect("connect");
    write!(stream, "GET {path} HTTP/1.0\r\n\r\n").expect("send");
    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read");
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("headers end");
    let head = String::from_utf8_lossy(&response[..split]).to_string();
    let status = head.lines().next().unwrap_or_default().to_string();
    (status, response[split + 4..].to_vec())
}

#[test]
#[ignore = "needs GPUI_AV_REMUX_CLIPS"]
fn remuxes_clips() {
    let Some(folder) = std::env::var_os("GPUI_AV_REMUX_CLIPS").map(PathBuf::from) else {
        panic!("set GPUI_AV_REMUX_CLIPS to a folder of .mkv and .webm files");
    };
    let mut clips: Vec<PathBuf> = fs::read_dir(&folder)
        .expect("read the folder")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "mkv" || extension == "webm")
        })
        .collect();
    clips.sort();
    assert!(!clips.is_empty(), "no .mkv or .webm files in {folder:?}");
    for clip in clips {
        let file = fs::File::open(&clip).expect("open the clip");
        let mut remuxer = match Remuxer::open(BufReader::new(file)) {
            Ok(remuxer) => remuxer,
            Err(error) => panic!("{clip:?}: {error}"),
        };
        let out = clip.with_extension("remux");
        fs::create_dir_all(&out).expect("make the output folder");
        let init = remuxer.init_segment();
        let mut whole = init.clone();
        fs::write(out.join("init.mp4"), &init).expect("write init");
        let segments = remuxer.segments();
        for index in 0..segments.len() {
            let segment = remuxer
                .media_segment(index)
                .unwrap_or_else(|error| panic!("{clip:?} segment {index}: {error}"));
            fs::write(out.join(format!("{index}.m4s")), &segment).expect("write segment");
            whole.extend_from_slice(&segment);
        }
        fs::write(out.join("whole.mp4"), &whole).expect("write whole");
        let playlist = remuxer.hls_playlist("init.mp4", |index| format!("{index}.m4s"));
        fs::write(out.join("index.m3u8"), playlist).expect("write playlist");
        let server = HlsServer::start(remuxer_for(&clip)).expect("start the server");
        let (status, served) = get(server.url());
        assert!(status.contains("200"), "{status}");
        assert_eq!(served, playlist_text(&out));
        let (status, served) = get(&server.url().replace("index.m3u8", "init.mp4"));
        assert!(status.contains("200"), "{status}");
        assert_eq!(served, init);
        let last = segments.len() - 1;
        let (status, served) = get(&server.url().replace("index.m3u8", &format!("{last}.m4s")));
        assert!(status.contains("200"), "{status}");
        assert_eq!(
            served,
            fs::read(out.join(format!("{last}.m4s"))).expect("read segment")
        );
        let (status, _) = get(&server
            .url()
            .replace("index.m3u8", &format!("{}.m4s", last + 1)));
        assert!(status.contains("404"), "{status}");
        let (status, _) = get(&format!(
            "http://{}/index.m3u8",
            server.url()[7..].split('/').next().unwrap_or_default()
        ));
        assert!(status.contains("404"), "{status}");
        drop(server);
        println!(
            "{}: {} {:?}, {} segments, {}, skipped {:?}",
            clip.display(),
            remuxer.doc_type(),
            remuxer.duration(),
            segments.len(),
            remuxer.mime_type(),
            remuxer.skipped(),
        );
    }
}

fn remuxer_for(clip: &std::path::Path) -> Remuxer<BufReader<fs::File>> {
    Remuxer::open(BufReader::new(fs::File::open(clip).expect("open the clip"))).expect("remux")
}

fn playlist_text(out: &std::path::Path) -> Vec<u8> {
    fs::read(out.join("index.m3u8")).expect("read the playlist")
}
