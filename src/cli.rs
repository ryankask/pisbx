//! Command-line parsing.
//!
//! pisbx consumes its own options and hands everything from the first
//! non-option argument (or after `--`) to pi untouched. clap cannot express
//! that rule — it would reject `pisbx -p "hello"` as an unknown flag — so the
//! argv is split first and only the head goes to clap.

use clap::Parser;
use std::ffi::OsString;
use std::path::PathBuf;

const VALUE_FLAGS: &[&str] = &[
    "--dir",
    "--allow-write",
    "--allow-read",
    "--deny-read",
    "--deny-write",
    "--domains",
    "--allow-unix-sockets",
];

const BOOL_FLAGS: &[&str] = &[
    "--clipboard",
    "--no-clipboard",
    "--allow-git-config",
    "--read-only",
    "--init",
    "--force",
    "--explain",
    "--trust",
    "--keep-temp",
    "--print-config",
    "--selftest",
    "--help",
    "-h",
];

#[derive(Parser, Debug, Default)]
#[command(
    name = "pisbx",
    about = "Run pi inside a macOS Seatbelt sandbox via srt",
    disable_version_flag = true,
    after_help = "Options are consumed only up to the first non-option argument or `--`;\n\
                  everything after that is passed to pi unchanged.\n\
                  \n\
                  Paths may be absolute, start with ~, or be relative to the target.\n\
                  Lists are comma separated, and flags may be repeated.\n\
                  \n\
                  Environment (fallbacks for aliases; flags win):\n\
                  \x20 PISBX_DIR, PISBX_ALLOW_WRITE, PISBX_ALLOW_READ, PISBX_DENY_READ,\n\
                  \x20 PISBX_DOMAINS, PISBX_CLIPBOARD, PISBX_TRUST, PISBX_KEEP_TEMP, PISBX_CONFIG"
)]
pub struct Cli {
    /// Directory to sandbox (default: current directory)
    #[arg(long, value_name = "PATH")]
    pub dir: Option<PathBuf>,

    /// Additional writable directories
    #[arg(long = "allow-write", value_name = "PATHS", value_delimiter = ',')]
    pub allow_write: Vec<String>,

    /// Re-open paths inside a deny-read region
    #[arg(long = "allow-read", value_name = "PATHS", value_delimiter = ',')]
    pub allow_read: Vec<String>,

    /// Additional unreadable paths
    #[arg(long = "deny-read", value_name = "PATHS", value_delimiter = ',')]
    pub deny_read: Vec<String>,

    /// Paths to keep read-only (wins over allow-write)
    #[arg(long = "deny-write", value_name = "PATHS", value_delimiter = ',')]
    pub deny_write: Vec<String>,

    /// Replace the policy file's domain list for this run
    #[arg(long, value_name = "LIST", value_delimiter = ',')]
    pub domains: Vec<String>,

    /// Allow unix socket connections at these paths
    #[arg(
        long = "allow-unix-sockets",
        value_name = "PATHS",
        value_delimiter = ','
    )]
    pub allow_unix_sockets: Vec<String>,

    /// Allow writes to .git/config (hooks stay blocked)
    #[arg(long = "allow-git-config")]
    pub allow_git_config: bool,

    /// Allow the pasteboard
    #[arg(long)]
    pub clipboard: bool,

    /// Deny pasteboard access (the default)
    #[arg(long = "no-clipboard")]
    pub no_clipboard: bool,

    /// Drop the workspace write and every extra write grant, for this run
    #[arg(long = "read-only")]
    pub read_only: bool,

    /// Write a starter policy to the config file, then exit
    #[arg(long)]
    pub init: bool,

    /// Replace an existing config when used with --init
    #[arg(long)]
    pub force: bool,

    /// Print the effective policy with the origin of every entry
    #[arg(long)]
    pub explain: bool,

    /// Load project .pi resources (default: refused)
    #[arg(long)]
    pub trust: bool,

    /// Keep the generated settings file
    #[arg(long = "keep-temp")]
    pub keep_temp: bool,

    /// Print the resolved srt settings, then exit
    #[arg(long = "print-config")]
    pub print_config: bool,

    /// Verify the boundary holds on this machine
    #[arg(long)]
    pub selftest: bool,
}

impl Cli {
    /// `None` when neither flag was given, so the caller can apply its default.
    pub fn clipboard(&self) -> Option<bool> {
        if self.no_clipboard {
            Some(false)
        } else if self.clipboard {
            Some(true)
        } else {
            None
        }
    }
}

/// Split argv into "our arguments" and "pi's arguments".
pub fn split<I>(args: I) -> (Vec<OsString>, Vec<OsString>)
where
    I: IntoIterator<Item = OsString>,
{
    let mut head = Vec::new();
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        let Some(text) = arg.to_str() else {
            // Not valid UTF-8, so it cannot be one of ours.
            let mut tail = vec![arg];
            tail.extend(args);
            return (head, tail);
        };

        if text == "--" {
            return (head, args.collect());
        }

        let (name, inline_value) = match text.split_once('=') {
            Some((name, _)) => (name, true),
            None => (text, false),
        };

        if !inline_value && BOOL_FLAGS.contains(&text) {
            head.push(arg);
            continue;
        }

        if VALUE_FLAGS.contains(&name) {
            head.push(arg);
            if !inline_value {
                if let Some(value) = args.next() {
                    head.push(value);
                }
                // A missing value is left for clap to report.
            }
            continue;
        }

        // Unknown flag, or the first non-option argument.
        let mut tail = vec![arg];
        tail.extend(args);
        return (head, tail);
    }

    (head, Vec::new())
}

/// Parse pisbx's arguments, returning them with pi's untouched tail.
pub fn parse<I>(args: I) -> Result<(Cli, Vec<OsString>), clap::Error>
where
    I: IntoIterator<Item = OsString>,
{
    let (head, tail) = split(args);
    let mut argv = vec![OsString::from("pisbx")];
    argv.extend(head);
    Ok((Cli::try_parse_from(argv)?, tail))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> (Cli, Vec<OsString>) {
        parse(args.iter().map(OsString::from)).expect("parses")
    }

    #[test]
    fn empty_args_leave_everything_at_default() {
        let (cli, tail) = p(&[]);
        assert!(tail.is_empty());
        assert!(cli.dir.is_none());
        assert!(cli.allow_write.is_empty());
        assert!(cli.domains.is_empty());
        assert_eq!(cli.clipboard(), None);
        assert!(!cli.trust && !cli.keep_temp && !cli.print_config && !cli.selftest);
    }

    #[test]
    fn separate_and_inline_values() {
        let (cli, _) = p(&["--allow-write", "/a", "--deny-read=/b"]);
        assert_eq!(cli.allow_write, ["/a"]);
        assert_eq!(cli.deny_read, ["/b"]);
    }

    #[test]
    fn comma_lists_and_repeats_both_accumulate() {
        let (cli, _) = p(&["--allow-write", "/a,/b", "--allow-write=/c"]);
        assert_eq!(cli.allow_write, ["/a", "/b", "/c"]);
    }

    #[test]
    fn stops_at_first_non_option() {
        let (cli, tail) = p(&["--allow-write", "/a", "-p", "--model", "x", "hello"]);
        assert_eq!(cli.allow_write, ["/a"]);
        assert_eq!(tail, ["-p", "--model", "x", "hello"].map(OsString::from));
    }

    #[test]
    fn double_dash_passes_everything_on() {
        let (cli, tail) = p(&["--", "--help"]);
        assert!(!cli.print_config);
        assert_eq!(tail, [OsString::from("--help")]);
    }

    #[test]
    fn clipboard_flags_are_tri_state() {
        assert_eq!(p(&["--clipboard"]).0.clipboard(), Some(true));
        assert_eq!(p(&["--no-clipboard"]).0.clipboard(), Some(false));
    }

    #[test]
    fn missing_value_is_a_usage_error() {
        let error = parse(["--allow-write"].map(OsString::from)).unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::InvalidValue);
    }

    #[test]
    fn socket_and_git_flags() {
        let (cli, _) = p(&["--allow-unix-sockets", "/tmp/a.sock,/tmp/b.sock"]);
        assert_eq!(cli.allow_unix_sockets, ["/tmp/a.sock", "/tmp/b.sock"]);
        assert!(p(&["--allow-git-config"]).0.allow_git_config);
    }

    #[test]
    fn mode_flags() {
        assert!(p(&["--print-config"]).0.print_config);
        assert!(p(&["--selftest"]).0.selftest);
        assert!(p(&["--trust"]).0.trust);
        assert!(p(&["--keep-temp"]).0.keep_temp);
    }
}
