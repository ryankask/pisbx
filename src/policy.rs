//! The resolved policy and its two renderings: the srt settings document, and
//! the human-readable explanation.
//!
//! Every granted path carries its provenance. That is not decoration: the
//! acceptance test for the whole tool is that `--explain` matches what the
//! kernel enforces, so the explanation is generated from the same struct the
//! settings are.

use serde::Serialize;
use std::path::Path;

/// Pasteboard access is a Mach service lookup. Without it pi cannot copy or
/// paste and reports "Clipboard unavailable".
pub const CLIPBOARD_MACH_SERVICES: &[&str] = &[
    "com.apple.pasteboard.1",
    "com.apple.lsd.mapdb",
    "com.apple.lsd.modifydb",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Builtin,
    Config,
    Env,
    Flag,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Builtin => "built-in",
            Source::Config => "config",
            Source::Env => "env",
            Source::Flag => "flag",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub value: String,
    pub source: Source,
}

impl Entry {
    pub fn new(value: impl Into<String>, source: Source) -> Self {
        Self {
            value: value.into(),
            source,
        }
    }
}

/// Append entries, letting a later, more specific source take attribution for
/// a value an earlier one already granted.
pub fn merge(list: &mut Vec<Entry>, entries: impl IntoIterator<Item = Entry>) {
    for entry in entries {
        match list
            .iter_mut()
            .find(|existing| existing.value == entry.value)
        {
            Some(existing) => existing.source = entry.source,
            None => list.push(entry),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Policy {
    pub allow_write: Vec<Entry>,
    pub deny_write: Vec<Entry>,
    pub deny_read: Vec<Entry>,
    pub allow_read: Vec<Entry>,
    pub domains: Vec<Entry>,
    pub denied_domains: Vec<Entry>,
    pub mach_lookup: Vec<Entry>,
    pub unix_sockets: Vec<Entry>,
    pub allow_git_config: bool,
    /// Raw srt settings merged in add-only: arrays are unioned, objects merge,
    /// scalars never override what pisbx already decided.
    pub extras: serde_json::Value,
    pub read_only: bool,
    pub trust: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub allow_pty: bool,
    pub allow_apple_events: bool,
    pub filesystem: Filesystem,
    pub network: Network,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Filesystem {
    pub deny_read: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allow_read: Vec<String>,
    pub allow_write: Vec<String>,
    pub deny_write: Vec<String>,
    // srt keeps .git/hooks protected either way; this only unblocks the config
    // file, for tools that rewrite remotes or submodules. Read from
    // `filesystem`, not the top level.
    #[serde(skip_serializing_if = "is_false")]
    pub allow_git_config: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Network {
    pub allowed_domains: Vec<String>,
    pub denied_domains: Vec<String>,
    // These two live under `network` in srt's config schema; at the top level
    // they are accepted and silently ignored.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allow_mach_lookup: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allow_unix_sockets: Vec<String>,
}

fn values(entries: &[Entry]) -> Vec<String> {
    entries.iter().map(|entry| entry.value.clone()).collect()
}

/// Merge `extra` into `target` add-only.
///
/// Arrays are unioned and objects recurse, so a passthrough entry can widen
/// what the policy already allows but never remove a deny or turn off a
/// built-in decision such as `allowPty`.
pub fn merge_extra(target: &mut serde_json::Value, extra: &serde_json::Value) {
    match (target, extra) {
        (serde_json::Value::Object(target), serde_json::Value::Object(extra)) => {
            for (key, value) in extra {
                match target.get_mut(key) {
                    Some(existing) => merge_extra(existing, value),
                    None => {
                        target.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (serde_json::Value::Array(target), serde_json::Value::Array(extra)) => {
            for value in extra {
                if !target.contains(value) {
                    target.push(value.clone());
                }
            }
        }
        // Scalars, and any shape mismatch, keep what pisbx decided.
        _ => {}
    }
}

/// Flatten a passthrough value alongside the settings it was merged into, so
/// `--explain` can say whether each entry actually took effect.
fn flatten(
    value: &serde_json::Value,
    merged: &serde_json::Value,
    prefix: &str,
    out: &mut Vec<(String, String, bool)>,
) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                let merged_child = merged.get(key).unwrap_or(&serde_json::Value::Null);
                flatten(child, merged_child, &path, out);
            }
        }
        serde_json::Value::Array(items) => {
            let applied = match merged {
                serde_json::Value::Array(merged_items) => {
                    items.iter().all(|item| merged_items.contains(item))
                }
                _ => false,
            };
            out.push((prefix.to_string(), value.to_string(), applied));
        }
        scalar => out.push((prefix.to_string(), scalar.to_string(), merged == scalar)),
    }
}

impl Policy {
    pub fn settings(&self) -> Settings {
        Settings {
            // pi's TUI calls tcsetattr/setRawMode; Seatbelt denies pty ioctl by
            // default.
            allow_pty: true,
            // Letting sandboxed commands send Apple Events hands them an
            // unsandboxed code-execution path; keep it off.
            allow_apple_events: false,
            filesystem: Filesystem {
                deny_read: values(&self.deny_read),
                allow_read: values(&self.allow_read),
                allow_write: values(&self.allow_write),
                deny_write: values(&self.deny_write),
                allow_git_config: self.allow_git_config,
            },
            network: Network {
                allowed_domains: values(&self.domains),
                denied_domains: values(&self.denied_domains),
                allow_mach_lookup: values(&self.mach_lookup),
                allow_unix_sockets: values(&self.unix_sockets),
            },
        }
    }

    pub fn to_json(&self) -> String {
        let mut value = serde_json::to_value(self.settings())
            .expect("settings contain no values JSON cannot encode");
        merge_extra(&mut value, &self.extras);
        let mut json = serde_json::to_string_pretty(&value)
            .expect("settings contain no values JSON cannot encode");
        json.push('\n');
        json
    }

    /// The human-readable policy: every entry with where it came from.
    pub fn explain(&self, target: &Path, config: Option<&Path>) -> String {
        let mut out = String::new();
        out.push_str(&format!("pisbx policy for {}\n", target.display()));

        let section = |out: &mut String, title: &str, entries: &[Entry], empty: &str| {
            out.push_str(&format!("\n{title}\n"));
            if entries.is_empty() {
                out.push_str(&format!("  {empty}\n"));
                return;
            }
            let width = entries
                .iter()
                .map(|entry| entry.value.len())
                .max()
                .unwrap_or(0);
            for entry in entries {
                out.push_str(&format!(
                    "  {:<width$}  {}\n",
                    entry.value,
                    entry.source.label(),
                    width = width
                ));
            }
        };

        section(&mut out, "writes", &self.allow_write, "none");
        section(
            &mut out,
            "writes denied (wins over the above)",
            &self.deny_write,
            "none",
        );
        section(
            &mut out,
            "reads (home is closed; these are the carve-outs)",
            &self.allow_read,
            "none",
        );
        section(&mut out, "reads denied", &self.deny_read, "none");
        section(&mut out, "network", &self.domains, "none");
        section(&mut out, "network denied", &self.denied_domains, "none");
        section(&mut out, "mach lookups", &self.mach_lookup, "none");
        section(&mut out, "unix sockets", &self.unix_sockets, "none");

        if !self.extras.is_null() {
            let merged: serde_json::Value =
                serde_json::from_str(&self.to_json()).unwrap_or(serde_json::Value::Null);
            let mut flat = Vec::new();
            flatten(&self.extras, &merged, "", &mut flat);
            out.push_str("\nsrt settings passed through (add-only)\n");
            let width = flat
                .iter()
                .map(|(path, _, _)| path.len())
                .max()
                .unwrap_or(0);
            for (path, value, applied) in &flat {
                let note = if *applied {
                    ""
                } else {
                    "  (ignored: pisbx already decided)"
                };
                out.push_str(&format!("  {path:<width$}  {value}{note}\n", width = width));
            }
        }

        out.push_str("\nflags\n");
        out.push_str(&format!(
            "  mode       {}\n",
            if self.read_only {
                "read-only"
            } else {
                "read-write"
            }
        ));
        out.push_str(&format!(
            "  trust      {}\n",
            if self.trust {
                "project resources loaded"
            } else {
                "refused"
            }
        ));
        out.push_str(&format!(
            "  clipboard  {}\n",
            if self.mach_lookup.is_empty() {
                "off"
            } else {
                "on"
            }
        ));
        out.push_str(&format!(
            "  git config {}\n",
            if self.allow_git_config {
                "writable (.git/hooks still denied)"
            } else {
                "read-only"
            }
        ));

        match config {
            Some(path) if path.exists() => out.push_str(&format!("\nconfig  {}\n", path.display())),
            Some(path) => out.push_str(&format!(
                "\nconfig  {} (missing — run `pisbx --init`)\n",
                path.display()
            )),
            None => out.push_str("\nconfig  none (HOME is not set)\n"),
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn entries(values: &[&str], source: Source) -> Vec<Entry> {
        values
            .iter()
            .map(|value| Entry::new(*value, source))
            .collect()
    }

    fn parse(policy: &Policy) -> Value {
        serde_json::from_str(&policy.to_json()).expect("valid JSON")
    }

    #[test]
    fn renders_camel_case_keys() {
        let value = parse(&Policy::default());
        assert_eq!(value["allowPty"], Value::Bool(true));
        assert_eq!(value["allowAppleEvents"], Value::Bool(false));
        assert_eq!(value["filesystem"]["allowWrite"], Value::Array(vec![]));
        assert_eq!(value["network"]["deniedDomains"], Value::Array(vec![]));
    }

    #[test]
    fn optional_switches_are_omitted_when_off() {
        let value = parse(&Policy::default());
        assert!(value["network"].get("allowUnixSockets").is_none());
        assert!(value["filesystem"].get("allowGitConfig").is_none());
    }

    #[test]
    fn optional_switches_appear_when_on() {
        let policy = Policy {
            allow_git_config: true,
            unix_sockets: entries(&["/tmp/agent.sock"], Source::Config),
            mach_lookup: entries(&["com.example.service"], Source::Config),
            ..Policy::default()
        };
        let value = parse(&policy);
        // srt reads these from `filesystem` and `network`; at the top level
        // they parse fine and do nothing.
        assert_eq!(value["filesystem"]["allowGitConfig"], Value::Bool(true));
        assert_eq!(value["network"]["allowUnixSockets"][0], "/tmp/agent.sock");
        assert_eq!(
            value["network"]["allowMachLookup"][0],
            "com.example.service"
        );
        assert!(value.get("allowGitConfig").is_none());
        assert!(value.get("allowUnixSockets").is_none());
    }

    #[test]
    fn empty_optional_arrays_are_omitted() {
        let value = parse(&Policy::default());
        assert!(value["network"].get("allowMachLookup").is_none());
        assert!(value["filesystem"].get("allowRead").is_none());
        assert!(value["filesystem"].get("denyRead").is_some());
        assert!(value["filesystem"].get("denyWrite").is_some());
    }

    #[test]
    fn paths_survive_a_json_round_trip() {
        let awkward = ["/tmp/a\"b", "/tmp/c\\d", "/tmp/e\tf", "/tmp/g\nh"];
        let policy = Policy {
            allow_write: entries(&awkward, Source::Flag),
            ..Policy::default()
        };
        let value = parse(&policy);
        let written: Vec<&str> = value["filesystem"]["allowWrite"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry.as_str().unwrap())
            .collect();
        assert_eq!(written, awkward);
    }

    #[test]
    fn later_sources_take_attribution_without_duplicating() {
        let mut list = entries(&["/a", "/b"], Source::Builtin);
        merge(&mut list, entries(&["/b", "/c"], Source::Flag));
        assert_eq!(
            list,
            vec![
                Entry::new("/a", Source::Builtin),
                Entry::new("/b", Source::Flag),
                Entry::new("/c", Source::Flag),
            ]
        );
    }

    #[test]
    fn explain_lists_provenance_and_modes() {
        let policy = Policy {
            allow_write: entries(&["/target"], Source::Builtin),
            deny_write: entries(&["/target/.git"], Source::Config),
            allow_read: entries(&["/target"], Source::Builtin),
            domains: entries(&["api.example"], Source::Config),
            read_only: true,
            trust: false,
            ..Policy::default()
        };
        let text = policy.explain(Path::new("/target"), Some(Path::new("/missing")));
        assert!(text.contains("/target"), "{text}");
        assert!(text.contains("built-in"), "{text}");
        assert!(text.contains("config"), "{text}");
        assert!(text.contains("mode       read-only"), "{text}");
        assert!(text.contains("git config read-only"), "{text}");
        assert!(text.contains("run `pisbx --init`"), "{text}");
    }

    fn with_extras(json: &str) -> Policy {
        Policy {
            extras: serde_json::from_str(json).unwrap(),
            ..Policy::default()
        }
    }

    #[test]
    fn passthrough_adds_settings_we_do_not_set() {
        let value = parse(&with_extras(r#"{"network":{"allowLocalBinding":true}}"#));
        assert_eq!(value["network"]["allowLocalBinding"], Value::Bool(true));
    }

    #[test]
    fn passthrough_cannot_flip_a_built_in_decision() {
        let policy = Policy {
            allow_write: entries(&["/target"], Source::Builtin),
            deny_write: entries(&["/target/.git"], Source::Builtin),
            ..with_extras(
                r#"{"allowPty":false,"allowAppleEvents":true,
                    "filesystem":{"denyWrite":[]}}"#,
            )
        };
        let value = parse(&policy);
        assert_eq!(value["allowPty"], Value::Bool(true), "pty stays on");
        assert_eq!(
            value["allowAppleEvents"],
            Value::Bool(false),
            "apple events stay off"
        );
        // denyWrite is not emptied: the union keeps what pisbx decided.
        assert_eq!(value["filesystem"]["denyWrite"][0], "/target/.git");
    }

    #[test]
    fn passthrough_unions_arrays_rather_than_replacing() {
        let policy = Policy {
            allow_write: entries(&["/target"], Source::Builtin),
            ..with_extras(r#"{"filesystem":{"allowWrite":["/extra"]}}"#)
        };
        let value = parse(&policy);
        let writes = value["filesystem"]["allowWrite"].as_array().unwrap();
        assert_eq!(writes.len(), 2, "{writes:?}");
        assert!(writes.iter().any(|entry| entry == "/target"));
        assert!(writes.iter().any(|entry| entry == "/extra"));
    }

    #[test]
    fn passthrough_merges_nested_maps() {
        let value = parse(&with_extras(
            r#"{"network":{"deniedDomainReasons":{"github.com:22":"use https"}}}"#,
        ));
        assert_eq!(
            value["network"]["deniedDomainReasons"]["github.com:22"],
            "use https"
        );
    }

    #[test]
    fn explain_lists_passthrough_entries() {
        let text =
            with_extras(r#"{"network":{"allowLocalBinding":true}}"#).explain(Path::new("/t"), None);
        assert!(text.contains("srt settings passed through"), "{text}");
        assert!(text.contains("network.allowLocalBinding"), "{text}");
        assert!(!text.contains("ignored"), "{text}");
    }

    #[test]
    fn explain_flags_passthrough_entries_that_did_not_apply() {
        let text = with_extras(r#"{"allowPty":false,"network":{"allowLocalBinding":true}}"#)
            .explain(Path::new("/t"), None);
        let pty = text
            .lines()
            .find(|line| line.contains("allowPty"))
            .expect("allowPty line");
        assert!(pty.contains("ignored"), "{pty}");
        let local = text
            .lines()
            .find(|line| line.contains("allowLocalBinding"))
            .expect("allowLocalBinding line");
        assert!(!local.contains("ignored"), "{local}");
    }

    #[test]
    fn explain_marks_empty_sections() {
        let text = Policy::default().explain(Path::new("/t"), None);
        assert!(text.contains("network\n  none"), "{text}");
        assert!(text.contains("mach lookups\n  none"), "{text}");
        assert!(text.contains("unix sockets\n  none"), "{text}");
    }

    #[test]
    fn explain_reports_git_config_when_open() {
        let policy = Policy {
            allow_git_config: true,
            ..Policy::default()
        };
        let text = policy.explain(Path::new("/t"), None);
        assert!(text.contains("git config writable"), "{text}");
    }
}
