//! HTTP transport for model calls.
//!
//! The project deliberately shells out to the system `curl` instead of pulling in an HTTP
//! client (the same choice VOICEVOX integration makes), which also keeps the whole model layer
//! testable: `HttpTransport` is a trait, so tests inject a fake transport and never touch the
//! network.
//!
//! Credentials are never placed in the process argument list: when an `Authorization` header is
//! present every header is written to a private (0600) file and passed as `-H @file`, because
//! `ps` would otherwise expose the API key to other users on the machine.

use super::ModelError;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

impl HttpResponse {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Minimal HTTP client interface. Implementations must be thread-safe because model calls run on
/// worker threads.
pub trait HttpTransport: Send + Sync {
    fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[String],
        body: Option<&str>,
        timeout_seconds: u32,
    ) -> Result<HttpResponse, ModelError>;
}

/// `curl`-backed transport used in production.
#[derive(Debug, Default)]
pub struct CurlTransport;

impl HttpTransport for CurlTransport {
    fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[String],
        body: Option<&str>,
        timeout_seconds: u32,
    ) -> Result<HttpResponse, ModelError> {
        let unique = Uuid::new_v4();
        let scratch = std::env::temp_dir();
        let body_path = scratch.join(format!("yuki-model-body-{unique}"));
        let timeout = timeout_seconds.clamp(1, 600).to_string();

        let mut command = Command::new("curl");
        command.args([
            "-sS",
            "--connect-timeout",
            "5",
            "--max-time",
            &timeout,
            // curl adds `Expect: 100-continue` for bodies over ~1KB; some proxies mishandle it and
            // stall until the timeout. Sending it empty disables the wait.
            "-H",
            "Expect:",
            "-X",
            method,
            "-o",
        ]);
        command.arg(&body_path);
        command.args(["-w", "%{http_code}"]);

        let secret_header = headers
            .iter()
            .any(|header| header.to_ascii_lowercase().starts_with("authorization:"));
        let header_file = if secret_header {
            let path = scratch.join(format!("yuki-model-headers-{unique}"));
            write_private_file(&path, &headers.join("\n"))?;
            command.arg("-H").arg(format!("@{}", path.display()));
            Some(path)
        } else {
            for header in headers {
                command.arg("-H").arg(header);
            }
            None
        };

        if body.is_some() {
            command.args(["--data-binary", "@-"]);
        }
        command.arg(url);
        command
            .stdin(if body.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn()?;
        if let Some(body) = body
            && let Some(mut stdin) = child.stdin.take()
        {
            // Dropping `stdin` at the end of the block marks end-of-input for curl.
            stdin.write_all(body.as_bytes())?;
        }
        let output = child.wait_with_output()?;

        let response_body = std::fs::read_to_string(&body_path).unwrap_or_default();
        let _ = std::fs::remove_file(&body_path);
        if let Some(path) = header_file {
            let _ = std::fs::remove_file(path);
        }

        if !output.status.success() {
            let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            return Err(ModelError::Connection(if message.is_empty() {
                format!("curl exited with {}", output.status)
            } else {
                message
            }));
        }
        let status = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u16>()
            .unwrap_or(0);
        Ok(HttpResponse {
            status,
            body: response_body,
        })
    }
}

#[cfg(unix)]
fn write_private_file(path: &PathBuf, contents: &str) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents.as_bytes())
}

#[cfg(not(unix))]
fn write_private_file(path: &PathBuf, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn private_header_file_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("yuki-header-test-{}", Uuid::new_v4()));
        write_private_file(&path, "Authorization: Bearer secret").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn success_range_is_detected() {
        assert!(
            HttpResponse {
                status: 200,
                body: String::new()
            }
            .is_success()
        );
        assert!(
            !HttpResponse {
                status: 429,
                body: String::new()
            }
            .is_success()
        );
    }
}
