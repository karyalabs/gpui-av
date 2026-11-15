//! Serves a Matroska or WebM file as HLS on 127.0.0.1 and prints the
//! playlist URL, until stdin closes. For trying players by hand:
//! `cargo run -p gpui-av-remux --example serve -- film.mkv`, then open the
//! URL in QuickTime Player, Safari or `gst-launch-1.0 playbin uri=...`.

use std::io::{BufReader, Read};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: serve <file.mkv|file.webm>")?;
    let file = std::fs::File::open(&path)?;
    let remuxer = gpui_av_remux::Remuxer::open(BufReader::new(file))?;
    eprintln!(
        "{} segments, {}, offset {:?}",
        remuxer.segments().len(),
        remuxer.mime_type(),
        remuxer.presentation_offset()
    );
    let server = gpui_av_remux::HlsServer::start(remuxer)?;
    println!("{}", server.url());
    // Serve until stdin closes.
    std::io::stdin().read_to_end(&mut Vec::new())?;
    Ok(())
}
