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

/// Run srt with inherited stdio.
pub fn run(settings: &Path, args: &[OsString]) -> std::io::Result<ExitStatus> {
    let mut command = std::process::Command::new("srt");
    command.arg("-s").arg(settings).arg("--");
    command.args(args);
    command.status()
}

/// Run a shell snippet inside the sandbox and return its combined output.
pub fn capture(settings: &Path, cwd: &Path, script: &str) -> std::io::Result<String> {
    let output = std::process::Command::new("srt")
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
