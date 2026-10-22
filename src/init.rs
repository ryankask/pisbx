//! The `--init` scaffold.
//!
//! Home is closed by default, so the first run has nothing to read with. This
//! is the one-time, visible act of turning the machine's own configuration —
//! PATH, `$DOTFILES`, caches, the model provider — into a policy file the user
//! owns from then on. Nothing here runs at launch time.

use crate::paths;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// A path or host to grant, with the reason it was included.
pub type Item = (String, String);

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Draft {
    pub allow_read: Vec<Item>,
    pub domains: Vec<Item>,
    pub provider: Option<String>,
    pub domains_inferred: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Written(PathBuf),
    Exists(PathBuf),
}

/// Provider hosts that are not discoverable from `models-store.json`, because
/// OAuth flows reach hosts the model list never mentions.
fn provider_extras(provider: &str) -> &'static [&'static str] {
    match provider {
        "openai-codex" => &["chatgpt.com", "auth.openai.com"],
        "openai" => &["api.openai.com"],
        "anthropic" => &["api.anthropic.com"],
        "google" | "gemini" => &["generativelanguage.googleapis.com"],
        "openrouter" => &["openrouter.ai"],
        _ => &[],
    }
}

fn host_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = rest.split('/').next()?.trim();
    if host.is_empty() || host.contains(' ') {
        None
    } else {
        Some(host.to_string())
    }
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn provider() -> Option<String> {
    let settings = read_json(&paths::home()?.join(".pi/agent/settings.json"))?;
    settings
        .get("defaultProvider")
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

/// The provider's base URL, looked up under the provider name or its first
/// dash-separated segment (`openai-codex` → `openai`).
fn provider_base_url(provider: &str) -> Option<(String, String)> {
    let store = read_json(&paths::home()?.join(".pi/agent/models-store.json"))?;
    let mut keys = vec![provider];
    if let Some(stripped) = provider.split('-').next() {
        if stripped != provider {
            keys.push(stripped);
        }
    }
    for key in keys {
        let Some(entry) = store.get(key) else {
            continue;
        };
        let Some(url) = entry
            .get("models")
            .and_then(|models| models.get(0))
            .and_then(|model| model.get("baseUrl"))
            .and_then(|url| url.as_str())
        else {
            continue;
        };
        if let Some(host) = host_of(url) {
            return Some((host, format!("models-store: {key}")));
        }
    }
    None
}

pub fn draft() -> Draft {
    let mut draft = Draft::default();
    let target = paths::home().unwrap_or_else(|| PathBuf::from("/"));

    // Grants the floor already makes are not repeated in the file.
    let floor = crate::floor::reads_around(&target);

    let mut read_candidates: Vec<Item> = Vec::new();
    if let Some(dotfiles) = std::env::var_os("DOTFILES") {
        let dotfiles = PathBuf::from(dotfiles);
        read_candidates.push((
            dotfiles.to_string_lossy().into_owned(),
            "$DOTFILES".to_string(),
        ));
    }
    for (relative, reason) in [
        (".cache", "package caches"),
        (".npm", "package caches"),
        (".cargo/registry", "package caches"),
        (".config/git", "git config"),
        (".gitconfig", "git config"),
    ] {
        if let Some(home) = paths::home() {
            let path = home.join(relative);
            if path.exists() {
                read_candidates.push((format!("~/{relative}"), reason.to_string()));
            }
        }
    }
    for entry in crate::floor::home_path_entries() {
        read_candidates.push((entry, "toolchains on PATH".to_string()));
    }

    for (value, reason) in read_candidates {
        let resolved = paths::expand(&value, &target);
        let Some(value) = resolved.first() else {
            continue;
        };
        if covered_by(value, &floor)
            || draft
                .allow_read
                .iter()
                .any(|(existing, _)| existing == value)
        {
            continue;
        }
        draft.allow_read.push((value.clone(), reason));
    }

    let provider = provider();
    if let Some(name) = &provider {
        if let Some((host, reason)) = provider_base_url(name) {
            draft.domains.push((host, reason));
        }
        for host in provider_extras(name) {
            let reason = format!("provider {name}");
            if !draft.domains.iter().any(|(existing, _)| existing == host) {
                draft.domains.push(((*host).to_string(), reason));
            }
        }
        draft.domains_inferred = !draft.domains.is_empty();
    }
    draft.provider = provider;

    draft
}

/// Rewrite a home-rooted path as `~/…`, so the file is readable and portable.
/// Whether `path` is already granted by `entries` (or one of their parents).
fn covered_by(path: &str, entries: &[String]) -> bool {
    entries
        .iter()
        .any(|entry| path == entry || path.starts_with(&format!("{entry}/")))
}

fn tilde(path: &str) -> String {
    let Some(home) = paths::home() else {
        return path.to_string();
    };
    let home = home.to_string_lossy();
    match path.strip_prefix(home.as_ref()) {
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// Drop entries already covered by an ancestor: Seatbelt grants subpaths, so
/// listing `~/x` makes `~/x/bin` redundant.
fn collapse(paths: &[String]) -> Vec<String> {
    let mut sorted: Vec<&String> = paths.iter().collect();
    sorted.sort_by(|a, b| a.len().cmp(&b.len()).then(a.cmp(b)));

    let mut kept: Vec<String> = Vec::new();
    for path in sorted {
        let covered = kept
            .iter()
            .any(|ancestor| path == ancestor || path.starts_with(&format!("{ancestor}/")));
        if !covered {
            kept.push(path.clone());
        }
    }
    kept.sort();
    kept
}

/// Group entries under the reason they were included, keeping the order the
/// reasons first appeared. Inline alignment was the wrong tool here: a single
/// long PATH entry pushed every comment to the far right.
fn grouped(items: &[Item], collapse_paths: bool) -> Vec<(String, Vec<String>)> {
    let kept: std::collections::HashSet<String> = if collapse_paths {
        let paths: Vec<String> = items.iter().map(|item| item.0.clone()).collect();
        collapse(&paths).into_iter().collect()
    } else {
        items.iter().map(|item| item.0.clone()).collect()
    };

    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for (value, reason) in items {
        if !kept.contains(value) {
            continue;
        }
        let path = if collapse_paths {
            tilde(value)
        } else {
            value.clone()
        };
        match groups.iter_mut().find(|group| &group.0 == reason) {
            Some(group) => group.1.push(path),
            None => groups.push((reason.clone(), vec![path])),
        }
    }

    for group in &mut groups {
        group.1.sort();
        group.1.dedup();
    }
    groups
}

pub fn render(draft: &Draft) -> String {
    let mut out = String::new();
    out.push_str("# pisbx policy, generated by `pisbx --init`.\n");
    out.push_str("#\n");
    out.push_str("# Home is closed by default: the workspace, pi's state directory, pi's\n");
    out.push_str("# runtime, the paths below, and nothing else. Every other path under $HOME\n");
    out.push_str("# is denied. Flags add to this for a single run.\n");
    out.push_str("#\n");
    out.push_str("# Seatbelt matches resolved symlinks, so each entry also applies under its\n");
    out.push_str(
        "# real path. Paths the built-in floor already grants (pi's state, its runtime,\n",
    );
    out.push_str("# the directories on $PATH) are not repeated here, and neither are paths\n");
    out.push_str("# covered by a parent. `pisbx --explain` shows the whole picture.\n");

    out.push_str("\nallow-read = [\n");
    for (index, (reason, entries)) in grouped(&draft.allow_read, true).iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(&format!("  # {reason}\n"));
        for entry in entries {
            out.push_str(&format!("  {},\n", quote(entry)));
        }
    }
    out.push_str("]\n");

    out.push_str("\n# Writable paths beyond the workspace. The workspace, pi's state directory\n");
    out.push_str("# and temp space are always writable; `--read-only` drops even those.\n");
    out.push_str("# allow-write = [\"~/scratch\"]\n");

    out.push_str("\n# Model provider hosts. Nothing else is reachable: a fetch to any other\n");
    out.push_str("# host fails. Wildcards are written `*.example.com`.\n");
    if draft.domains.is_empty() {
        out.push_str("# domains = []   # no provider detected: add the host your model uses\n");
    } else {
        out.push_str("domains = [\n");
        for (index, (reason, entries)) in grouped(&draft.domains, false).iter().enumerate() {
            if index > 0 {
                out.push('\n');
            }
            out.push_str(&format!("  # {reason}\n"));
            for entry in entries {
                out.push_str(&format!("  {},\n", quote(entry)));
            }
        }
        out.push_str("]\n");
    }

    out.push_str(
        "\n# Clipboard: the pasteboard is a Mach service, denied by default. Reading it\n",
    );
    out.push_str("# means the agent sees whatever you copied, so this ships commented out.\n");
    out.push_str("# mach-lookup = [\"com.apple.pasteboard.1\", \"com.apple.lsd.mapdb\", \"com.apple.lsd.modifydb\"]\n");

    out
}

fn quote(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn write(path: &Path, force: bool) -> Result<Outcome> {
    if path.exists() && !force {
        return Ok(Outcome::Exists(path.to_path_buf()));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    std::fs::write(path, render(&draft()))
        .with_context(|| format!("could not write {}", path.display()))?;
    Ok(Outcome::Written(path.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft_with(reads: &[&str], domains: &[&str]) -> Draft {
        Draft {
            allow_read: reads
                .iter()
                .map(|value| ((*value).to_string(), "test".to_string()))
                .collect(),
            domains: domains
                .iter()
                .map(|value| ((*value).to_string(), "test".to_string()))
                .collect(),
            provider: Some("openai-codex".to_string()),
            domains_inferred: true,
        }
    }

    fn parsed(text: &str) -> crate::config::Config {
        toml::from_str(text).expect("valid TOML")
    }

    #[test]
    fn renders_toml_that_parses_back() {
        let draft = draft_with(
            &["~/Projects/dotfiles", "~/nonexistent"],
            &["api.openai.com"],
        );
        let config = parsed(&render(&draft));

        let mut expected = vec!["~/Projects/dotfiles", "~/nonexistent"];
        expected.sort_unstable();
        let mut actual = config.allow_read;
        actual.sort();
        assert_eq!(actual, expected);
        assert_eq!(config.domains, ["api.openai.com"]);
        // Clipboard ships commented out.
        assert!(config.mach_lookup.is_empty());
    }

    #[test]
    fn reasons_get_their_own_line() {
        let text = render(&draft_with(
            &["~/.cache", "~/Projects/dotfiles"],
            &["api.example"],
        ));
        assert!(text.contains("  # test\n"), "{text}");
        // No inline comment column, which is what made long paths ugly.
        assert!(!text.contains(",  #"), "{text}");
    }

    #[test]
    fn entries_are_grouped_by_reason() {
        let draft = Draft {
            allow_read: vec![
                ("~/a".to_string(), "first".to_string()),
                ("~/b".to_string(), "second".to_string()),
                ("~/c".to_string(), "first".to_string()),
            ],
            ..Draft::default()
        };
        let text = render(&draft);
        let first = text.find("# first").expect("first group");
        let second = text.find("# second").expect("second group");
        assert!(first < second, "{text}");
        // `~/c` belongs to the first group, so it appears before the second header.
        assert!(text[first..second].contains("~/c"), "{text}");
    }

    #[test]
    fn floor_covered_paths_are_skipped() {
        let floor = vec!["/Users/x/.pi".to_string()];
        assert!(covered_by("/Users/x/.pi/agent/bin", &floor));
        assert!(!covered_by("/Users/x/.cache", &floor));
        assert!(!covered_by("/Users/x/.pix", &floor));
    }

    #[test]
    fn nested_paths_are_collapsed() {
        let draft = draft_with(&["/tmp/parent", "/tmp/parent/child"], &[]);
        let text = render(&draft);
        assert!(text.contains("/tmp/parent\""), "{text}");
        assert!(!text.contains("/tmp/parent/child"), "{text}");
    }

    #[test]
    fn home_paths_are_shortened() {
        let Some(home) = paths::home() else { return };
        let path = home.join("scratch").to_string_lossy().into_owned();
        let text = render(&draft_with(&[path.as_str()], &[]));
        assert!(text.contains("\"~/scratch\""), "{text}");
    }

    #[test]
    fn no_domains_renders_a_hint() {
        let text = render(&draft_with(&["~/x"], &[]));
        assert!(text.contains("# domains = []"), "{text}");
    }

    #[test]
    fn host_extraction_strips_scheme_and_path() {
        assert_eq!(
            host_of("https://api.openai.com/v1").as_deref(),
            Some("api.openai.com")
        );
        assert_eq!(
            host_of("http://localhost:8080/x").as_deref(),
            Some("localhost:8080")
        );
        assert_eq!(host_of("ftp://x"), None);
    }

    #[test]
    fn provider_extras_are_known_for_codex() {
        assert!(provider_extras("openai-codex").contains(&"chatgpt.com"));
        assert!(provider_extras("nonexistent").is_empty());
    }

    #[test]
    fn existing_file_is_not_clobbered() {
        let dir = std::env::temp_dir().join(format!("pisbx-init-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "domains = []\n").unwrap();

        assert_eq!(write(&path, false).unwrap(), Outcome::Exists(path.clone()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "domains = []\n");

        assert!(matches!(write(&path, true).unwrap(), Outcome::Written(_)));
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("allow-read"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
