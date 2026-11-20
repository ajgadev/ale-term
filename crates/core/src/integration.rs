//! Installs the shell integration script and works out how to load it.
//!
//! Only zsh is supported for now. zsh has no "also run this file" option, so
//! we use the standard trick: point `ZDOTDIR` at a folder of ours whose
//! `.zshenv` restores the real `ZDOTDIR`, loads the user's files and then
//! adds our hooks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

const ZSHENV: &str = include_str!("integration/zshenv.zsh");

/// Writes the script under `data_dir` (if it is missing or out of date) and
/// returns the folder to use as `ZDOTDIR`.
pub fn install(data_dir: &Path) -> std::io::Result<PathBuf> {
    let dir = data_dir.join("shell-integration").join("zsh");
    let file = dir.join(".zshenv");
    if std::fs::read_to_string(&file).ok().as_deref() != Some(ZSHENV) {
        std::fs::create_dir_all(&dir)?;
        std::fs::write(&file, ZSHENV)?;
    }
    Ok(dir)
}

/// Adds the environment variables that make `shell` load the integration.
/// Does nothing for shells we do not support.
pub fn apply(shell: &str, zdotdir: &Path, env: &mut HashMap<String, String>) {
    let name = Path::new(shell).file_name().and_then(|name| name.to_str()).unwrap_or("");
    if name.trim_start_matches('-') != "zsh" {
        return;
    }
    if let Ok(original) = std::env::var("ZDOTDIR") {
        env.insert("ALE_TERM_ORIG_ZDOTDIR".into(), original);
    }
    env.insert("ZDOTDIR".into(), zdotdir.to_string_lossy().into_owned());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_zsh_gets_the_integration() {
        let mut env = HashMap::new();
        apply("/bin/bash", Path::new("/x"), &mut env);
        assert!(env.is_empty());
        apply("/bin/zsh", Path::new("/x"), &mut env);
        assert_eq!(env.get("ZDOTDIR").map(String::as_str), Some("/x"));
    }
}
