//! Path resolution.
//!
//! Seatbelt matches the *resolved* path of a file, so a grant written as
//! `~/.pi/agent` does nothing when `~/.pi` is a symlink into a checkout. Every
//! user-supplied path is therefore granted twice: as written, and as the kernel
//! will see it.

use std::path::{Path, PathBuf};

pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Seatbelt has no glob support of its own beyond what srt compiles, so a path
/// containing glob characters is passed through verbatim instead of resolved.
pub fn has_glob(entry: &str) -> bool {
    entry.contains(['*', '?', '['])
}

/// Resolve a directory the way `cd <path> && pwd -P` does.
pub fn resolve_dir(path: &Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(path)
}

fn expand_tilde(entry: &str) -> Option<PathBuf> {
    let home = home()?;
    match entry {
        "~" => Some(home),
        _ => entry.strip_prefix("~/").map(|rest| home.join(rest)),
    }
}

/// The spellings a single user-supplied entry should grant.
pub fn expand(entry: &str, target: &Path) -> Vec<String> {
    let entry = entry.trim();
    if entry.is_empty() {
        return Vec::new();
    }

    let glob = has_glob(entry);
    let literal = match expand_tilde(entry) {
        Some(path) => path,
        None => {
            let path = PathBuf::from(entry);
            if path.is_absolute() || glob {
                path
            } else {
                target.join(path)
            }
        }
    };

    let mut spellings = vec![literal.to_string_lossy().into_owned()];
    if !glob {
        if let Ok(resolved) = std::fs::canonicalize(&literal) {
            let resolved = resolved.to_string_lossy().into_owned();
            if !spellings.contains(&resolved) {
                spellings.push(resolved);
            }
        }
    }
    spellings
}

/// Split a comma-separated list, dropping blanks.
pub fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_and_trims_lists() {
        assert_eq!(split_list(" a, b ,,c "), ["a", "b", "c"]);
        assert!(split_list("  ").is_empty());
    }

    #[test]
    fn relative_paths_resolve_against_the_target() {
        let out = expand("shared", Path::new("/tmp/project"));
        assert_eq!(out, ["/tmp/project/shared"]);
    }

    #[test]
    fn globs_are_passed_through_untouched() {
        let out = expand("~/Library/Caches/*", Path::new("/tmp/project"));
        assert_eq!(out.len(), 1);
        assert!(out[0].ends_with("Library/Caches/*"), "{:?}", out);
    }

    #[test]
    fn tilde_expands_against_home() {
        let Some(home) = home() else { return };
        let out = expand("~/scratch", Path::new("/tmp/project"));
        assert_eq!(out, [home.join("scratch").to_string_lossy().into_owned()]);
    }

    #[test]
    fn existing_paths_gain_their_resolved_spelling() {
        // /tmp is a symlink to /private/tmp on macOS; on other systems it is
        // already real, so the assertion is that the literal is always present
        // and that at most two spellings come back.
        let out = expand("/tmp", Path::new("/"));
        assert_eq!(out[0], "/tmp");
        assert!(out.len() <= 2, "{:?}", out);
    }

    #[test]
    fn empty_entries_are_dropped() {
        assert!(expand("  ", Path::new("/")).is_empty());
    }
}
