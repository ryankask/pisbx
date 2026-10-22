//! The optional config file.
//!
//! TOML, read from `$HOME`, never from the workspace: a cloned repository must
//! not be able to widen its own sandbox. Keys match the flags, so there is one
//! vocabulary to remember.

use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const DEFAULT_PATH: &str = ".config/pisbx/config.toml";

#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    #[serde(default)]
    pub allow_write: Vec<String>,
    #[serde(default)]
    pub allow_read: Vec<String>,
    #[serde(default)]
    pub deny_read: Vec<String>,
    #[serde(default)]
    pub deny_write: Vec<String>,
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub denied_domains: Vec<String>,
    #[serde(default)]
    pub mach_lookup: Vec<String>,
    #[serde(default)]
    pub allow_unix_sockets: Vec<String>,
    #[serde(default)]
    pub allow_git_config: bool,
    /// Raw srt settings, merged add-only into the generated document.
    #[serde(default)]
    pub srt: Option<toml::Value>,
}

pub fn default_path() -> Option<PathBuf> {
    crate::paths::home().map(|home| home.join(DEFAULT_PATH))
}

/// Parse a config file. Unknown keys are an error rather than a silent no-op:
/// a typo in a security policy should be loud.
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
        assert_eq!(p("").unwrap(), Config::default());
        assert!(!p("").unwrap().allow_git_config);
        assert_eq!(p("# only a comment\n").unwrap(), Config::default());
    }

    #[test]
    fn reads_every_key() {
        let config = p(r#"
allow-write = ["~/scratch", "/tmp/artifacts"]
allow-read = ["~/keys"]
deny-read = ["~/vault"]
deny-write = ["~/scratch/.git"]
domains = ["api.openai.com", "*.openai.com"]
denied-domains = ["evil.example"]
mach-lookup = ["com.example.notes.*"]
"#)
        .unwrap();

        assert_eq!(config.allow_write, ["~/scratch", "/tmp/artifacts"]);
        assert_eq!(config.allow_read, ["~/keys"]);
        assert_eq!(config.deny_read, ["~/vault"]);
        assert_eq!(config.deny_write, ["~/scratch/.git"]);
        assert_eq!(config.domains, ["api.openai.com", "*.openai.com"]);
        assert_eq!(config.denied_domains, ["evil.example"]);
        assert_eq!(config.mach_lookup, ["com.example.notes.*"]);
    }

    #[test]
    fn reads_the_srt_passthrough_table() {
        let config = p(r#"
[srt]
allowPty = true

[srt.network]
allowLocalBinding = true
deniedResolvedAddresses = ["10.0.0.0/8"]

[srt.network.deniedDomainReasons]
"github.com:22" = "SSH pushes are blocked; use an https remote"
"#)
        .unwrap();
        let srt = config.srt.expect("passthrough table");
        assert_eq!(
            srt["network"]["allowLocalBinding"],
            toml::Value::Boolean(true)
        );
        assert_eq!(
            srt["network"]["deniedDomainReasons"]["github.com:22"],
            toml::Value::String("SSH pushes are blocked; use an https remote".into())
        );
    }

    #[test]
    fn unknown_keys_fail_loudly() {
        let error = p("allow-writ = [\"/a\"]\n").unwrap_err();
        assert!(error.contains("allow-writ"), "{error}");
        assert!(error.contains("test.toml"), "{error}");
    }

    #[test]
    fn malformed_toml_reports_a_location() {
        let error = p("allow-write = [\n").unwrap_err();
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
