//! pisbx — run the pi coding agent inside a macOS Seatbelt sandbox.
//!
//! The flow is: parse our own flags, resolve the policy, render srt settings,
//! then print, explain, test or run. Nothing here talks to Seatbelt directly;
//! srt owns the enforcement, pisbx owns the decisions.
//!
//! Default posture: home is closed, nothing is reachable on the network, the
//! pasteboard is denied, and only the workspace, pi's state directory and its
//! runtime are open. Everything else comes from `$HOME/.config/pisbx/config.toml`
//! or flags.

mod cli;
mod config;
mod floor;
mod init;
mod paths;
mod policy;
mod selftest;
mod srt;

use anyhow::{Context, Result};
use cli::Cli;
use policy::{merge, Entry, Policy, Source};
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::exit;

const EXIT_USAGE: i32 = 64;
const EXIT_CONFIG: i32 = 65;
const EXIT_MISSING_DEPENDENCY: i32 = 69;
const EXIT_NOT_INITIALISED: i32 = 78;

/// An error carrying the exit code it should produce.
struct Failure {
    code: i32,
    message: String,
}

impl Failure {
    fn new(code: i32, error: impl std::fmt::Display) -> Self {
        Self {
            code,
            message: error.to_string(),
        }
    }

    fn usage(error: impl std::fmt::Display) -> Self {
        Self::new(EXIT_USAGE, error)
    }

    fn config(error: impl std::fmt::Display) -> Self {
        Self::new(EXIT_CONFIG, error)
    }

    fn dependency(error: impl std::fmt::Display) -> Self {
        Self::new(EXIT_MISSING_DEPENDENCY, error)
    }

    fn not_initialised(path: &Path) -> Self {
        Self::new(
            EXIT_NOT_INITIALISED,
            format!(
                "{} does not exist. Run `pisbx --init` to write a starter policy.",
                path.display()
            ),
        )
    }
}

/// The generated settings file, removed on drop unless `keep` is set.
struct SettingsFile {
    file: Option<tempfile::NamedTempFile>,
    keep: bool,
}

impl SettingsFile {
    fn write(contents: &str, keep: bool) -> Result<Self> {
        let mut file = tempfile::Builder::new()
            .prefix("pisbx-")
            .suffix(".json")
            .tempfile()
            .context("could not create the settings file")?;
        file.write_all(contents.as_bytes())
            .and_then(|()| file.flush())
            .context("could not write the settings file")?;
        Ok(Self {
            file: Some(file),
            keep,
        })
    }

    fn path(&self) -> &Path {
        self.file
            .as_ref()
            .expect("settings file is present until dropped")
            .path()
    }
}

impl Drop for SettingsFile {
    fn drop(&mut self) {
        if self.keep {
            if let Some(file) = self.file.take() {
                let _ = file.keep();
            }
        }
    }
}

fn env_list(name: &str) -> Vec<String> {
    std::env::var(name)
        .map(|value| paths::split_list(&value))
        .unwrap_or_default()
}

fn env_flag(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(value) => !matches!(value.trim(), "" | "0" | "false" | "no"),
        Err(_) => default,
    }
}

fn config_path() -> Option<PathBuf> {
    std::env::var_os("PISBX_CONFIG")
        .map(PathBuf::from)
        .or_else(config::default_path)
}

fn load_config(path: Option<&Path>) -> Result<Option<config::Config>> {
    let Some(path) = path else {
        return Ok(None);
    };
    config::load(path).map_err(anyhow::Error::msg)
}

/// Expand every entry into the path spellings to grant, keeping the source.
fn expand(entries: impl IntoIterator<Item = String>, target: &Path, source: Source) -> Vec<Entry> {
    let mut out = Vec::new();
    for entry in entries {
        for spelling in paths::expand(&entry, target) {
            out.push(Entry::new(spelling, source));
        }
    }
    out
}

fn literal(entries: impl IntoIterator<Item = String>, source: Source) -> Vec<Entry> {
    entries
        .into_iter()
        .map(|value| Entry::new(value, source))
        .collect()
}

/// Everything pi needs before any policy question is asked.
fn floor_writes(target: Option<&Path>) -> Vec<Entry> {
    let mut entries = Vec::new();
    if let Some(target) = target {
        entries.extend(expand(
            [target.to_string_lossy().into_owned()],
            target,
            Source::Builtin,
        ));
    }
    for root in floor::WRITE_ROOTS {
        entries.extend(expand(
            [(*root).to_string()],
            target.unwrap_or(Path::new("/")),
            Source::Builtin,
        ));
    }
    entries
}

struct Resolved {
    target: PathBuf,
    policy: Policy,
}

fn resolve(
    cli: &Cli,
    loaded: Option<&config::Config>,
    config_path: Option<&Path>,
) -> Result<Resolved> {
    let target_arg = cli
        .dir
        .clone()
        .or_else(|| std::env::var_os("PISBX_DIR").map(PathBuf::from))
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .to_path_buf()
        });
    let target =
        paths::resolve_dir(&target_arg).with_context(|| format!("{}", target_arg.display()))?;

    let mut policy = Policy {
        read_only: cli.read_only,
        trust: (cli.trust || env_flag("PISBX_TRUST", false)) && !cli.read_only,
        ..Policy::default()
    };

    // Writes: the workspace and the floor, then anything granted on top. Each
    // layer keeps its own label so --explain can say where a grant came from.
    let mut writes = floor_writes(if cli.read_only { None } else { Some(&target) });
    if !cli.read_only {
        writes.extend(expand(env_list("PISBX_ALLOW_WRITE"), &target, Source::Env));
        writes.extend(expand(
            cli.allow_write.iter().cloned(),
            &target,
            Source::Flag,
        ));
        if let Some(config) = loaded {
            writes.extend(expand(
                config.allow_write.iter().cloned(),
                &target,
                Source::Config,
            ));
        }
    }
    merge(&mut policy.allow_write, writes);

    // Denied writes win over everything, including the floor.
    // Deny entries are expanded too: ~/.pi is usually a symlink, and Seatbelt
    // matches the resolved path, so a literal deny would never fire.
    let mut denied = expand(
        floor::ALWAYS_DENY_WRITE.iter().map(|s| s.to_string()),
        &target,
        Source::Builtin,
    );
    if let Ok(executable) = std::env::current_exe() {
        if let Ok(executable) = std::fs::canonicalize(executable) {
            denied.push(Entry::new(executable.to_string_lossy(), Source::Builtin));
        }
    }
    if let Some(path) = config_path {
        denied.push(Entry::new(path.to_string_lossy(), Source::Builtin));
        if let Ok(path) = std::fs::canonicalize(path) {
            denied.push(Entry::new(path.to_string_lossy(), Source::Builtin));
        }
    }
    // Denials apply in every mode: denying more can never cause a write.
    denied.extend(expand(
        cli.deny_write.iter().cloned(),
        &target,
        Source::Flag,
    ));
    if let Some(config) = loaded {
        denied.extend(expand(
            config.deny_write.iter().cloned(),
            &target,
            Source::Config,
        ));
    }
    merge(&mut policy.deny_write, denied);

    // Reads: home is closed, the floor re-opens what pi needs.
    merge(
        &mut policy.deny_read,
        expand(["~".to_string()], &target, Source::Builtin),
    );
    let mut reads = expand(floor::reads(&target), &target, Source::Builtin);
    reads.extend(expand(env_list("PISBX_ALLOW_READ"), &target, Source::Env));
    reads.extend(expand(
        cli.allow_read.iter().cloned(),
        &target,
        Source::Flag,
    ));
    if let Some(config) = loaded {
        reads.extend(expand(
            config.allow_read.iter().cloned(),
            &target,
            Source::Config,
        ));
        merge(
            &mut policy.deny_read,
            expand(config.deny_read.iter().cloned(), &target, Source::Config),
        );
    }
    merge(&mut policy.allow_read, reads);
    merge(
        &mut policy.deny_read,
        expand(env_list("PISBX_DENY_READ"), &target, Source::Env),
    );
    merge(
        &mut policy.deny_read,
        expand(cli.deny_read.iter().cloned(), &target, Source::Flag),
    );

    // Network: nothing by default. The list is replaced, never extended, and
    // the file beats the environment: an inherited PISBX_DOMAINS from an outer
    // session must not quietly override the policy you wrote.
    let domains = if !cli.domains.is_empty() {
        literal(cli.domains.iter().cloned(), Source::Flag)
    } else if let Some(config) = loaded.filter(|config| !config.domains.is_empty()) {
        literal(config.domains.iter().cloned(), Source::Config)
    } else {
        literal(env_list("PISBX_DOMAINS"), Source::Env)
    };
    merge(&mut policy.domains, domains);
    if let Some(config) = loaded {
        merge(
            &mut policy.denied_domains,
            literal(config.denied_domains.iter().cloned(), Source::Config),
        );
    }

    // Switches srt exposes that are off unless asked for. --read-only wins:
    // writing .git/config is still writing.
    policy.allow_git_config = !cli.read_only
        && (cli.allow_git_config
            || env_flag("PISBX_ALLOW_GIT_CONFIG", false)
            || loaded
                .map(|config| config.allow_git_config)
                .unwrap_or(false));

    merge(
        &mut policy.unix_sockets,
        expand(env_list("PISBX_ALLOW_UNIX_SOCKETS"), &target, Source::Env),
    );
    merge(
        &mut policy.unix_sockets,
        expand(
            cli.allow_unix_sockets.iter().cloned(),
            &target,
            Source::Flag,
        ),
    );
    if let Some(config) = loaded {
        merge(
            &mut policy.unix_sockets,
            expand(
                config.allow_unix_sockets.iter().cloned(),
                &target,
                Source::Config,
            ),
        );
    }

    let clipboard = cli
        .clipboard()
        .unwrap_or_else(|| env_flag("PISBX_CLIPBOARD", false));
    if clipboard {
        merge(
            &mut policy.mach_lookup,
            literal(
                policy::CLIPBOARD_MACH_SERVICES
                    .iter()
                    .map(|s| s.to_string()),
                Source::Builtin,
            ),
        );
    }
    if let Some(config) = loaded {
        merge(
            &mut policy.mach_lookup,
            literal(config.mach_lookup.iter().cloned(), Source::Config),
        );
    }

    Ok(Resolved { target, policy })
}

fn main() {
    match run() {
        Ok(code) => exit(code),
        Err(failure) => {
            eprintln!("pisbx: {}", failure.message);
            exit(failure.code);
        }
    }
}

fn run() -> Result<i32, Failure> {
    let (cli, tail) = match cli::parse(std::env::args_os().skip(1)) {
        Ok(parsed) => parsed,
        Err(error) => {
            let help = matches!(error.kind(), clap::error::ErrorKind::DisplayHelp);
            let _ = error.print();
            return Ok(if help { 0 } else { EXIT_USAGE });
        }
    };

    let Some(config_path) = config_path() else {
        return Err(Failure::usage(
            "HOME is not set, so there is no config path",
        ));
    };

    // --init writes the policy and needs nothing else: not srt, not pi, not an
    // existing config.
    if cli.init {
        match init::write(&config_path, cli.force)
            .map_err(|error| Failure::config(format!("{error:#}")))?
        {
            init::Outcome::Written(path) => {
                let draft = init::draft();
                println!("wrote {}", path.display());
                println!(
                    "  {} readable paths, {} provider domains{}",
                    draft.allow_read.len(),
                    draft.domains.len(),
                    draft
                        .provider
                        .map(|provider| format!(" (provider {provider})"))
                        .unwrap_or_default()
                );
                println!(
                    "  clipboard left commented out: uncomment the mach-lookup line to enable it"
                );
                println!("\nreview it with `pisbx --explain`");
                Ok(0)
            }
            init::Outcome::Exists(path) => {
                eprintln!(
                    "pisbx: {} already exists; pass --force to replace it",
                    path.display()
                );
                Ok(0)
            }
        }
    } else {
        let loaded = load_config(Some(&config_path)).map_err(Failure::config)?;
        let resolved = resolve(&cli, loaded.as_ref(), Some(&config_path))
            .map_err(|error| Failure::usage(format!("{error:#}")))?;
        let settings = resolved.policy.to_json();
        let keep_temp = cli.keep_temp || env_flag("PISBX_KEEP_TEMP", false);

        if cli.explain {
            print!(
                "{}",
                resolved
                    .policy
                    .explain(&resolved.target, Some(&config_path))
            );
            return Ok(0);
        }

        if cli.print_config {
            print!("{settings}");
            return Ok(0);
        }

        if cli.selftest {
            let file = SettingsFile::write(&settings, keep_temp)
                .map_err(|error| Failure::usage(format!("{error:#}")))?;
            // Prove the first extra write grant works, if there is one.
            let probe = resolved
                .policy
                .allow_write
                .iter()
                .filter(|entry| entry.source != Source::Builtin)
                .map(|entry| entry.value.clone())
                .find(|entry| Path::new(entry).is_dir());
            return Ok(
                if selftest::run(
                    file.path(),
                    &resolved.target,
                    cli.read_only,
                    probe.as_deref(),
                ) {
                    0
                } else {
                    1
                },
            );
        }

        // Only a real session insists that the policy file exists; the
        // inspection modes above work from the floor alone.
        if !config_path.exists() {
            return Err(Failure::not_initialised(&config_path));
        }

        srt::locate("srt").ok_or_else(|| {
            Failure::dependency(
                "srt not found on PATH (mise use -g npm:@anthropic-ai/sandbox-runtime)",
            )
        })?;
        srt::locate("pi").ok_or_else(|| Failure::dependency("pi not found on PATH"))?;

        let file = SettingsFile::write(&settings, keep_temp)
            .map_err(|error| Failure::usage(format!("{error:#}")))?;

        let mut argv: Vec<OsString> = vec![OsString::from("pi")];
        if !resolved.policy.trust {
            argv.push(OsString::from("--no-approve"));
        }
        argv.extend(tail);

        let status = srt::run(file.path(), &argv)
            .map_err(|error| Failure::dependency(format!("could not run srt: {error}")))?;
        Ok(status.code().unwrap_or(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_workspace_is_writable_only_when_asked_for() {
        let with = floor_writes(Some(Path::new("/tmp/project")));
        let without = floor_writes(None);

        assert!(with.iter().any(|entry| entry.value == "/tmp/project"));
        assert!(!without.iter().any(|entry| entry.value == "/tmp/project"));
        assert!(without.iter().all(|entry| entry.source == Source::Builtin));
    }

    #[test]
    fn config_entries_keep_their_label() {
        let dir = std::env::temp_dir().join(format!("pisbx-resolve-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let loaded = config::Config {
            allow_read: vec!["/tmp/config-read".to_string()],
            domains: vec!["api.example".to_string()],
            ..config::Config::default()
        };
        let cli = Cli {
            dir: Some(dir.clone()),
            ..Cli::default()
        };

        let resolved = resolve(&cli, Some(&loaded), None).unwrap();
        assert!(
            resolved
                .policy
                .allow_read
                .iter()
                .any(|entry| entry.value == "/tmp/config-read" && entry.source == Source::Config),
            "{:?}",
            resolved.policy.allow_read
        );
        assert!(
            resolved
                .policy
                .domains
                .iter()
                .all(|entry| entry.source == Source::Config),
            "{:?}",
            resolved.policy.domains
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn config_switches_reach_the_settings() {
        let loaded = config::Config {
            allow_git_config: true,
            allow_unix_sockets: vec!["/tmp/agent.sock".to_string()],
            ..config::Config::default()
        };
        let cli = Cli::default();
        let resolved = resolve(&cli, Some(&loaded), None).unwrap();

        assert!(resolved.policy.allow_git_config);
        assert!(resolved
            .policy
            .unix_sockets
            .iter()
            .any(|entry| entry.value == "/tmp/agent.sock" && entry.source == Source::Config));

        let settings = resolved.policy.settings();
        assert!(settings.filesystem.allow_git_config);
        assert_eq!(settings.network.allow_unix_sockets, ["/tmp/agent.sock"]);
    }

    #[test]
    fn read_only_also_closes_git_config() {
        let loaded = config::Config {
            allow_git_config: true,
            ..config::Config::default()
        };
        let cli = Cli {
            read_only: true,
            ..Cli::default()
        };
        let resolved = resolve(&cli, Some(&loaded), None).unwrap();
        assert!(!resolved.policy.allow_git_config);
    }

    #[test]
    fn read_only_drops_every_extra_write() {
        let loaded = config::Config {
            allow_write: vec!["/tmp/from-config".to_string()],
            ..config::Config::default()
        };
        let cli = Cli {
            read_only: true,
            allow_write: vec!["/tmp/from-flag".to_string()],
            ..Cli::default()
        };

        let resolved = resolve(&cli, Some(&loaded), None).unwrap();
        assert!(
            resolved
                .policy
                .allow_write
                .iter()
                .all(|entry| entry.source == Source::Builtin),
            "{:?}",
            resolved.policy.allow_write
        );
    }

    #[test]
    fn expand_keeps_the_source() {
        let entries = expand(["/tmp/a".to_string()], Path::new("/tmp"), Source::Flag);
        assert_eq!(entries[0], Entry::new("/tmp/a", Source::Flag));
    }

    #[test]
    fn settings_file_removes_itself() {
        let path = {
            let file = SettingsFile::write("{}", false).unwrap();
            let path = file.path().to_path_buf();
            assert!(path.exists());
            path
        };
        assert!(!path.exists(), "settings file leaked at {path:?}");
    }
}
