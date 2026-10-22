//! The policy file: srt's settings document, written in TOML.
//!
//! One schema, so there is nothing to translate and no way to put a key in the
//! wrong place. pisbx reads the parts it must interpret (paths, domains, the
//! switches that interact with its own defaults) and forwards everything else
//! untouched, so an srt option pisbx has never heard of still works.

use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const DEFAULT_PATH: &str = ".config/pisbx/config.toml";

/// Top-level keys srt accepts. Anything else is forwarded but flagged, because
/// srt's own validation drops unknown keys silently rather than complaining.
const KNOWN_NETWORK: &[&str] = &[
    "allowAllUnixSockets",
    "allowLocalBinding",
    "allowMachLookup",
    "allowPlaintextHeaderSet",
    "allowUnixSockets",
    "allowedDomains",
    "ambiguousRequestTarget",
    "deniedDomains",
    "deniedDomainReasons",
    "deniedResolvedAddresses",
    "filterRequest",
    "httpProxyPort",
    "mitmProxy",
    "parentProxy",
    "refuseOpaqueTunnels",
    "requireHostMatch",
    "strictAllowlist",
    "stripResponseHeaders",
    "tlsTerminate",
];

const KNOWN_TOP_LEVEL: &[&str] = &[
    "allowAppleEvents",
    "allowPty",
    "credentials",
    "enableWeakerNestedSandbox",
    "enableWeakerNetworkIsolation",
    "git",
    "ignoreViolations",
    "javaAgentJarPath",
    "mandatoryDenySearchDepth",
    "ripgrep",
    "seccomp",
    "windows",
];

/// A path entry: either a plain string (glob if it reads as one) or the
/// `{ path, literal }` form srt uses to name a path and nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum PathEntry {
    Plain(String),
    Literal {
        path: String,
        #[serde(default)]
        literal: bool,
    },
}

impl PathEntry {
    pub fn text(&self) -> &str {
        match self {
            PathEntry::Plain(text) => text,
            PathEntry::Literal { path, .. } => path,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Filesystem {
    pub allow_read: Vec<PathEntry>,
    pub allow_write: Vec<PathEntry>,
    pub deny_read: Vec<PathEntry>,
    pub deny_write: Vec<PathEntry>,
    /// Writes to `.git/config`; `.git/hooks` stay denied either way.
    pub allow_git_config: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Network {
    pub allowed_domains: Vec<String>,
    pub denied_domains: Vec<String>,
    pub allow_unix_sockets: Vec<PathEntry>,
    pub allow_mach_lookup: Vec<String>,
    /// Everything pisbx only forwards: deniedDomainReasons, allowLocalBinding,
    /// deniedResolvedAddresses, tlsTerminate, mitmProxy, …
    #[serde(flatten)]
    pub rest: toml::Table,
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    pub filesystem: Filesystem,
    pub network: Network,
    /// Settings pisbx does not model, forwarded to srt as-is.
    #[serde(flatten)]
    pub rest: toml::Table,
}

impl Config {
    /// Keys that will be forwarded but that nothing recognises.
    pub fn unknown_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .rest
            .keys()
            .filter(|key| !KNOWN_TOP_LEVEL.contains(&key.as_str()))
            .cloned()
            .collect();
        keys.extend(
            self.network
                .rest
                .keys()
                .filter(|key| !KNOWN_NETWORK.contains(&key.as_str()))
                .map(|key| format!("network.{key}")),
        );
        keys.sort();
        keys
    }
}

pub fn default_path() -> Option<PathBuf> {
    crate::paths::home().map(|home| home.join(DEFAULT_PATH))
}

/// Parse the policy file. Unknown keys inside `filesystem` or `network` are an
/// error; unknown top-level keys are passed through to srt.
pub fn parse(text: &str, source: &Path) -> Result<Config, String> {
    toml::from_str(text).map_err(|error| format!("{}: {error}", source.display()))
}

/// Load the config if it exists. A missing file is not an error; an unreadable
/// or malformed one is.
pub fn load(path: &Path) -> Result<Option<Config>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text, path).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(text: &str) -> Result<Config, String> {
        parse(text, Path::new("test.toml"))
    }

    #[test]
    fn empty_input_is_an_empty_config() {
        let config = p("").unwrap();
        assert_eq!(config, Config::default());
        assert!(!config.filesystem.allow_git_config);
        assert!(config.unknown_keys().is_empty());
    }

    #[test]
    fn reads_the_srt_schema() {
        let config = p(r#"
[filesystem]
allowRead = ["~/Projects/dotfiles", { path = "/work/*.env", literal = true }]
allowWrite = ["~/scratch"]
denyRead = ["~/vault"]
denyWrite = [".env"]
allowGitConfig = true

[network]
allowedDomains = ["api.openai.com", "*.github.com"]
deniedDomains = ["*:22"]
allowUnixSockets = ["~/Library/Group Containers/TEAM.app/t/agent.sock"]
allowMachLookup = ["com.apple.pasteboard.1"]
allowLocalBinding = true
deniedResolvedAddresses = ["10.0.0.0/8"]

[network.deniedDomainReasons]
"github.com:22" = "SSH pushes are blocked; use https"
"#)
        .unwrap();

        assert_eq!(config.filesystem.allow_read.len(), 2);
        assert_eq!(
            config.filesystem.allow_read[0].text(),
            "~/Projects/dotfiles"
        );
        assert_eq!(config.filesystem.allow_read[1].text(), "/work/*.env");
        assert_eq!(config.filesystem.allow_write[0].text(), "~/scratch");
        assert_eq!(config.filesystem.deny_read[0].text(), "~/vault");
        assert_eq!(config.filesystem.deny_write[0].text(), ".env");
        assert!(config.filesystem.allow_git_config);

        assert_eq!(
            config.network.allowed_domains,
            ["api.openai.com", "*.github.com"]
        );
        assert_eq!(config.network.denied_domains, ["*:22"]);
        // Forwarded, not modelled.
        assert_eq!(
            config.network.rest["deniedDomainReasons"]["github.com:22"],
            toml::Value::String("SSH pushes are blocked; use https".into())
        );
        assert_eq!(
            config.network.rest["allowLocalBinding"],
            toml::Value::Boolean(true)
        );
        assert_eq!(
            config.network.rest["deniedResolvedAddresses"][0],
            toml::Value::String("10.0.0.0/8".into())
        );
        assert!(config.unknown_keys().is_empty());
    }

    #[test]
    fn unmodelled_network_settings_are_forwarded() {
        let config =
            p("[network.tlsTerminate]\nexcludeDomains = [\"internal.example\"]\n").unwrap();
        assert!(config.network.rest.contains_key("tlsTerminate"));
        assert!(config.unknown_keys().is_empty());
    }

    #[test]
    fn unknown_filesystem_keys_are_an_error() {
        let error = p("[filesystem]\nallowread = [\"/a\"]\n").unwrap_err();
        assert!(
            error.contains("allowread") || error.contains("unknown field"),
            "{error}"
        );
    }

    #[test]
    fn unknown_top_level_keys_are_forwarded_and_flagged() {
        let config = p("[ignoreViolations]\n\"*\" = [\"/usr/bin\"]\n").unwrap();
        assert!(config.rest.contains_key("ignoreViolations"));
        assert!(
            config.unknown_keys().is_empty(),
            "ignoreViolations is a known srt key"
        );

        let config = p("allowPtyy = true\n").unwrap();
        assert_eq!(config.unknown_keys(), ["allowPtyy"]);

        let config = p("[network]\nallowLocalBindings = true\n").unwrap();
        assert_eq!(config.unknown_keys(), ["network.allowLocalBindings"]);
    }

    #[test]
    fn malformed_toml_reports_the_file() {
        let error = p("[filesystem\n").unwrap_err();
        assert!(error.contains("test.toml"), "{error}");
    }

    #[test]
    fn missing_files_are_not_errors() {
        assert_eq!(
            load(Path::new("/nonexistent/pisbx/config.toml")).unwrap(),
            None
        );
    }
}
