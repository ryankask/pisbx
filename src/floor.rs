//! The built-in floor: what pi needs before any policy question is asked.
//!
//! Home is closed by default, so the floor has to name everything pi reaches
//! for on its own: the workspace, its state directory, its runtime, and the
//! toolchain directories on PATH. These are not choices; they are the cost of
//! running pi at all. Everything else arrives through config or flags.

use crate::paths;
use std::path::{Path, PathBuf};

/// Writable regardless: the workspace, pi's state, and scratch space.
pub const WRITE_ROOTS: &[&str] = &["~/.pi/agent", "/private/tmp", "/private/var/folders"];

/// Never writable, whatever else grants them. `extensions/` is code pi loads at
/// startup and `install/` is the runtime it executes; either one writable means
/// a sandboxed agent can arm the next session.
pub const ALWAYS_DENY_WRITE: &[&str] = &[
    // Code pi loads and runs in this or the next session. `bin/` holds the
    // launcher and the fd/rg binaries pi puts on PATH; `git/` holds
    // git-installed packages.
    "~/.pi/agent/bin",
    "~/.pi/agent/extensions",
    "~/.pi/agent/git",
    "~/.pi/agent/install",
    "~/.pi/agent/mcp.json",
    "~/.pi/agent/skills",
    // Prompt material: a write here is durable prompt injection.
    "~/.pi/agent/SYSTEM.md",
    "~/.pi/agent/APPEND_SYSTEM.md",
    "~/.pi/agent/AGENTS.md",
    "~/.pi/agent/AGENTS.override.md",
    "~/.pi/agent/CLAUDE.md",
    "~/.pi/agent/prompts",
    // Configuration that steers pi or its traffic.
    "~/.pi/agent/settings.json",
    "~/.pi/agent/keybindings.json",
    "~/.pi/agent/models.json",
    "~/.pi/agent/models-store.json",
    "~/.pi/agent/npm",
    "~/.pi/agent/themes",
    "~/.pi/agent/trust.json",
    "~/.pi/agent/web-search-cache",
    "~/.pi/agent/web-search.json",
    // srt merges these convenience writes into every profile; deny wins.
    "~/.npm/_logs",
    "~/.claude/debug",
];

/// Never writable inside the workspace, whatever else grants it. Relative to
/// the target directory: project `.pi` configuration and context files are read
/// as project-level overrides, so a write is a next-session code path or
/// durable prompt injection. Edit them on the host, not through the agent.
pub const ALWAYS_DENY_WRITE_PROJECT: &[&str] = &[
    // Project settings, MCP servers, system prompts, extensions, skills,
    // prompts and themes. A bare `.pi` directory is inert to pi, but a
    // writable one lets the agent create the rest.
    ".pi",
    // Project skills loaded from `.agents/skills` in the working directory.
    ".agents",
    // Context files: discovery does not require project trust, and a write
    // here persists instructions into every later session in this tree.
    "AGENTS.md",
    "AGENTS.MD",
    "AGENTS.override.md",
    "CLAUDE.md",
    "CLAUDE.MD",
];

/// Executables whose install prefix pi needs readable.
const RUNTIME_BINARIES: &[&str] = &["node", "pi"];

/// The spellings to grant for a path: as written, and resolved, because
/// Seatbelt matches what the kernel sees.
pub fn spellings(entry: &str, target: &Path) -> Vec<String> {
    paths::expand(entry, target)
}

/// PATH entries that live under the home directory. This is the user's own
/// statement of which toolchains matter, expressed in another file.
pub fn home_path_entries() -> Vec<String> {
    let (Some(home), Some(path)) = (paths::home(), std::env::var_os("PATH")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for dir in std::env::split_paths(&path) {
        if !dir.is_absolute() || !dir.starts_with(&home) || !dir.is_dir() {
            continue;
        }
        out.extend(spellings(&dir.to_string_lossy(), &home));
    }
    out
}

/// Install prefix of an executable: the directory above its `bin`, or its
/// parent when it is not in a `bin` directory.
fn install_prefix(binary: &Path) -> Option<PathBuf> {
    let resolved = std::fs::canonicalize(binary).ok()?;
    let parent = resolved.parent()?;
    match parent.file_name()?.to_str() {
        Some("bin") => parent.parent().map(Path::to_path_buf),
        _ => Some(parent.to_path_buf()),
    }
}

/// Read paths pi cannot start without, including the workspace.
pub fn reads(target: &Path) -> Vec<String> {
    let mut out = spellings(&target.to_string_lossy(), target);
    out.extend(reads_around(target));
    out
}

/// The floor minus the workspace. `--init` uses this to avoid repeating grants
/// the tool already makes on every run.
pub fn reads_around(target: &Path) -> Vec<String> {
    let mut out = Vec::new();

    // pi's own tree: install, extensions, sessions, auth.
    out.extend(spellings("~/.pi", target));

    // pi's bundled node, when the installer provided one.
    if let Some(home) = paths::home() {
        let pi_node = home.join(".local/share/pi-node");
        if pi_node.is_dir() {
            out.extend(spellings(&pi_node.to_string_lossy(), target));
        }
    }

    // Toolchain directories on PATH, including any runtime prefixes they
    // resolve into (mise, nvm, volta, asdf all hide the real tree behind a
    // shim directory).
    out.extend(home_path_entries());
    for binary in RUNTIME_BINARIES {
        let Some(found) = crate::srt::locate(binary) else {
            continue;
        };
        if let Some(prefix) = install_prefix(&found) {
            out.extend(spellings(&prefix.to_string_lossy(), target));
        }
        out.extend(spellings(&found.to_string_lossy(), target));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn always_denied_covers_the_escalation_paths() {
        for entry in [
            "~/.pi/agent/bin",
            "~/.pi/agent/extensions",
            "~/.pi/agent/git",
            "~/.pi/agent/install",
            "~/.pi/agent/mcp.json",
            "~/.pi/agent/skills",
            "~/.pi/agent/SYSTEM.md",
            "~/.pi/agent/settings.json",
            "~/.pi/agent/themes",
            "~/.pi/agent/trust.json",
            "~/.pi/agent/web-search.json",
        ] {
            assert!(ALWAYS_DENY_WRITE.contains(&entry), "{entry}");
        }
    }

    #[test]
    fn project_config_is_always_denied() {
        for entry in [".pi", ".agents", "AGENTS.md", "CLAUDE.md"] {
            assert!(ALWAYS_DENY_WRITE_PROJECT.contains(&entry), "{entry}");
        }
    }

    #[test]
    fn floor_reads_include_the_workspace_and_state() {
        let reads = reads(Path::new("/tmp/project"));
        assert!(
            reads.iter().any(|entry| entry == "/tmp/project"),
            "{reads:?}"
        );
        assert!(
            reads.iter().any(|entry| entry.ends_with(".pi")
                || entry.ends_with("/pi")
                || entry.contains("/.pi")),
            "{reads:?}"
        );
    }

    #[test]
    fn install_prefix_uses_the_bin_parent() {
        let Some(node) = crate::srt::locate("node") else {
            return;
        };
        let prefix = install_prefix(&node).expect("node has a prefix");
        assert!(prefix.is_dir(), "{prefix:?}");
        assert!(!prefix.ends_with("bin"), "{prefix:?}");
    }

    #[test]
    fn home_path_entries_are_under_home_and_exist() {
        let Some(home) = paths::home() else { return };
        for entry in home_path_entries() {
            let path = PathBuf::from(&entry);
            assert!(path.starts_with(&home), "{entry}");
            assert!(path.is_dir(), "{entry}");
        }
    }
}
