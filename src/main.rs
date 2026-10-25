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
mod credentials;
mod floor;
mod init;
mod paths;
mod policy;
mod selftest;
mod srt;
mod why;

use anyhow::{Context, Result};
use cli::Cli;
use policy::{merge, merge_extra, Entry, Policy, Source};
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::exit;

/// Probe used by `--why --probe`: the kernel's answer, with no side effects
/// beyond removing a file the probe itself created.
const PROBE_SCRIPT: &str = r#"
import errno, os, sys

kind, path = sys.argv[1], sys.argv[2]
flags = os.O_RDONLY if kind == "read" else (os.O_WRONLY | os.O_APPEND | os.O_CREAT)
created = kind == "write" and not os.path.exists(path)

try:
    fd = os.open(path, flags)
    os.close(fd)
    if created:
        os.unlink(path)
    print("allowed")
except OSError as error:
    print("denied", errno.errorcode.get(error.errno, str(error.errno)))
"#;

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
    env: Vec<(String, String)>,
    unset: Vec<String>,
}

fn text_entries(entries: &[config::PathEntry]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| entry.text().to_string())
        .collect()
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

    let mut env_pairs: Vec<(String, String)> = Vec::new();
    let mut unset_names: Vec<String> = Vec::new();
    let mut policy = Policy {
        read_only: cli.read_only,
        trust: (cli.trust
            || env_flag("PISBX_TRUST", false)
            || loaded.map(|config| config.pisbx.trust).unwrap_or(false))
            && !cli.read_only,
        ..Policy::default()
    };

    // Writes: the workspace and the floor, then whatever the file, the
    // environment and the flags add.
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
                text_entries(&config.filesystem.allow_write),
                &target,
                Source::Config,
            ));
        }
    }
    merge(&mut policy.allow_write, writes);

    // Denied writes win over everything, including the floor.
    let mut denied = expand(
        floor::ALWAYS_DENY_WRITE.iter().map(|s| s.to_string()),
        &target,
        Source::Builtin,
    );
    denied.extend(expand(
        floor::ALWAYS_DENY_WRITE_PROJECT
            .iter()
            .map(|s| s.to_string()),
        &target,
        Source::Builtin,
    ));
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
    denied.extend(expand(
        cli.deny_write.iter().cloned(),
        &target,
        Source::Flag,
    ));
    if let Some(config) = loaded {
        denied.extend(expand(
            text_entries(&config.filesystem.deny_write),
            &target,
            Source::Config,
        ));
    }
    merge(&mut policy.deny_write, denied);

    // Reads: home is closed, then the floor re-opens what pi needs.
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
            text_entries(&config.filesystem.allow_read),
            &target,
            Source::Config,
        ));
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
    if let Some(config) = loaded {
        merge(
            &mut policy.deny_read,
            expand(
                text_entries(&config.filesystem.deny_read),
                &target,
                Source::Config,
            ),
        );
    }

    // Network: the list is replaced, never extended. Flags win, then the file,
    // then the environment, so an inherited PISBX_DOMAINS cannot override a
    // policy you wrote.
    let domains = if !cli.domains.is_empty() {
        literal(cli.domains.iter().cloned(), Source::Flag)
    } else if let Some(config) = loaded.filter(|config| !config.network.allowed_domains.is_empty())
    {
        literal(
            config.network.allowed_domains.iter().cloned(),
            Source::Config,
        )
    } else {
        literal(env_list("PISBX_DOMAINS"), Source::Env)
    };
    merge(&mut policy.domains, domains);
    if let Some(config) = loaded {
        merge(
            &mut policy.denied_domains,
            literal(
                config.network.denied_domains.iter().cloned(),
                Source::Config,
            ),
        );
    }

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
                text_entries(&config.network.allow_unix_sockets),
                &target,
                Source::Config,
            ),
        );
    }

    // Pasteboard: whatever the file lists, plus the built-in trio when asked
    // for, minus it when explicitly refused.
    if let Some(config) = loaded {
        merge(
            &mut policy.mach_lookup,
            literal(
                config.network.allow_mach_lookup.iter().cloned(),
                Source::Config,
            ),
        );
    }
    let clipboard = cli.clipboard().or_else(|| {
        std::env::var("PISBX_CLIPBOARD")
            .ok()
            .map(|value| !matches!(value.trim(), "" | "0" | "false" | "no"))
    });
    match clipboard {
        Some(true) => merge(
            &mut policy.mach_lookup,
            literal(
                policy::CLIPBOARD_MACH_SERVICES
                    .iter()
                    .map(|s| s.to_string()),
                Source::Builtin,
            ),
        ),
        Some(false) => policy
            .mach_lookup
            .retain(|entry| !policy::CLIPBOARD_MACH_SERVICES.contains(&entry.value.as_str())),
        None => {}
    }

    // --read-only wins: writing .git/config is still writing.
    policy.allow_git_config = !cli.read_only
        && (cli.allow_git_config
            || env_flag("PISBX_ALLOW_GIT_CONFIG", false)
            || loaded
                .map(|config| config.filesystem.allow_git_config)
                .unwrap_or(false));

    // [pisbx.env]: environment for the process srt launches, not for srt.
    if let Some(config) = loaded {
        for (key, value) in &config.pisbx.env {
            let value = paths::expand(value, &target)
                .into_iter()
                .next()
                .unwrap_or_else(|| value.clone());
            policy.env.push(Entry::new(key.clone(), Source::Config));
            env_pairs.push((key.clone(), value));
        }
    }

    if let Some(config) = loaded {
        for name in &config.pisbx.unset {
            policy.unset.push(Entry::new(name.clone(), Source::Config));
            unset_names.push(name.clone());
        }
    }

    // Everything pisbx does not interpret is forwarded as-is.
    let mut extras = serde_json::json!({});
    if let Some(config) = loaded {
        if !config.rest.is_empty() {
            let table = serde_json::to_value(&config.rest).unwrap_or_default();
            merge_extra(&mut extras, &table);
        }
        if !config.network.rest.is_empty() {
            let network = serde_json::json!({
                "network": serde_json::to_value(&config.network.rest).unwrap_or_default()
            });
            merge_extra(&mut extras, &network);
        }
    }
    policy.extras = extras;

    Ok(Resolved {
        target,
        policy,
        env: env_pairs,
        unset: unset_names,
    })
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
        if let Some(config) = loaded.as_ref() {
            for key in config.unknown_keys() {
                eprintln!("pisbx: warning: {key} is not a known setting; forwarded to srt anyway");
            }
        }
        let resolved = resolve(&cli, loaded.as_ref(), Some(&config_path))
            .map_err(|error| Failure::usage(format!("{error:#}")))?;
        let settings = resolved.policy.to_json();
        let keep_temp = cli.keep_temp || env_flag("PISBX_KEEP_TEMP", false);

        if let Some(query) = &cli.why {
            let op = match cli.op.as_deref() {
                None => why::Op::Read,
                Some(text) => match why::Op::parse(text) {
                    Some(op) => op,
                    None => {
                        return Err(Failure::usage(format!(
                            "--op must be read or write, not `{text}`"
                        )))
                    }
                },
            };

            let (path, verdict) = why::evaluate(&resolved.policy, query, op, &resolved.target);
            println!("path    {path}");
            println!("op      {}", op.label());
            match &verdict {
                why::Verdict::Allowed(Some(entry)) => println!(
                    "result  allowed\nby      {} ({})",
                    entry.value,
                    entry.source.label()
                ),
                why::Verdict::Allowed(None) => {
                    println!("result  allowed\nby      (no entry; nothing denies it)")
                }
                why::Verdict::Denied(Some(entry)) => println!(
                    "result  denied\nby      {} ({})",
                    entry.value,
                    entry.source.label()
                ),
                why::Verdict::Denied(None) => {
                    println!("result  denied\nby      (nothing grants it)")
                }
            }

            if cli.probe {
                let file = SettingsFile::write(&settings, keep_temp)
                    .map_err(|error| Failure::usage(format!("{error:#}")))?;
                let script = SettingsFile::write(PROBE_SCRIPT, keep_temp)
                    .map_err(|error| Failure::usage(format!("{error:#}")))?;
                match srt::capture_args(
                    file.path(),
                    &[
                        "python3",
                        &script.path().to_string_lossy(),
                        op.label(),
                        &path,
                    ],
                    &resolved.target,
                ) {
                    Ok(output) => {
                        let line = output.lines().last().unwrap_or("").trim();
                        println!("probe   {line}");
                    }
                    Err(error) => println!("probe   could not run: {error}"),
                }
            }

            return Ok(if verdict.is_allowed() { 0 } else { 1 });
        }

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

        // Facts about this session, for the pi extension. Appended last so a
        // config entry cannot shadow them (and config rejects PISBX_* anyway).
        // Credential routes: the real secret never enters the sandbox.
        let mut env = resolved.env.clone();
        if let Some(config) = loaded.as_ref() {
            if !config.pisbx.credentials.is_empty() {
                if config.network.rest.get("allowLocalBinding") != Some(&toml::Value::Boolean(true))
                {
                    return Err(Failure::config(
                        "credential routes need `network.allowLocalBinding = true`, so the sandbox may reach the proxy on 127.0.0.1",
                    ));
                }
                let routes: Vec<credentials::Route> =
                    config.pisbx.credentials.values().cloned().collect();
                let proxy = credentials::Proxy::start_on(
                    config.pisbx.credential_proxy_port.unwrap_or(0),
                    routes,
                )
                .map_err(|error| Failure::config(format!("credential proxy: {error:#}")))?;
                env.push(("PISBX_CREDPROXY".to_string(), "1".to_string()));
                env.push(("PISBX_PHANTOM".to_string(), proxy.phantom.clone()));
                for (name, route) in &config.pisbx.credentials {
                    env.push((
                        format!("PISBX_CREDPROXY_{}", name.to_ascii_uppercase()),
                        proxy.base_url(name),
                    ));
                    if let Some(variable) = &route.env {
                        env.push((variable.clone(), proxy.phantom.clone()));
                    }
                }
                println!(
                    "pisbx: credential proxy on 127.0.0.1:{} for {} route(s)",
                    proxy.port,
                    config.pisbx.credentials.len()
                );
                // Keep it alive for the session.
                std::mem::forget(proxy);
            }
        }
        env.push(("PISBX_ACTIVE".to_string(), "1".to_string()));
        env.push((
            "PISBX_VERSION".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
        ));
        env.push((
            "PISBX_SETTINGS".to_string(),
            file.path().display().to_string(),
        ));
        env.push((
            "PISBX_POLICY".to_string(),
            config_path.display().to_string(),
        ));

        let status = srt::run(file.path(), &argv, &env, &resolved.unset)
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
    fn project_config_is_denied_relative_to_the_target() {
        let dir = std::env::temp_dir().join(format!("pisbx-project-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = std::fs::canonicalize(&dir).unwrap();
        let cli = Cli {
            dir: Some(dir.clone()),
            ..Cli::default()
        };

        let resolved = resolve(&cli, None, None).unwrap();
        for entry in [
            ".pi",
            ".agents",
            "AGENTS.md",
            "AGENTS.override.md",
            "CLAUDE.md",
        ] {
            let expected = target.join(entry).to_string_lossy().into_owned();
            assert!(
                resolved
                    .policy
                    .deny_write
                    .iter()
                    .any(|denied| denied.value == expected && denied.source == Source::Builtin),
                "{entry} missing from {:?}",
                resolved.policy.deny_write
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn config_entries_keep_their_label() {
        let dir = std::env::temp_dir().join(format!("pisbx-resolve-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let loaded = config::Config {
            filesystem: config::Filesystem {
                allow_read: vec![config::PathEntry::Plain("/tmp/config-read".into())],
                ..config::Filesystem::default()
            },
            network: config::Network {
                allowed_domains: vec!["api.example".to_string()],
                ..config::Network::default()
            },
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
            filesystem: config::Filesystem {
                allow_git_config: true,
                ..config::Filesystem::default()
            },
            network: config::Network {
                allow_unix_sockets: vec![config::PathEntry::Plain("/tmp/agent.sock".into())],
                ..config::Network::default()
            },
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
        // /tmp resolves to /private/tmp, so both spellings are granted.
        assert!(
            settings
                .network
                .allow_unix_sockets
                .iter()
                .any(|entry| entry == "/tmp/agent.sock"),
            "{:?}",
            settings.network.allow_unix_sockets
        );
    }

    #[test]
    fn pisbx_section_sets_trust_and_env_without_reaching_srt() {
        let loaded = config::Config {
            pisbx: config::Pisbx {
                trust: true,
                env: [(
                    "SSH_AUTH_SOCK".to_string(),
                    "~/Library/Group Containers/TEAM.app/t/agent.sock".to_string(),
                )]
                .into_iter()
                .collect(),
                unset: vec!["GITHUB_TOKEN".to_string()],
                credentials: Default::default(),
                credential_proxy_port: None,
            },
            ..config::Config::default()
        };

        let resolved = resolve(&Cli::default(), Some(&loaded), None).unwrap();
        assert!(resolved.policy.trust, "trust comes from the file");
        assert_eq!(resolved.env.len(), 1);
        assert_eq!(resolved.env[0].0, "SSH_AUTH_SOCK");
        assert!(resolved.env[0].1.starts_with('/'), "{:?}", resolved.env);
        // Names only in the explanation, never values.
        assert!(resolved
            .policy
            .env
            .iter()
            .any(|entry| entry.value == "SSH_AUTH_SOCK"));
        assert!(!resolved.policy.extras.to_string().contains("SSH_AUTH_SOCK"));
        assert_eq!(resolved.unset, ["GITHUB_TOKEN"]);
        assert!(resolved
            .policy
            .unset
            .iter()
            .any(|entry| entry.value == "GITHUB_TOKEN" && entry.source == Source::Config));
    }

    #[test]
    fn read_only_still_vetoes_config_trust() {
        let loaded = config::Config {
            pisbx: config::Pisbx {
                trust: true,
                ..config::Pisbx::default()
            },
            ..config::Config::default()
        };
        let cli = Cli {
            read_only: true,
            ..Cli::default()
        };
        let resolved = resolve(&cli, Some(&loaded), None).unwrap();
        assert!(!resolved.policy.trust);
    }

    #[test]
    fn unmodelled_settings_are_forwarded_add_only() {
        let loaded = config::Config {
            network: config::Network {
                rest: toml::from_str(
                    "allowLocalBinding = true\n[deniedDomainReasons]\n\"github.com:22\" = \"use https\"\n",
                )
                .unwrap_or_default(),
                ..config::Network::default()
            },
            ..config::Config::default()
        };
        let resolved = resolve(&Cli::default(), Some(&loaded), None).unwrap();
        let settings = resolved.policy.settings();

        // Pisbx's own decisions are untouched...
        assert!(settings.allow_pty);
        // ...and the forwarded ones are merged in.
        assert_eq!(resolved.policy.extras["network"]["allowLocalBinding"], true);
        assert_eq!(
            resolved.policy.extras["network"]["deniedDomainReasons"]["github.com:22"],
            "use https"
        );
    }

    #[test]
    fn read_only_also_closes_git_config() {
        let loaded = config::Config {
            filesystem: config::Filesystem {
                allow_git_config: true,
                ..config::Filesystem::default()
            },
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
            filesystem: config::Filesystem {
                allow_write: vec![config::PathEntry::Plain("/tmp/from-config".into())],
                ..config::Filesystem::default()
            },
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
