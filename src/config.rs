//! The policy file: srt's settings document, written in TOML.
//!
//! One schema, so there is nothing to translate and no way to put a key in the
//! wrong place. pisbx reads the parts it must interpret (paths, domains, the
//! switches that interact with its own defaults) and forwards everything else
//! untouched, so an srt option pisbx has never heard of still works.

use serde::Deserialize;
use std::collections::BTreeMap;
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

/// Settings about how pisbx runs pi. Never forwarded to srt: `trust` becomes
/// `--no-approve` (or not), and `env` is applied to the process srt launches.
#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Pisbx {
    /// Load project `.pi` extensions, skills and prompts without asking.
    pub trust: bool,
    /// Environment variables for the sandboxed process. Names are shown by
    /// `--explain`; values never are.
    pub env: BTreeMap<String, String>,
    /// Variables removed from the environment the sandboxed process inherits,
    /// so a token in your shell does not travel into the sandbox.
    pub unset: Vec<String>,
    /// Provider routes whose real secret stays outside the sandbox.
    pub credentials: BTreeMap<String, crate::credentials::Route>,
    /// Fixed proxy port, for a `models.json` base URL that cannot interpolate
    /// an ephemeral one.
    pub credential_proxy_port: Option<u16>,
}

/// A per-project policy section, keyed in `[projects]` by the resolved
/// directory it applies to. Unlike the top-level tables, unknown keys here are
/// an error and there is no srt passthrough: the section may only add the
/// modelled lists and `allowGitConfig`.
#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct Project {
    pub filesystem: Filesystem,
    pub network: ProjectNetwork,
}

/// The network settings a project section may set. Scalar srt switches stay
/// host-global, so a project cannot change them by accident or by review slip.
#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct ProjectNetwork {
    pub allowed_domains: Vec<String>,
    pub denied_domains: Vec<String>,
    pub allow_unix_sockets: Vec<PathEntry>,
    pub allow_mach_lookup: Vec<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    #[serde(rename = "pisbx")]
    pub pisbx: Pisbx,
    pub filesystem: Filesystem,
    pub network: Network,
    /// Per-project deltas, keyed by a `~` or absolute path. Host-authored: the
    /// file is read from `$HOME`, never from the workspace, so a cloned repo
    /// cannot add one.
    pub projects: BTreeMap<String, Project>,
    /// Settings pisbx does not model, forwarded to srt as-is.
    #[serde(flatten)]
    pub rest: toml::Table,
}

impl Config {
    /// Rules serde cannot express: an env var cannot be both set and unset, and
    /// a name has to look like one.
    fn valid_name(name: &str) -> bool {
        let mut chars = name.chars();
        match chars.next() {
            Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
            _ => return false,
        }
        chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    }

    /// Names pisbx sets itself; letting a config shadow them would forge the
    /// session facts the pi extension reads.
    fn reserved(name: &str) -> bool {
        name.starts_with("PISBX_")
    }

    fn validate(&self, source: &Path) -> Result<(), String> {
        let mut resolved: BTreeMap<String, &str> = BTreeMap::new();
        for key in self.projects.keys() {
            let anchored = key == "~" || key.starts_with("~/") || Path::new(key).is_absolute();
            if !anchored {
                return Err(format!(
                    "{}: projects.{key:?}: project keys must be absolute, `~`, or start with `~/`",
                    source.display()
                ));
            }
            if crate::paths::has_glob(key) {
                return Err(format!(
                    "{}: projects.{key:?}: project keys must name one directory, not a glob",
                    source.display()
                ));
            }
            let resolved_key = crate::paths::expand(key, Path::new("/"))
                .pop()
                .unwrap_or_else(|| key.clone());
            if let Some(previous) = resolved.insert(resolved_key.clone(), key) {
                return Err(format!(
                    "{}: projects.{previous:?} and projects.{key:?} both resolve to {resolved_key}",
                    source.display()
                ));
            }
        }
        for (index, name) in self.pisbx.unset.iter().enumerate() {
            if !Self::valid_name(name) {
                return Err(format!(
                    "{}: pisbx.unset[{index}]: `{name}` is not an environment variable name",
                    source.display()
                ));
            }
            if Self::reserved(name) {
                return Err(format!(
                    "{}: pisbx.unset[{index}]: `{name}` is reserved by pisbx",
                    source.display()
                ));
            }
            if self.pisbx.env.contains_key(name) {
                return Err(format!(
                    "{}: pisbx.unset[{index}]: `{name}` is also set in pisbx.env",
                    source.display()
                ));
            }
        }
        for (name, route) in &self.pisbx.credentials {
            if route.name != *name {
                return Err(format!(
                    "{}: pisbx.credentials.{name}: name must be `{name}`",
                    source.display()
                ));
            }
            route
                .validate()
                .map_err(|error| format!("{}: {error}", source.display()))?;
        }
        for name in self.pisbx.env.keys() {
            if Self::reserved(name) {
                return Err(format!(
                    "{}: pisbx.env: `{name}` is reserved by pisbx",
                    source.display()
                ));
            }
            if !Self::valid_name(name) {
                return Err(format!(
                    "{}: pisbx.env: `{name}` is not an environment variable name",
                    source.display()
                ));
            }
        }
        Ok(())
    }

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
    let mut config: Config =
        toml::from_str(text).map_err(|error| format!("{}: {error}", source.display()))?;
    // A credential route is named by its table key, not by a field inside it.
    for (name, route) in config.pisbx.credentials.iter_mut() {
        route.name = name.clone();
    }
    config.validate(source)?;
    Ok(config)
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
    fn reads_the_pisbx_section() {
        let config = p(r#"
[pisbx]
trust = true
[pisbx.env]
SSH_AUTH_SOCK = "~/Library/Group Containers/TEAM.app/t/agent.sock"
PI_CLIPBOARD_OSC52 = "1"
"#)
        .unwrap();
        assert!(config.pisbx.trust);
        assert_eq!(config.pisbx.env.len(), 2);
        assert!(
            config.rest.is_empty(),
            "pisbx settings must not be forwarded"
        );
        assert!(config.pisbx.unset.is_empty());
    }

    #[test]
    fn unset_variables_are_read() {
        let config = p("[pisbx]\nunset = [\"GITHUB_TOKEN\", \"AWS_SECRET_ACCESS_KEY\"]\n").unwrap();
        assert_eq!(
            config.pisbx.unset,
            ["GITHUB_TOKEN", "AWS_SECRET_ACCESS_KEY"]
        );
    }

    #[test]
    fn a_variable_cannot_be_set_and_unset() {
        let error = p("[pisbx]\nunset = [\"FOO\"]\n[pisbx.env]\nFOO = \"1\"\n").unwrap_err();
        assert!(error.contains("also set"), "{error}");
    }

    #[test]
    fn pisbx_names_cannot_be_shadowed() {
        let error = p("[pisbx.env]\nPISBX_ACTIVE = \"0\"\n").unwrap_err();
        assert!(error.contains("reserved"), "{error}");
        let error = p("[pisbx]\nunset = [\"PISBX_SETTINGS\"]\n").unwrap_err();
        assert!(error.contains("reserved"), "{error}");
    }

    #[test]
    fn unset_names_must_look_like_names() {
        let error = p("[pisbx]\nunset = [\"NOT A NAME\"]\n").unwrap_err();
        assert!(error.contains("NOT A NAME"), "{error}");
        let error = p("[pisbx]\nunset = [\"\"]\n").unwrap_err();
        assert!(
            error.contains("not an environment variable name"),
            "{error}"
        );
    }

    #[test]
    fn unknown_pisbx_keys_are_an_error() {
        let error = p("[pisbx]\ntrustt = true\n").unwrap_err();
        assert!(
            error.contains("trustt") || error.contains("unknown field"),
            "{error}"
        );
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
    fn reads_project_sections() {
        let config = p(r#"
[projects."~/Projects/foo".filesystem]
allowWrite = ["~/.cargo"]
denyWrite = [".env"]
allowGitConfig = true

[projects."~/Projects/foo".network]
allowedDomains = ["registry.npmjs.org"]
deniedDomains = ["*.internal.example"]
allowUnixSockets = ["/tmp/agent.sock"]
allowMachLookup = ["com.apple.pasteboard.1"]
"#)
        .unwrap();

        let project = config
            .projects
            .get("~/Projects/foo")
            .expect("project section");
        assert_eq!(project.filesystem.allow_write[0].text(), "~/.cargo");
        assert_eq!(project.filesystem.deny_write[0].text(), ".env");
        assert!(project.filesystem.allow_git_config);
        assert_eq!(project.network.allowed_domains, ["registry.npmjs.org"]);
        assert_eq!(project.network.denied_domains, ["*.internal.example"]);
        assert_eq!(
            project.network.allow_unix_sockets[0].text(),
            "/tmp/agent.sock"
        );
        assert_eq!(
            project.network.allow_mach_lookup,
            ["com.apple.pasteboard.1"]
        );
        assert!(config.rest.is_empty(), "projects must not be forwarded");
    }

    #[test]
    fn project_keys_must_be_absolute_or_tilde() {
        for key in ["relative/path", "../up", "~user/foo", "~/*"] {
            let error = p(&format!("[projects.{key:?}]\n")).unwrap_err();
            assert!(error.contains("project keys"), "{key}: {error}");
        }
    }

    #[test]
    fn unknown_project_keys_error_instead_of_forwarding() {
        let error = p("[projects.\"~/foo\"]\nallowWrite = [\"/x\"]\n").unwrap_err();
        assert!(
            error.contains("allowWrite") || error.contains("unknown field"),
            "{error}"
        );

        let error = p("[projects.\"~/foo\".network]\nallowLocalBinding = true\n").unwrap_err();
        assert!(
            error.contains("allowLocalBinding") || error.contains("unknown field"),
            "{error}"
        );
    }

    #[test]
    fn duplicate_project_keys_are_a_config_error() {
        let error = p("[projects.\"/tmp/pisbx-duplicate-project\"]\n\
             [projects.\"/tmp/pisbx-duplicate-project/\"]\n")
        .unwrap_err();
        assert!(error.contains("both resolve"), "{error}");
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
