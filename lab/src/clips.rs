//! The lab's clip list: what the command line named, plus built-in test
//! patterns.

use std::path::Path;

use gpui_av::Source;

/// One entry in the clip list.
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    /// The name shown in the list.
    pub label: String,
    /// What to play.
    pub source: Source,
}

/// The file extensions a folder is searched for.
const MEDIA_EXTENSIONS: [&str; 8] = ["mp4", "m4v", "mov", "webm", "mkv", "m4a", "mp3", "aac"];

/// Patterns for the test-pattern backend. Other backends refuse them, which
/// is itself worth seeing.
pub fn built_in() -> Vec<Clip> {
    [
        (
            "Pattern 720p60, 10 s",
            "pattern://1280x720?fps=60&duration=10",
        ),
        (
            "Pattern 1080p30, 20 s",
            "pattern://1920x1080?fps=30&duration=20",
        ),
        (
            "Pattern portrait 720x1280",
            "pattern://720x1280?fps=60&duration=10",
        ),
    ]
    .into_iter()
    .map(|(label, url)| Clip {
        label: label.to_string(),
        source: Source::url(url),
    })
    .collect()
}

/// Turns command-line targets into clips. A URL or a file is one clip; a
/// folder is every media file directly inside it, in name order. `headers`
/// go on every URL, and only on URLs.
pub fn from_targets(targets: &[String], headers: &[(String, String)]) -> Result<Vec<Clip>, String> {
    let mut clips = Vec::new();
    for target in targets {
        if is_url(target) {
            let mut source = Source::url(target);
            for (name, value) in headers {
                source = source.header(name, value);
            }
            clips.push(Clip {
                label: url_label(target),
                source,
            });
            continue;
        }
        let path = Path::new(target);
        if path.is_dir() {
            let entries = std::fs::read_dir(path).map_err(|error| format!("{target}: {error}"))?;
            let mut files: Vec<_> = entries
                .filter_map(|entry| Some(entry.ok()?.path()))
                .filter(|file| file.is_file() && is_media(file))
                .collect();
            files.sort();
            clips.extend(files.iter().map(|file| file_clip(file)));
        } else {
            // A file that does not exist stays in the list: playing it
            // shows how the backend reports the failure.
            clips.push(file_clip(path));
        }
    }
    Ok(clips)
}

fn is_url(target: &str) -> bool {
    ["http://", "https://", "pattern://"]
        .iter()
        .any(|scheme| target.starts_with(scheme))
}

fn is_media(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            MEDIA_EXTENSIONS
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

fn file_clip(path: &Path) -> Clip {
    let label = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    Clip {
        label,
        source: Source::file(path),
    }
}

/// The last path segment of a URL without its query, or the whole URL when
/// it has no path.
fn url_label(url: &str) -> String {
    let without_query = url.split(['?', '#']).next().unwrap_or(url);
    without_query
        .split_once("://")
        .and_then(|(_, rest)| rest.split_once('/'))
        .and_then(|(_, path)| path.rsplit('/').find(|segment| !segment.is_empty()))
        .unwrap_or(url)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_becomes_one_clip_with_the_headers() {
        let clips = from_targets(
            &["https://example.test/media/clip.mp4?token=1".to_string()],
            &[("Authorization".to_string(), "Bearer t".to_string())],
        )
        .unwrap();
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].label, "clip.mp4");
        assert_eq!(
            clips[0].source,
            Source::url("https://example.test/media/clip.mp4?token=1")
                .header("Authorization", "Bearer t")
        );
    }

    #[test]
    fn a_url_without_a_path_is_its_own_label() {
        assert_eq!(url_label("https://example.test/"), "https://example.test/");
        assert_eq!(url_label("https://example.test"), "https://example.test");
    }

    #[test]
    fn a_file_gets_no_headers_and_need_not_exist() {
        let clips = from_targets(
            &["/nowhere/missing.mp4".to_string()],
            &[("Authorization".to_string(), "Bearer t".to_string())],
        )
        .unwrap();
        assert_eq!(clips[0].label, "missing.mp4");
        assert_eq!(clips[0].source, Source::file("/nowhere/missing.mp4"));
    }

    #[test]
    fn a_folder_lists_its_media_files_in_name_order() {
        let folder = std::env::temp_dir().join(format!("gpui-av-lab-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        for name in ["b.MOV", "a.mp4", "notes.txt"] {
            std::fs::write(folder.join(name), b"").unwrap();
        }
        let clips = from_targets(&[folder.display().to_string()], &[]).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();

        let labels: Vec<_> = clips.iter().map(|clip| clip.label.as_str()).collect();
        assert_eq!(labels, ["a.mp4", "b.MOV"]);
    }
}
