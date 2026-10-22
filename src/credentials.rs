//! A local proxy that holds the real secret so the sandbox never does.
//!
//! pi needs a credential to reach a model provider, and anything pi can read a
//! prompt-injected agent can read. So the sandbox gets a phantom token and a
//! base URL on localhost; this process, which runs outside the sandbox, swaps
//! the phantom for the real secret on the way out.
//!
//! Only API-key providers are covered. OAuth (pi's `openai-codex`, for one)
//! keeps refresh tokens in `auth.json` and is out of scope for a first cut.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::Arc;

/// One provider route: `/<name>/…` forwards to `upstream/…`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Route {
    /// Filled from the table key in the policy file.
    #[serde(default)]
    pub name: String,
    /// Scheme + host (+ optional base path), no trailing slash.
    pub upstream: String,
    /// `op://…`, `env:NAME` or `file:PATH`.
    pub secret: String,
    /// Header the value is injected into.
    #[serde(default = "default_header")]
    pub header: String,
    /// Prefix for the injected value, e.g. `Bearer `.
    #[serde(default = "default_prefix")]
    pub prefix: String,
    /// Environment variable pi should find the phantom in, e.g.
    /// `OPENAI_API_KEY`. Without it the phantom is only in `PISBX_PHANTOM`.
    #[serde(default)]
    pub env: Option<String>,
}

fn default_header() -> String {
    "authorization".to_string()
}

fn default_prefix() -> String {
    "Bearer ".to_string()
}

impl Route {
    pub fn validate(&self) -> Result<()> {
        if !self
            .name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        {
            bail!(
                "route name `{}` must be lowercase letters, digits, - or _",
                self.name
            );
        }
        if !(self.upstream.starts_with("http://") || self.upstream.starts_with("https://")) {
            bail!(
                "route `{}`: upstream must start with http:// or https://",
                self.name
            );
        }
        if self.secret.starts_with("op://")
            || self.secret.starts_with("env:")
            || self.secret.starts_with("file:")
        {
            Ok(())
        } else {
            bail!(
                "route `{}`: secret must be op://, env:NAME or file:PATH",
                self.name
            )
        }
    }
}

pub fn resolve_secret(spec: &str) -> Result<String> {
    let value = if let Some(reference) = spec.strip_prefix("op://") {
        let output = Command::new("op")
            .arg("read")
            .arg(format!("op://{reference}"))
            .stderr(Stdio::inherit())
            .output()
            .context("could not run `op read`")?;
        if !output.status.success() {
            bail!("`op read op://{reference}` failed");
        }
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    } else if let Some(name) = spec.strip_prefix("env:") {
        std::env::var(name).with_context(|| format!("environment variable {name} is not set"))?
    } else if let Some(path) = spec.strip_prefix("file:") {
        std::fs::read_to_string(path)
            .with_context(|| format!("could not read {path}"))?
            .trim()
            .to_string()
    } else {
        bail!("unsupported secret reference `{spec}`")
    };

    if value.is_empty() {
        bail!("secret `{spec}` resolved to nothing");
    }
    Ok(value)
}

fn token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    format!("pisbx_{:x}{:x}", nanos, std::process::id())
}

/// A running proxy. Dropping it stops accepting new connections; the session
/// ends with the process anyway.
pub struct Proxy {
    pub port: u16,
    pub phantom: String,
    routes: Arc<Vec<Route>>,
    secrets: Arc<HashMap<String, String>>,
}

impl Proxy {
    /// Start on a fixed port when the caller needs a stable base URL (a
    /// `models.json` entry cannot interpolate an ephemeral one).
    pub fn start_on(port: u16, routes: Vec<Route>) -> Result<Self> {
        for route in &routes {
            route.validate()?;
        }
        let mut secrets = HashMap::new();
        for route in &routes {
            secrets.insert(route.name.clone(), resolve_secret(&route.secret)?);
        }

        let listener = TcpListener::bind(("127.0.0.1", port))
            .with_context(|| format!("could not bind the credential proxy on port {port}"))?;
        let port = listener.local_addr()?.port();
        let proxy = Self {
            port,
            phantom: token(),
            routes: Arc::new(routes),
            secrets: Arc::new(secrets),
        };

        let routes = Arc::clone(&proxy.routes);
        let secrets = Arc::clone(&proxy.secrets);
        let phantom = proxy.phantom.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let routes = Arc::clone(&routes);
                let secrets = Arc::clone(&secrets);
                let phantom = phantom.clone();
                std::thread::spawn(move || {
                    // A broken connection is the client's problem, not ours.
                    let _ = handle(stream, &routes, &secrets, &phantom);
                });
            }
        });

        Ok(proxy)
    }

    pub fn base_url(&self, route: &str) -> String {
        format!("http://127.0.0.1:{}/{route}", self.port)
    }
}

fn respond_error(stream: &mut TcpStream, status: &str, message: &str) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{message}",
        message.len()
    )
}

fn handle(
    mut stream: TcpStream,
    routes: &[Route],
    secrets: &HashMap<String, String>,
    phantom: &str,
) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);

    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let (Some(method), Some(target), Some(_version)) = (parts.next(), parts.next(), parts.next())
    else {
        return respond_error(&mut stream, "400 Bad Request", "malformed request line")
            .map_err(Into::into);
    };

    let mut headers: Vec<(String, String)> = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }

    let header = |name: &str| {
        headers
            .iter()
            .rev()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };

    let path = target.trim_start_matches('/');
    let (route_name, rest) = path.split_once('/').unwrap_or((path, ""));
    let Some(route) = routes.iter().find(|route| route.name == route_name) else {
        return respond_error(&mut stream, "404 Not Found", "no such credential route")
            .map_err(Into::into);
    };

    // The sandbox holds the phantom, never the secret. A request without it is
    // refused rather than forwarded, so the proxy cannot be used as an
    // unauthenticated tunnel by anything else in the sandbox.
    let presented = header(&route.header).or_else(|| header("x-pisbx-token"));
    let authorised = presented
        .map(|value| value.contains(phantom))
        .unwrap_or(false);
    if !authorised {
        return respond_error(
            &mut stream,
            "403 Forbidden",
            "missing or wrong phantom token",
        )
        .map_err(Into::into);
    }

    let body = match header("content-length").and_then(|value| value.parse::<usize>().ok()) {
        Some(length) => {
            let mut buffer = vec![0u8; length];
            reader.read_exact(&mut buffer)?;
            Some(buffer)
        }
        None => None,
    };

    let url = if rest.is_empty() {
        route.upstream.clone()
    } else {
        format!("{}/{rest}", route.upstream.trim_end_matches('/'))
    };

    let mut curl = Command::new("curl");
    curl.arg("-sS")
        .arg("--http1.1")
        .arg("-i")
        .arg("--no-buffer")
        .arg("-X")
        .arg(method)
        .arg("-H")
        .arg(format!(
            "{}: {}{}",
            route.header,
            route.prefix,
            secrets.get(&route.name).cloned().unwrap_or_default()
        ));
    for (name, value) in &headers {
        let lower = name.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "host" | "authorization" | "content-length" | "connection" | "x-pisbx-token"
        ) || lower == route.header.to_ascii_lowercase()
        {
            continue;
        }
        curl.arg("-H").arg(format!("{name}: {value}"));
    }
    if body.is_some() {
        curl.arg("--data-binary").arg("@-");
    }
    curl.arg(&url);
    curl.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut child = curl.spawn().context("could not start curl")?;
    if let Some(body) = body {
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(&body)?;
        }
    }
    if let Some(mut stdout) = child.stdout.take() {
        let mut buffer = [0u8; 8192];
        loop {
            let read = stdout.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            stream.write_all(&buffer[..read])?;
            stream.flush()?;
        }
    }
    let status = child.wait()?;
    if !status.success() {
        // Nothing useful was streamed; say so rather than leaving the client
        // with a truncated response.
        println!("pisbx: credential route `{}` failed: {status}", route.name);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    /// A one-shot upstream: records the request head, answers 200 with "ok".
    fn upstream() -> (u16, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut head = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    head.push_str(&line);
                    if line == "\r\n" {
                        break;
                    }
                }
                tx.send(head).ok();
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok");
            }
        });
        (port, rx)
    }

    fn route(upstream: u16, secret: &str) -> Route {
        Route {
            name: "test".into(),
            upstream: format!("http://127.0.0.1:{upstream}"),
            secret: secret.into(),
            header: "authorization".into(),
            prefix: "Bearer ".into(),
            env: None,
        }
    }

    fn request(port: u16, path: &str, auth: Option<&str>) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let auth = auth
            .map(|value| format!("authorization: {value}\r\n"))
            .unwrap_or_default();
        write!(
            stream,
            "POST /{path} HTTP/1.1\r\nhost: 127.0.0.1\r\n{auth}content-type: application/json\r\ncontent-length: 2\r\n\r\n{{}}"
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).ok();
        response
    }

    #[test]
    fn resolves_env_and_file_secrets() {
        std::env::set_var("PISBX_TEST_SECRET", "s3cret");
        assert_eq!(resolve_secret("env:PISBX_TEST_SECRET").unwrap(), "s3cret");

        let path = std::env::temp_dir().join("pisbx-secret-test");
        std::fs::write(&path, "from-file\n").unwrap();
        assert_eq!(
            resolve_secret(&format!("file:{}", path.display())).unwrap(),
            "from-file"
        );
        std::fs::remove_file(path).ok();

        assert!(resolve_secret("plain-text").is_err());
    }

    #[test]
    fn injects_the_secret_and_rejects_a_missing_phantom() {
        let (upstream_port, requests) = upstream();
        let path = std::env::temp_dir().join("pisbx-secret-inject");
        std::fs::write(&path, "real-key").unwrap();

        let proxy = Proxy::start_on(
            0,
            vec![route(upstream_port, &format!("file:{}", path.display()))],
        )
        .unwrap();

        let denied = request(proxy.port, "test/v1/chat", Some("not-the-phantom"));
        assert!(denied.starts_with("HTTP/1.1 403"), "{denied}");

        let allowed = request(proxy.port, "test/v1/chat", Some(&proxy.phantom));
        assert!(allowed.contains("200 OK"), "{allowed}");
        assert!(allowed.ends_with("ok"), "{allowed}");

        let head = requests.recv().unwrap();
        assert!(
            head.to_ascii_lowercase()
                .contains("authorization: bearer real-key"),
            "{head}"
        );
        assert!(
            !head.contains(&proxy.phantom),
            "phantom leaked upstream: {head}"
        );

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn rejects_unknown_routes() {
        let proxy = Proxy::start_on(0, vec![route(9, "env:PATH")]).unwrap();
        let response = request(proxy.port, "nope/x", Some(&proxy.phantom));
        assert!(response.starts_with("HTTP/1.1 404"), "{response}");
    }

    #[test]
    fn validates_route_shape() {
        let bad_name = Route {
            name: "Bad Name".into(),
            ..route(1, "env:PATH")
        };
        assert!(bad_name.validate().is_err());

        let bad_secret = Route {
            secret: "plaintext".into(),
            ..route(1, "env:PATH")
        };
        assert!(bad_secret.validate().is_err());

        let bad_upstream = Route {
            upstream: "ftp://example".into(),
            ..route(1, "env:PATH")
        };
        assert!(bad_upstream.validate().is_err());
    }
}
