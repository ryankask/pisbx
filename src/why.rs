//! `pisbx why` — answer a question in the sandbox's own vocabulary.
//!
//! `--explain` prints the policy document; this answers "may this process read
//! or write *this path* now", naming the entry that decided it and where that
//! entry came from. The static answer is a reading of the policy; `--probe`
//! asks the kernel instead, and the two are reported separately because only
//! one of them is enforcement.

use crate::paths;
use crate::policy::{Entry, Policy};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Read,
    Write,
}

impl Op {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "read" | "r" => Some(Op::Read),
            "write" | "w" => Some(Op::Write),
            _ => None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Op::Read => "read",
            Op::Write => "write",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Allowed(Option<Entry>),
    Denied(Option<Entry>),
}

impl Verdict {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Verdict::Allowed(_))
    }
}

fn has_glob(entry: &str) -> bool {
    paths::has_glob(entry)
}

/// Component-wise containment: `/a/b` covers `/a/b` and `/a/b/c`, never `/a/bc`.
fn covers(entry: &str, path: &str) -> bool {
    if has_glob(entry) {
        return glob_match(entry, path);
    }
    path == entry
        || (path.starts_with(entry) && path[entry.len()..].starts_with('/'))
        || (entry == "/" && path.starts_with('/'))
}

/// Glob match with `*` (within a path segment), `**` (across segments) and `?`.
fn glob_match(pattern: &str, text: &str) -> bool {
    fn go(pattern: &[char], text: &[char]) -> bool {
        let Some(next) = pattern.first() else {
            return text.is_empty();
        };
        match next {
            '*' if pattern.get(1) == Some(&'*') => {
                let rest = &pattern[2..];
                // `**/x` should also match `x`, so an immediately following
                // separator is optional at the start.
                let rest = match rest.first() {
                    Some('/') => &rest[1..],
                    _ => rest,
                };
                (0..=text.len()).any(|skip| go(rest, &text[skip..]))
            }
            '*' => {
                let rest = &pattern[1..];
                for skip in 0..=text.len() {
                    if go(rest, &text[skip..]) {
                        return true;
                    }
                    match text.get(skip) {
                        Some('/') | None => return false,
                        _ => {}
                    }
                }
                false
            }
            '?' => !text.is_empty() && text[0] != '/' && go(&pattern[1..], &text[1..]),
            literal => !text.is_empty() && text[0] == *literal && go(&pattern[1..], &text[1..]),
        }
    }

    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    go(&pattern, &text)
}

/// Best (most specific) entry that covers `path`.
fn best<'a>(entries: &'a [Entry], path: &str) -> Option<&'a Entry> {
    entries
        .iter()
        .filter(|entry| covers(&entry.value, path))
        .max_by_key(|entry| entry.value.len())
}

/// Decide one access against the resolved policy. Returns the path as it was
/// resolved, plus the verdict.
pub fn evaluate(policy: &Policy, query: &str, op: Op, target: &Path) -> (String, Verdict) {
    // Prefer the resolved spelling: it is what the kernel compares.
    let spellings = paths::expand(query, target);
    let path = spellings
        .last()
        .cloned()
        .unwrap_or_else(|| query.to_string());

    let verdict = match op {
        Op::Write => match best(&policy.deny_write, &path) {
            // A deny always wins over an allow for the same path.
            Some(deny) => Verdict::Denied(Some(deny.clone())),
            None => match best(&policy.allow_write, &path) {
                Some(allow) => Verdict::Allowed(Some(allow.clone())),
                None => Verdict::Denied(None),
            },
        },
        Op::Read => {
            let allow = best(&policy.allow_read, &path).cloned();
            let deny = best(&policy.deny_read, &path);

            // srt: an allow re-opens a deny, unless the deny is at least as
            // specific as the allow.
            match (allow, deny) {
                (Some(allow), Some(deny)) if deny.value.len() >= allow.value.len() => {
                    Verdict::Denied(Some(deny.clone()))
                }
                (Some(allow), _) => Verdict::Allowed(Some(allow)),
                (None, Some(deny)) => Verdict::Denied(Some(deny.clone())),
                (None, None) => Verdict::Denied(None),
            }
        }
    };

    (path, verdict)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Source;

    fn entry(value: &str, source: Source) -> Entry {
        Entry::new(value, source)
    }

    #[test]
    fn containment_is_component_wise() {
        assert!(covers("/a/b", "/a/b"));
        assert!(covers("/a/b", "/a/b/c"));
        assert!(!covers("/a/b", "/a/bc"));
        assert!(covers("/", "/anything"));
    }

    #[test]
    fn globs_match_within_and_across_segments() {
        assert!(glob_match("/a/*.md", "/a/x.md"));
        assert!(!glob_match("/a/*.md", "/a/b/x.md"));
        assert!(glob_match("/a/**/*.md", "/a/b/c/x.md"));
        assert!(glob_match("/a/**/x.md", "/a/x.md"));
        assert!(glob_match("**/mcp.json", "/Users/x/.pi/agent/mcp.json"));
        assert!(glob_match("/a/?", "/a/b"));
        assert!(!glob_match("/a/?", "/a/bc"));
    }

    #[test]
    fn deny_beats_allow_for_writes() {
        let policy = Policy {
            allow_write: vec![entry("/target", Source::Builtin)],
            deny_write: vec![entry("/target/.git", Source::Builtin)],
            ..Policy::default()
        };
        let (_, inside) = evaluate(&policy, "/target/file", Op::Write, Path::new("/"));
        assert!(inside.is_allowed());

        let (_, denied) = evaluate(&policy, "/target/.git/config", Op::Write, Path::new("/"));
        assert_eq!(
            denied,
            Verdict::Denied(Some(entry("/target/.git", Source::Builtin)))
        );
    }

    #[test]
    fn a_write_needs_a_grant() {
        let policy = Policy::default();
        let (path, verdict) = evaluate(&policy, "/etc/passwd", Op::Write, Path::new("/"));
        assert!(path.ends_with("/etc/passwd"), "{path}");
        assert_eq!(verdict, Verdict::Denied(None));
    }

    #[test]
    fn a_more_specific_read_deny_wins() {
        let policy = Policy {
            deny_read: vec![entry("/Users", Source::Builtin)],
            allow_read: vec![entry("/Users/ryan", Source::Builtin)],
            ..Policy::default()
        };
        let (_, allowed) = evaluate(&policy, "/Users/ryan/notes", Op::Read, Path::new("/"));
        assert!(allowed.is_allowed(), "{allowed:?}");
    }
}
