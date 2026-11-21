//! Pictures and the terminal: saving a copied picture so it can be pasted
//! as a file, and finding the picture behind an `[Image #3]` mention so it
//! can be shown while the mouse is over it.
//!
//! Agents running in a terminal cannot receive a picture directly. What
//! they accept is the path of an image file; Claude Code, for one, turns a
//! pasted path into an `[Image #N]` attachment and keeps its own copy in
//! its temporary folder, which is where the preview reads it from.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::layout::Rect;

/// Renderer image key of the preview (see `icons::key` for why it cannot
/// collide with a pane's images).
pub const KEY: u64 = 0xA1E1_0000;

/// Files larger than this are not previewed.
const MAX_FILE_BYTES: u64 = 48 * 1024 * 1024;

/// Saved pictures older than this are removed at launch.
const KEEP_PASTES: Duration = Duration::from_secs(24 * 60 * 60);

/// Whether `path` names a picture we can show.
pub fn is_image(path: &Path) -> bool {
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "gif")
}

/// Finds an `[Image #N]` mention covering position `at` in a line.
/// Returns where it starts and ends (exclusive) and its number.
pub fn image_tag_at(chars: &[char], at: usize) -> Option<(usize, usize, u32)> {
    const OPEN: &str = "[Image #";
    let open: Vec<char> = OPEN.chars().collect();
    // The mention is short, so its start is close to the left of `at`.
    let earliest = at.saturating_sub(open.len() + 10);
    for start in (earliest..=at).rev() {
        if !chars[start..].starts_with(&open) {
            continue;
        }
        let digits = chars[start + open.len()..].iter().take_while(|c| c.is_ascii_digit()).count();
        let end = start + open.len() + digits;
        if digits == 0 || digits > 6 || chars.get(end) != Some(&']') || at > end {
            return None;
        }
        let number: String = chars[start + open.len()..end].iter().collect();
        return Some((start, end + 1, number.parse().ok()?));
    }
    None
}

/// The folder where Claude Code keeps what belongs to running sessions.
pub fn claude_temp_dir() -> PathBuf {
    let base = std::env::var_os("CLAUDE_CODE_TMPDIR").map(PathBuf::from).unwrap_or_else(|| "/tmp".into());
    // SAFETY: getuid has no preconditions and cannot fail.
    base.join(format!("claude-{}", unsafe { libc::getuid() }))
}

/// The file behind `[Image #number]` in a Claude Code session.
///
/// Claude Code stores pasted pictures as
/// `<temp>/<project>/<session id>/images/<number>.png`. With the session's
/// id (known when status hooks are installed) the file is found exactly.
/// Without it, the most recently used session of the project in `cwd`
/// that has such a picture is taken, which is a guess.
pub fn claude_image(temp: &Path, session_id: Option<&str>, cwd: Option<&Path>, number: u32) -> Option<PathBuf> {
    let file = |session: &Path| -> Option<PathBuf> {
        let images = session.join("images");
        ["png", "jpg", "jpeg", "gif"]
            .iter()
            .map(|extension| images.join(format!("{number}.{extension}")))
            .find(|path| path.is_file())
    };
    let projects = || std::fs::read_dir(temp).into_iter().flatten().flatten().map(|entry| entry.path());
    if let Some(id) = session_id {
        // An id is only ever a file name, never a path.
        if id.is_empty() || id.contains(['/', '\\']) || id.starts_with('.') {
            return None;
        }
        return projects().find_map(|project| file(&project.join(id)));
    }
    // Claude Code names the project folder after the working folder, with
    // everything but letters and digits replaced by dashes.
    let name: String = cwd?.to_str()?.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let sessions = std::fs::read_dir(temp.join(name)).ok()?;
    sessions
        .flatten()
        .filter_map(|entry| file(&entry.path()))
        .max_by_key(|path| path.metadata().and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH))
}

/// Reads a picture and shrinks it to fit `max_width` x `max_height`.
/// Returns its width, height and RGBA pixels.
pub fn load(path: &Path, max_width: u32, max_height: u32) -> Option<(u32, u32, Vec<u8>)> {
    if std::fs::metadata(path).ok()?.len() > MAX_FILE_BYTES {
        return None;
    }
    let mut reader = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    // Refuse pictures whose pixels would not fit in a sensible amount of
    // memory, whatever the file's size.
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    let picture = reader.decode().ok()?;
    let picture = if picture.width() > max_width || picture.height() > max_height {
        picture.thumbnail(max_width.max(1), max_height.max(1))
    } else {
        picture
    };
    let rgba = picture.into_rgba8();
    Some((rgba.width(), rgba.height(), rgba.into_raw()))
}

/// Where to show a `width` x `height` preview for a pointer at (x, y):
/// below and to the right when there is room, otherwise wherever it fits.
pub fn place(x: f32, y: f32, width: f32, height: f32, gap: f32, window: (f32, f32)) -> Rect {
    let left = if x + gap + width <= window.0 { x + gap } else { x - gap - width };
    let top = if y + gap + height <= window.1 { y + gap } else { y - gap - height };
    let clamp = |value: f32, size: f32, limit: f32| value.min(limit - size).max(0.0).round();
    Rect::new(clamp(left, width, window.0), clamp(top, height, window.1), width, height)
}

/// The private folder where copied pictures are saved for pasting.
pub fn paste_dir() -> PathBuf {
    // SAFETY: getuid has no preconditions and cannot fail.
    std::env::temp_dir().join(format!("ale-term-{}", unsafe { libc::getuid() })).join("pastes")
}

/// Saves RGBA pixels as a PNG in `dir`, readable only by the user, and
/// returns the file's path.
pub fn save_png(dir: &Path, width: u32, height: u32, rgba: &[u8]) -> Option<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).ok()?;
    let picture = image::RgbaImage::from_raw(width, height, rgba.to_vec())?;
    let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).ok()?.as_millis();
    let path = dir.join(format!("pasted-{stamp}.png"));
    let file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).ok()?;
    let mut writer = std::io::BufWriter::new(file);
    match picture.write_to(&mut writer, image::ImageFormat::Png) {
        Ok(()) => Some(path),
        Err(_) => {
            let _ = std::fs::remove_file(&path);
            None
        }
    }
}

/// Removes saved pictures left behind by earlier runs.
pub fn remove_old_pastes(dir: &Path) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > KEEP_PASTES);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Writes file paths the way a shell (and an agent's prompt) reads them:
/// separated by spaces, with awkward characters escaped.
pub fn paths_as_text(paths: &[PathBuf]) -> String {
    let escape = |path: &PathBuf| -> String {
        let mut out = String::new();
        for c in path.to_string_lossy().chars() {
            if !(c.is_alphanumeric() || "/._-+,:@%=~".contains(c)) {
                out.push('\\');
            }
            out.push(c);
        }
        out
    };
    paths.iter().map(escape).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ale-term-preview-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn finds_image_mentions_under_the_pointer() {
        let line: Vec<char> = "> look at [Image #5] and [Image #12], ok".chars().collect();
        let text: String = line.iter().collect();
        let first = text.find("[Image #5]").unwrap();
        for at in first..first + 10 {
            assert_eq!(image_tag_at(&line, at), Some((first, first + 10, 5)), "at {at}");
        }
        let second = text.find("[Image #12]").unwrap();
        assert_eq!(image_tag_at(&line, second + 10), Some((second, second + 11, 12)));
        // Not on a mention.
        assert_eq!(image_tag_at(&line, 3), None);
        assert_eq!(image_tag_at(&line, first + 10), None);
        assert_eq!(image_tag_at(&line, line.len() - 1), None);
        // Look-alikes.
        for other in ["[Image #]", "[Image #x]", "[Image #5", "[Image 5]", "[Image #1234567]"] {
            let chars: Vec<char> = other.chars().collect();
            assert_eq!(image_tag_at(&chars, 2), None, "{other}");
        }
    }

    #[test]
    fn finds_the_file_behind_a_mention() {
        let temp = scratch("claude");
        let images = temp.join("-Users-me-my-app").join("session-1").join("images");
        std::fs::create_dir_all(&images).unwrap();
        std::fs::write(images.join("2.png"), b"x").unwrap();
        // Exactly, by session id.
        assert_eq!(claude_image(&temp, Some("session-1"), None, 2), Some(images.join("2.png")));
        assert_eq!(claude_image(&temp, Some("session-1"), None, 3), None);
        assert_eq!(claude_image(&temp, Some("other"), None, 2), None);
        // An id is never allowed to walk elsewhere.
        assert_eq!(claude_image(&temp, Some("../-Users-me-my-app/session-1"), None, 2), None);
        // By project folder when the id is unknown.
        assert_eq!(claude_image(&temp, None, Some(Path::new("/Users/me/my_app")), 2), Some(images.join("2.png")));
        assert_eq!(claude_image(&temp, None, Some(Path::new("/Users/me/elsewhere")), 2), None);
        assert_eq!(claude_image(&temp, None, None, 2), None);
        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn saved_pictures_are_private_and_load_back_scaled() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("pastes").join("pastes");
        // 40 x 20, left half red, right half blue.
        let mut pixels = Vec::new();
        for _ in 0..20 {
            for x in 0..40 {
                pixels.extend_from_slice(if x < 20 { &[255, 0, 0, 255] } else { &[0, 0, 255, 255] });
            }
        }
        let path = save_png(&dir, 40, 20, &pixels).unwrap();
        assert!(is_image(&path));
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        // Wrong pixel count is refused.
        assert_eq!(save_png(&dir, 40, 20, &pixels[4..]), None);

        // Small enough: unchanged.
        let (w, h, data) = load(&path, 100, 100).unwrap();
        assert_eq!((w, h), (40, 20));
        assert_eq!(data, pixels);
        // Too big: shrunk, keeping its shape and its colors.
        let (w, h, data) = load(&path, 10, 10).unwrap();
        assert_eq!((w, h), (10, 5));
        assert_eq!(&data[..4], &[255, 0, 0, 255]);
        assert_eq!(&data[data.len() - 4..], &[0, 0, 255, 255]);
        // Not pictures.
        std::fs::write(dir.join("notes.png"), b"not a picture").unwrap();
        assert_eq!(load(&dir.join("notes.png"), 10, 10), None);
        assert_eq!(load(&dir.join("missing.png"), 10, 10), None);

        // Fresh files survive the cleanup.
        remove_old_pastes(&dir);
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn previews_stay_inside_the_window() {
        let window = (1000.0, 800.0);
        assert_eq!(place(100.0, 100.0, 300.0, 200.0, 10.0, window), Rect::new(110.0, 110.0, 300.0, 200.0));
        // Near the right and bottom edges it flips to the other side.
        assert_eq!(place(900.0, 700.0, 300.0, 200.0, 10.0, window), Rect::new(590.0, 490.0, 300.0, 200.0));
        // Bigger than the window: pinned to the corner.
        let big = place(500.0, 400.0, 1200.0, 900.0, 10.0, window);
        assert_eq!((big.x, big.y), (0.0, 0.0));
    }

    #[test]
    fn paths_are_written_for_a_shell() {
        let paths = [PathBuf::from("/tmp/a b/shot (1).png"), PathBuf::from("/tmp/ok-1.png")];
        assert_eq!(paths_as_text(&paths), r"/tmp/a\ b/shot\ \(1\).png /tmp/ok-1.png");
        assert!(!is_image(Path::new("/tmp/notes.txt")));
        assert!(is_image(Path::new("/tmp/Photo.JPG")));
    }
}
