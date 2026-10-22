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
    pub read_only: bool,
    pub trust: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub allow_pty: bool,
    pub allow_apple_events: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allow_mach_lookup: Vec<String>,
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
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Network {
    pub allowed_domains: Vec<String>,
    pub denied_domains: Vec<String>,
}

fn values(entries: &[Entry]) -> Vec<String> {
    entries.iter().map(|entry| entry.value.clone()).collect()
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
            allow_mach_lookup: values(&self.mach_lookup),
            filesystem: Filesystem {
                deny_read: values(&self.deny_read),
                allow_read: values(&self.allow_read),
                allow_write: values(&self.allow_write),
                deny_write: values(&self.deny_write),
            },
            network: Network {
                allowed_domains: values(&self.domains),
                denied_domains: values(&self.denied_domains),
            },
        }
    }

    pub fn to_json(&self) -> String {
        let mut json = serde_json::to_string_pretty(&self.settings())
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
    fn empty_optional_arrays_are_omitted() {
        let value = parse(&Policy::default());
        assert!(value.get("allowMachLookup").is_none());
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
        assert!(text.contains("run `pisbx --init`"), "{text}");
    }

    #[test]
    fn explain_marks_empty_sections() {
        let text = Policy::default().explain(Path::new("/t"), None);
        assert!(text.contains("network\n  none"), "{text}");
        assert!(text.contains("mach lookups\n  none"), "{text}");
    }
}
