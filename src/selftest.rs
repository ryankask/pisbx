//! Assertions about the policy in force right now.
//!
//! A generated profile that looks right and enforces nothing is the failure
//! mode that matters, so `--selftest` runs real commands through srt and checks
//! what actually happened rather than what was requested.

use crate::srt;
use std::path::Path;

pub struct Check {
    pub label: &'static str,
    pub expected: &'static str,
    pub command: String,
}

/// Quote for `sh -c`, so a path with spaces or quotes still probes the right
/// directory.
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

pub fn checks(read_only: bool, extra_write: Option<&str>) -> Vec<Check> {
    let mut checks = vec![
        Check {
            label: "read workspace allowed",
            expected: "READ_OK",
            command: "ls . >/dev/null && echo READ_OK".to_string(),
        },
        Check {
            label: "read $HOME blocked",
            expected: "not permitted",
            command: "ls ~ 2>&1".to_string(),
        },
        Check {
            label: "write in $HOME blocked",
            expected: "not permitted",
            command: "echo x > ~/.pisbx-probe 2>&1".to_string(),
        },
        Check {
            label: "write to pi extensions blocked",
            expected: "not permitted",
            command: "echo x > ~/.pi/agent/extensions/.pisbx-probe 2>&1".to_string(),
        },
        Check {
            label: "write to pi install blocked",
            expected: "not permitted",
            command: "echo x > ~/.pi/agent/install/.pisbx-probe 2>&1".to_string(),
        },
        Check {
            label: "write to pi state allowed",
            expected: "WRITE_OK",
            command: "mkdir -p ~/.pi/agent/sessions && echo x > ~/.pi/agent/sessions/.pisbx-probe && echo WRITE_OK && rm -f ~/.pi/agent/sessions/.pisbx-probe".to_string(),
        },
        Check {
            label: "curl outside allowlist blocked",
            expected: "000",
            command: "curl -s -m 8 -o /dev/null -w \"%{http_code}\" https://example.com".to_string(),
        },
    ];

    if read_only {
        checks.insert(
            0,
            Check {
                label: "write in workspace blocked (read-only)",
                expected: "not permitted",
                command: "echo x > .pisbx-probe 2>&1".to_string(),
            },
        );
    } else {
        checks.insert(
            0,
            Check {
                label: "write in workspace allowed",
                expected: "WRITE_OK",
                command: "echo x > .pisbx-probe && echo WRITE_OK && rm -f .pisbx-probe".to_string(),
            },
        );
    }

    if let Some(dir) = extra_write {
        let dir = quote(dir);
        checks.push(Check {
            label: "write in extra directory allowed",
            expected: "WRITE_OK",
            command: format!(
                "echo x > {dir}/.pisbx-probe && echo WRITE_OK && rm -f {dir}/.pisbx-probe"
            ),
        });
    }

    checks
}

/// Run every check, printing one line each. Returns true when all pass.
pub fn run(settings: &Path, cwd: &Path, read_only: bool, extra_write: Option<&str>) -> bool {
    let mut failed = 0;

    for check in checks(read_only, extra_write) {
        let output = match srt::capture(settings, cwd, &check.command) {
            Ok(output) => output,
            Err(error) => {
                println!("FAIL  {}  (could not run srt: {error})", check.label);
                failed += 1;
                continue;
            }
        };

        if output.contains(check.expected) {
            println!("PASS  {}", check.label);
        } else {
            let last = output.lines().last().unwrap_or("").trim();
            println!("FAIL  {}  (got: {last})", check.label);
            failed += 1;
        }
    }

    failed == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_the_new_defaults() {
        let labels: Vec<_> = checks(false, None)
            .iter()
            .map(|check| check.label)
            .collect();
        assert_eq!(
            labels,
            [
                "write in workspace allowed",
                "read workspace allowed",
                "read $HOME blocked",
                "write in $HOME blocked",
                "write to pi extensions blocked",
                "write to pi install blocked",
                "write to pi state allowed",
                "curl outside allowlist blocked",
            ]
        );
    }

    #[test]
    fn read_only_inverts_the_workspace_assertion() {
        let checks = checks(true, None);
        assert_eq!(checks[0].label, "write in workspace blocked (read-only)");
        assert_eq!(checks[0].expected, "not permitted");
    }

    #[test]
    fn extra_directory_adds_a_check() {
        let checks = checks(false, Some("/tmp/keep"));
        assert_eq!(checks.len(), 9);
        assert!(
            checks[8].command.contains("'/tmp/keep'"),
            "{:?}",
            checks[8].command
        );
    }

    #[test]
    fn quotes_paths_for_the_shell() {
        let checks = checks(false, Some("/tmp/it's here"));
        assert!(
            checks[8].command.contains(r"'/tmp/it'\''s here'"),
            "{:?}",
            checks[8].command
        );
    }
}
