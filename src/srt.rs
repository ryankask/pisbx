//! Locating and invoking srt.
//!
//! srt owns the sandbox mechanics: the Seatbelt profile, the localhost proxy
//! and the domain checks. pisbx only decides what to ask for and runs it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

/// First `srt` on `PATH`.
pub fn locate(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

/// srt filters egress by pointing `HTTP(S)_PROXY` at its localhost proxy and
/// denying direct sockets. curl and git honour those variables; Node's built-in
/// `fetch` does not, so inside the sandbox every request resolves the name
/// itself and fails with `ENOTFOUND` even when the host is allowlisted.
/// `NODE_USE_ENV_PROXY` (Node 24+) makes fetch use the proxy env like curl does.
const NODE_ENV_PROXY: (&str, &str) = ("NODE_USE_ENV_PROXY", "1");

/// Run srt with inherited stdio, adding the environment the policy asked for.
/// srt passes its own environment on to the sandboxed command, which is how
/// `[pisbx.env]` reaches pi.
pub fn run(
    settings: &Path,
    args: &[OsString],
    env: &[(String, String)],
) -> std::io::Result<ExitStatus> {
    let mut command = std::process::Command::new("srt");
    command.env(NODE_ENV_PROXY.0, NODE_ENV_PROXY.1);
    command.arg("-s").arg(settings).arg("--");
    command.args(args);
    command.envs(env.iter().map(|(key, value)| (key, value)));
    command.status()
}

/// Run a shell snippet inside the sandbox and return its combined output.
pub fn capture(settings: &Path, cwd: &Path, script: &str) -> std::io::Result<String> {
    let output = std::process::Command::new("srt")
        .env(NODE_ENV_PROXY.0, NODE_ENV_PROXY.1)
        .arg("-s")
        .arg(settings)
        .arg("-c")
        .arg(script)
        .current_dir(cwd)
        .output()?;

    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_program_on_path() {
        // `sh` exists on every macOS install; the lookup must not care about
        // the absolute location.
        let found = locate("sh").expect("sh on PATH");
        assert!(found.is_absolute(), "{found:?}");
    }

    #[test]
    fn missing_programs_are_none() {
        assert!(locate("pisbx-definitely-not-installed").is_none());
    }

    #[test]
    fn nonexecutables_are_not_candidates() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("pisbx-exec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let plain = dir.join("plain");
        std::fs::write(&plain, "").unwrap();
        assert!(!is_executable(&plain));

        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(is_executable(&plain));

        std::fs::remove_dir_all(&dir).ok();
    }
}
