//! `@` references on the start page: typing `@` lists the files of the
//! session's folder, narrowed as you type, and picking one puts its path in
//! the prompt (`@src/main.rs`). Coding agents such as Claude Code read the
//! file a reference names.
//!
//! This module only deals with text and files; the start page draws the
//! list and handles the keys.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// At most this many files are listed, so a huge folder (a home folder, a
/// monorepo) stays quick to search.
const MAX_FILES: usize = 20_000;
/// How deep a folder outside git is searched.
const MAX_DEPTH: usize = 8;
/// Folders not worth searching outside git (git already skips what the
/// project ignores).
const SKIPPED: [&str; 6] = ["node_modules", "target", "build", "dist", "Library", "__pycache__"];

/// The files under `dir`, as paths relative to it. Uses git's list of the
/// project's files when `dir` is in a repository (which leaves out what
/// `.gitignore` excludes), and otherwise walks the folder, skipping hidden
/// and generated folders. Can take a moment: call it off the main thread.
pub fn list_files(dir: &Path) -> Vec<String> {
    if let Some(files) = git_files(dir) {
        return files;
    }
    let mut files = Vec::new();
    walk(dir, dir, 0, &mut files);
    files.sort();
    files
}

fn git_files(dir: &Path) -> Option<Vec<String>> {
    crate::workspace::project_root(dir)?;
    let output = Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard", "-z"])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut files: Vec<String> = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .take(MAX_FILES)
        .collect();
    files.sort();
    files.dedup();
    Some(files)
}

fn walk(root: &Path, dir: &Path, depth: usize, files: &mut Vec<String>) {
    if depth > MAX_DEPTH || files.len() >= MAX_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        // Not following links keeps the walk finite.
        let Ok(kind) = entry.file_type() else { continue };
        let path = entry.path();
        if kind.is_dir() {
            if !SKIPPED.contains(&name.as_ref()) {
                walk(root, &path, depth + 1, files);
            }
        } else if kind.is_file()
            && let Ok(relative) = path.strip_prefix(root)
        {
            files.push(relative.to_string_lossy().into_owned());
            if files.len() >= MAX_FILES {
                return;
            }
        }
    }
}

/// The reference being typed at the end of `text`: what follows an `@`
/// that starts a word. `None` when the last word is not one.
pub fn query(text: &str) -> Option<&str> {
    let start = text.rfind(char::is_whitespace).map_or(0, |at| at + text[at..].chars().next().map_or(1, char::len_utf8));
    text[start..].strip_prefix('@')
}

/// The files that best match `query`, best first, at most `limit`. With
/// nothing typed yet, the shallowest files come first.
pub fn suggestions<'a>(files: &'a [String], query: &str, limit: usize) -> Vec<&'a str> {
    if query.is_empty() {
        let mut shallow: Vec<&str> = files.iter().map(String::as_str).collect();
        shallow.sort_by_key(|file| (file.matches('/').count(), file.len()));
        shallow.truncate(limit);
        return shallow;
    }
    let mut scored: Vec<(i32, &str)> = files
        .iter()
        .filter_map(|file| {
            // Matching the file's own name counts for more than matching
            // somewhere in its folders.
            let name = file.rsplit('/').next().unwrap_or(file);
            let in_name = crate::actions::fuzzy_score(query, name).map(|score| score + 50);
            let in_path = crate::actions::fuzzy_score(query, file);
            Some((in_name.max(in_path)?, file.as_str()))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.len().cmp(&b.1.len())).then(a.1.cmp(b.1)));
    scored.into_iter().take(limit).map(|(_, file)| file).collect()
}

/// `text` with the reference being typed replaced by `@path` and a space,
/// ready for more typing.
pub fn accept(text: &str, path: &str) -> String {
    let start = text.rfind(char::is_whitespace).map_or(0, |at| at + text[at..].chars().next().map_or(1, char::len_utf8));
    format!("{}{} ", &text[..start], reference(path))
}

/// How a path is written as a reference: `@path`, quoted when it has
/// spaces so it stays one reference.
pub fn reference(path: &str) -> String {
    if path.contains(char::is_whitespace) { format!("@\"{path}\"") } else { format!("@{path}") }
}

/// A chosen file as a reference for a session running in `dir`: relative
/// to it when inside, absolute otherwise.
pub fn reference_to(file: &Path, dir: &Path) -> String {
    let shown: PathBuf = file.strip_prefix(dir).map_or_else(|_| file.to_path_buf(), Path::to_path_buf);
    reference(&shown.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> Vec<String> {
        ["README.md", "crates/app/src/main.rs", "crates/app/src/start.rs", "crates/render/src/lib.rs", "docs/start here.md"]
            .map(String::from)
            .to_vec()
    }

    #[test]
    fn a_reference_is_an_at_sign_starting_the_last_word() {
        assert_eq!(query("look at @src/ma"), Some("src/ma"));
        assert_eq!(query("@"), Some(""));
        assert_eq!(query("two lines\n@st"), Some("st"));
        assert_eq!(query("mail me at a@b.c"), None);
        assert_eq!(query("@done "), None);
        assert_eq!(query(""), None);
        assert_eq!(query("café @ré"), Some("ré"));
    }

    #[test]
    fn suggestions_rank_the_file_name_first() {
        let files = files();
        assert_eq!(suggestions(&files, "start.r", 5)[0], "crates/app/src/start.rs");
        // Both files named "start…" come before any path that only
        // contains the letters somewhere.
        let top: Vec<&str> = suggestions(&files, "start", 2);
        assert!(top.contains(&"crates/app/src/start.rs") && top.contains(&"docs/start here.md"));
        assert_eq!(suggestions(&files, "lib", 5), ["crates/render/src/lib.rs"]);
        assert!(suggestions(&files, "zzz", 5).is_empty());
        // Nothing typed: the top-level files first.
        assert_eq!(suggestions(&files, "", 2), ["README.md", "docs/start here.md"]);
    }

    #[test]
    fn accepting_replaces_only_the_reference() {
        assert_eq!(accept("fix @sta", "crates/app/src/start.rs"), "fix @crates/app/src/start.rs ");
        assert_eq!(accept("@", "README.md"), "@README.md ");
        assert_eq!(accept("see\n@do", "docs/start here.md"), "see\n@\"docs/start here.md\" ");
    }

    #[test]
    fn chosen_files_are_relative_to_the_session_folder() {
        let dir = Path::new("/repo");
        assert_eq!(reference_to(Path::new("/repo/src/main.rs"), dir), "@src/main.rs");
        assert_eq!(reference_to(Path::new("/tmp/log.txt"), dir), "@/tmp/log.txt");
    }

    #[test]
    fn folders_outside_git_are_walked_without_hidden_or_generated_folders() {
        let dir = std::env::temp_dir().join(format!("ale-term-mention-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for path in ["a.txt", "src/b.rs", ".hidden/c", "node_modules/d.js", "src/.e"] {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        assert_eq!(list_files(&dir), ["a.txt", "src/b.rs"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn repositories_list_their_files_through_git() {
        let dir = std::env::temp_dir().join(format!("ale-term-mention-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("out")).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git").args(args).current_dir(&dir).output().unwrap();
        };
        git(&["init", "-q"]);
        std::fs::write(dir.join(".gitignore"), "out/\n").unwrap();
        std::fs::write(dir.join("kept.rs"), "").unwrap();
        std::fs::write(dir.join("out/ignored.o"), "").unwrap();
        assert_eq!(list_files(&dir), [".gitignore", "kept.rs"]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
