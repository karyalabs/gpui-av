//! Remuxes real files. Ignored by default: set `GPUI_AV_REMUX_CLIPS` to a
//! folder of `.mkv` and `.webm` files (scripts/make-tier-b.sh makes some)
//! and run `cargo test -p gpui-av-remux -- --ignored`. Each file's output
//! is written beside it as `<name>.remux/` (an HLS playlist, its init and
//! media segments, and all of them joined as `whole.mp4`) for a player or
//! ffprobe to check.

use std::fs;
use std::io::BufReader;
use std::path::PathBuf;

use gpui_av_remux::Remuxer;

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
