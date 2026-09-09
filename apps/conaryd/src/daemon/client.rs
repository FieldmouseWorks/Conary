// apps/conaryd/src/daemon/client.rs

//! Daemon client for CLI forwarding
//!
//! Provides a client that connects to the running daemon via Unix socket.
//! Used by CLI commands to forward operations when a daemon is running.
//!
//! # Example
//!
//! ```ignore
//! use crate::daemon::client::DaemonClient;
//! use crate::daemon::enhance::EnhanceJobSpec;
//!
//! // Try to connect to daemon
//! if let Ok(client) = DaemonClient::connect() {
//!     // Trigger background enhancement
//!     let spec = EnhanceJobSpec { batch_size: 10, ..Default::default() };
//!     let job = client.enhance(&spec)?;
//!     println!("Job queued: {}", job.job_id);
//!
//!     // Wait for completion with progress
//!     client.wait_for_job(&job.job_id, |event| {
//!         println!("{:?}", event);
//!     })?;
//! } else {
//!     // Daemon not running, execute directly
//!     // ...
//! }
//! ```
//!
//! Package mutation helpers queue daemon jobs. Non-dry-run package requests
//! must explicitly set `apply_intent`, matching the CLI's live-host
//! acknowledgement boundary.

use crate::daemon::{DaemonConfig, DaemonError, DaemonEvent};
use conary_core::Result;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

mod response;

/// Read timeout while waiting for the SSE response head.
const SSE_HEADER_TIMEOUT: Duration = Duration::from_secs(10);
/// Read timeout while consuming SSE events after a verified head.
const SSE_EVENT_TIMEOUT: Duration = Duration::from_secs(300);

/// Daemon client for connecting to conaryd
pub struct DaemonClient {
    /// Path to the Unix socket
    socket_path: PathBuf,
    /// Connection timeout
    timeout: Duration,
}

/// Response from creating a transaction
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CreateTransactionResponse {
    pub job_id: String,
    pub status: String,
    pub queue_position: usize,
    pub location: String,
}

/// Transaction details
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TransactionDetails {
    pub id: String,
    pub idempotency_key: Option<String>,
    pub kind: String,
    pub status: String,
    pub spec: serde_json::Value,
    pub result: Option<serde_json::Value>,
    pub error: Option<DaemonError>,
    pub requested_by_uid: Option<u32>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub queue_position: Option<usize>,
}

/// Options for package installation
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct InstallOptions {
    pub allow_downgrade: bool,
    pub skip_deps: bool,
    pub dry_run: bool,
    pub yes: bool,
    pub apply_intent: bool,
}

/// Options for package removal
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RemoveOptions {
    pub cascade: bool,
    pub remove_orphans: bool,
    pub purge: bool,
    pub apply_intent: bool,
}

/// Options for package updates
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct UpdateOptions {
    pub security_only: bool,
    pub dry_run: bool,
    pub yes: bool,
    pub apply_intent: bool,
}

/// HTTP response from daemon
struct HttpResponse {
    status_code: u16,
    /// Every `Content-Type` header value, in wire order.
    content_type: Vec<String>,
    body: String,
}

impl DaemonClient {
    /// Create a new client with default socket path
    pub fn new() -> Self {
        Self {
            socket_path: DaemonConfig::default_socket_path(),
            timeout: Duration::from_secs(30),
        }
    }

    /// Create a client with a custom socket path
    pub fn with_socket_path<P: AsRef<Path>>(socket_path: P) -> Self {
        Self {
            socket_path: socket_path.as_ref().to_path_buf(),
            timeout: Duration::from_secs(30),
        }
    }

    /// Set connection timeout
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Try to connect to the daemon
    ///
    /// Returns Ok(client) if daemon is running and accessible.
    /// Returns Err if daemon is not running or connection fails.
    pub fn connect() -> Result<Self> {
        let client = Self::new();
        client.check_connection()?;
        Ok(client)
    }

    /// Try to connect to a specific socket path
    pub fn connect_to<P: AsRef<Path>>(socket_path: P) -> Result<Self> {
        let client = Self::with_socket_path(socket_path);
        client.check_connection()?;
        Ok(client)
    }

    /// Check if the daemon is running and accessible
    pub fn check_connection(&self) -> Result<()> {
        let response = self.request("GET", "/health", None)?;
        if response.status_code == 200 {
            Ok(())
        } else {
            Err(conary_core::Error::IoError(format!(
                "Daemon health check failed with status {}",
                response.status_code
            )))
        }
    }

    /// Check if the daemon is running (without connecting)
    pub fn is_daemon_running(&self) -> bool {
        self.socket_path.exists() && self.check_connection().is_ok()
    }

    /// Install packages
    pub fn install(
        &self,
        packages: &[&str],
        options: InstallOptions,
    ) -> Result<CreateTransactionResponse> {
        let body = serde_json::json!({
            "packages": packages,
            "options": options
        });

        let response = self.request("POST", "/v1/packages/install", Some(&body.to_string()))?;

        self.parse_response(response)
    }

    /// Remove packages
    pub fn remove(
        &self,
        packages: &[&str],
        options: RemoveOptions,
    ) -> Result<CreateTransactionResponse> {
        let body = serde_json::json!({
            "packages": packages,
            "options": options
        });

        let response = self.request("POST", "/v1/packages/remove", Some(&body.to_string()))?;

        self.parse_response(response)
    }

    /// Update packages
    pub fn update(
        &self,
        packages: &[&str],
        options: UpdateOptions,
    ) -> Result<CreateTransactionResponse> {
        let body = serde_json::json!({
            "packages": packages,
            "options": options
        });

        let response = self.request("POST", "/v1/packages/update", Some(&body.to_string()))?;

        self.parse_response(response)
    }

    /// Trigger a background enhancement job
    ///
    /// Pass an `idempotency_key` to deduplicate retried requests.
    pub fn enhance(
        &self,
        spec: &crate::daemon::EnhanceJobSpec,
        idempotency_key: Option<&str>,
    ) -> Result<CreateTransactionResponse> {
        let body = serde_json::to_string(spec)
            .map_err(|e| conary_core::Error::IoError(format!("Serialization error: {e}")))?;

        let extra_headers: Vec<(&str, &str)> = idempotency_key
            .iter()
            .map(|k| ("X-Idempotency-Key", *k))
            .collect();

        let response =
            self.request_with_headers("POST", "/v1/enhance", Some(&body), &extra_headers)?;

        self.parse_response(response)
    }

    /// Get transaction details
    pub fn get_transaction(&self, job_id: &str) -> Result<TransactionDetails> {
        let response = self.request("GET", &format!("/v1/transactions/{}", job_id), None)?;

        self.parse_response(response)
    }

    /// Cancel a transaction
    pub fn cancel_transaction(&self, job_id: &str) -> Result<()> {
        let response = self.request("DELETE", &format!("/v1/transactions/{}", job_id), None)?;

        if response.status_code == 204 || response.status_code == 200 {
            Ok(())
        } else {
            self.parse_error(response)
        }
    }

    /// Wait for a job to complete, calling the callback with progress events
    ///
    /// Returns the final transaction details when the job completes.
    pub fn wait_for_job<F>(&self, job_id: &str, mut on_event: F) -> Result<TransactionDetails>
    where
        F: FnMut(DaemonEvent),
    {
        // Connect to SSE stream for this job
        let mut stream = UnixStream::connect(&self.socket_path)?;
        stream.set_read_timeout(Some(SSE_HEADER_TIMEOUT))?;

        // Send HTTP request for SSE
        let request = format!(
            "GET /v1/transactions/{}/stream HTTP/1.1\r\n\
             Host: localhost\r\n\
             Accept: text/event-stream\r\n\
             Cache-Control: no-cache\r\n\
             Connection: keep-alive\r\n\
             \r\n",
            job_id
        );
        stream.write_all(request.as_bytes())?;

        // Read the bounded response head and verify SSE setup before consuming
        // any event data, so a mismatch cannot reach the event callback.
        let mut reader = BufReader::new(stream);
        let (status_code, content_types) =
            response::read_network_head(&mut reader, self.timeout.min(SSE_HEADER_TIMEOUT))
                .map_err(|diagnostic| conary_core::Error::IoError(diagnostic.to_string()))?;

        if status_code != 200 {
            return Err(conary_core::Error::IoError(format!(
                "SSE stream failed with status {}",
                status_code
            )));
        }

        response::verify_content_type(&content_types, response::ExpectedMediaType::EventStream)
            .map_err(conary_core::Error::IoError)?;
        reader.get_ref().set_read_timeout(Some(SSE_EVENT_TIMEOUT))?;

        // Read SSE events
        let mut event_data = String::new();

        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break, // EOF
                Ok(_) => {
                    let line = line.trim_end();

                    if line.starts_with("event:") {
                        // SSE event type (unused; DaemonEvent is self-describing via serde tag)
                    } else if let Some(stripped) = line.strip_prefix("data:") {
                        event_data = stripped.trim().to_string();
                    } else if line.is_empty() && !event_data.is_empty() {
                        // Event complete, process it
                        if let Ok(event) = serde_json::from_str::<DaemonEvent>(&event_data) {
                            let is_terminal = matches!(
                                &event,
                                DaemonEvent::JobCompleted { .. }
                                    | DaemonEvent::JobFailed { .. }
                                    | DaemonEvent::JobCancelled { .. }
                            );

                            on_event(event);

                            if is_terminal {
                                break;
                            }
                        }

                        event_data.clear();
                    } else if line.starts_with(':') {
                        // Comment/keepalive, ignore
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // Timeout, check job status
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }

        // Get final transaction details
        self.get_transaction(job_id)
    }

    /// Poll for job completion (without SSE)
    ///
    /// Polls the job status at the given interval until it completes.
    pub fn poll_job(&self, job_id: &str, poll_interval: Duration) -> Result<TransactionDetails> {
        loop {
            let details = self.get_transaction(job_id)?;

            match details.status.as_str() {
                "completed" | "failed" | "cancelled" => {
                    return Ok(details);
                }
                _ => {
                    std::thread::sleep(poll_interval);
                }
            }
        }
    }

    /// Make an HTTP request to the daemon
    fn request(&self, method: &str, path: &str, body: Option<&str>) -> Result<HttpResponse> {
        self.request_with_headers(method, path, body, &[])
    }

    /// Make an HTTP request with optional extra headers
    fn request_with_headers(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
        extra_headers: &[(&str, &str)],
    ) -> Result<HttpResponse> {
        let mut stream = UnixStream::connect(&self.socket_path)?;
        stream.set_read_timeout(Some(self.timeout))?;
        stream.set_write_timeout(Some(self.timeout))?;

        // Build HTTP request
        let content_length = body.map_or(0, str::len);
        let mut request = format!(
            "{} {} HTTP/1.1\r\n\
             Host: localhost\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n",
            method, path, content_length
        );

        for (name, value) in extra_headers {
            request.push_str(&format!("{}: {}\r\n", name, value));
        }

        request.push_str("\r\n");

        if let Some(body) = body {
            request.push_str(body);
        }

        // Send request
        stream.write_all(request.as_bytes())?;

        // Parse the bounded HTTP head before reading or decoding its body.
        let mut reader = BufReader::new(stream);
        let (status_code, content_type) = response::read_network_head(&mut reader, self.timeout)
            .map_err(|diagnostic| conary_core::Error::IoError(diagnostic.to_string()))?;
        reader.get_ref().set_read_timeout(Some(self.timeout))?;
        let mut body = String::new();
        reader.read_to_string(&mut body)?;
        Ok(HttpResponse {
            status_code,
            content_type,
            body,
        })
    }

    /// Parse successful response body
    fn parse_response<T: serde::de::DeserializeOwned>(&self, response: HttpResponse) -> Result<T> {
        if response.status_code >= 200 && response.status_code < 300 {
            response::verify_content_type(
                &response.content_type,
                response::ExpectedMediaType::Json,
            )
            .map_err(conary_core::Error::IoError)?;
            serde_json::from_str(&response.body).map_err(|e| {
                conary_core::Error::IoError(format!("Failed to parse response: {}", e))
            })
        } else {
            self.parse_error(response)
        }
    }

    /// Parse error response
    fn parse_error<T>(&self, response: HttpResponse) -> Result<T> {
        // Daemon errors are RFC 7807 problem+json; verify before deserializing.
        response::verify_content_type(
            &response.content_type,
            response::ExpectedMediaType::ProblemJson,
        )
        .map_err(conary_core::Error::IoError)?;

        if let Ok(error) = serde_json::from_str::<DaemonError>(&response.body) {
            Err(conary_core::Error::IoError(format!(
                "Daemon error ({}): {}",
                error.status, error.detail
            )))
        } else {
            Err(conary_core::Error::IoError(format!(
                "Daemon error response body is not valid problem JSON (status {})",
                response.status_code
            )))
        }
    }
}

impl Default for DaemonClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Check if the daemon is running and return a client if so
///
/// This is a convenience function for CLI commands to check if they
/// should forward to the daemon or execute directly.
pub fn try_connect() -> Option<DaemonClient> {
    DaemonClient::connect().ok()
}

/// Check if we should forward to the daemon
///
/// Returns true if:
/// - The daemon is running
/// - We're not already the daemon process
/// - The CONARY_NO_DAEMON env var is not set
pub fn should_forward_to_daemon() -> bool {
    // Don't forward if env var is set
    if std::env::var("CONARY_NO_DAEMON").is_ok() {
        return false;
    }

    // Check if daemon socket exists and is accessible
    let client = DaemonClient::new();
    client.is_daemon_running()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn response(status_code: u16, content_type: &[&str], body: &str) -> HttpResponse {
        HttpResponse {
            status_code,
            content_type: content_type.iter().map(|value| value.to_string()).collect(),
            body: body.to_string(),
        }
    }

    /// Serve the given raw HTTP responses on a Unix socket, one per accept.
    fn serve_responses(socket_path: PathBuf, responses: Vec<String>) {
        let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
        std::thread::spawn(move || {
            for raw in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut request = [0u8; 1024];
                let _ = stream.read(&mut request);
                let _ = stream.write_all(raw.as_bytes());
            }
        });
    }

    #[test]
    fn parse_response_accepts_json_with_parameters_and_case() {
        let client = DaemonClient::new();
        let parsed: CreateTransactionResponse = client
            .parse_response(response(
                200,
                &["Application/JSON; charset=\"UTF-8\""],
                r#"{"job_id":"job-1","status":"queued","queue_position":0,"location":"/v1/transactions/job-1"}"#,
            ))
            .unwrap();
        assert_eq!(parsed.job_id, "job-1");
    }

    #[test]
    fn parse_response_rejects_missing_malformed_duplicate_and_mismatched_types() {
        let client = DaemonClient::new();
        let cases: [(&[&str], &str); 4] = [
            (&[], "daemon response is missing a Content-Type header"),
            (
                &["not a media type"],
                "daemon response has a malformed Content-Type header",
            ),
            (
                &["application/json", "application/json"],
                "daemon response has duplicate Content-Type headers",
            ),
            (
                &["text/plain"],
                "daemon response Content-Type is not application/json",
            ),
        ];

        for (content_type, expected) in cases {
            let error = client
                .parse_response::<CreateTransactionResponse>(response(200, content_type, "{}"))
                .unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[test]
    fn parse_error_accepts_problem_json() {
        let client = DaemonClient::new();
        let error = client
            .parse_error::<CreateTransactionResponse>(response(
                400,
                &["application/problem+json"],
                r#"{"type":"urn:conary:error:bad_request","title":"Bad Request","status":400,"detail":"bad spec"}"#,
            ))
            .unwrap_err();
        assert!(error.to_string().contains("Daemon error (400): bad spec"));
    }

    #[test]
    fn parse_error_rejects_non_problem_json() {
        let client = DaemonClient::new();
        let error = client
            .parse_error::<CreateTransactionResponse>(response(500, &["text/plain"], "boom"))
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("daemon response Content-Type is not application/problem+json"),
            "{error}"
        );
    }

    #[test]
    fn cancel_transaction_accepts_empty_204_without_content_type() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("daemon.sock");
        serve_responses(
            socket_path.clone(),
            vec!["HTTP/1.1 204 No Content\r\n\r\n".to_string()],
        );

        let client = DaemonClient::with_socket_path(&socket_path);
        assert!(client.cancel_transaction("job-1").is_ok());
    }

    #[test]
    fn wait_for_job_streams_events_after_verified_event_stream_head() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("daemon.sock");
        let sse = concat!(
            "HTTP/1.1 200 OK\r\n",
            "Content-Type: text/event-stream\r\n",
            "\r\n",
            "data: {\"type\":\"job_completed\",\"job_id\":\"job-1\",\"duration_ms\":5}\n\n"
        );
        let details = concat!(
            "HTTP/1.1 200 OK\r\n",
            "Content-Type: application/json\r\n",
            "\r\n",
            r#"{"id":"job-1","idempotency_key":null,"kind":"install","status":"completed","spec":{},"result":null,"error":null,"requested_by_uid":null,"created_at":"2026-01-01T00:00:00Z","started_at":null,"completed_at":null,"queue_position":null}"#
        );
        serve_responses(
            socket_path.clone(),
            vec![sse.to_string(), details.to_string()],
        );

        let client = DaemonClient::with_socket_path(&socket_path);
        let events = AtomicUsize::new(0);
        let details = client
            .wait_for_job("job-1", |_| {
                events.fetch_add(1, Ordering::Relaxed);
            })
            .unwrap();

        assert_eq!(events.load(Ordering::Relaxed), 1);
        assert_eq!(details.id, "job-1");
    }

    #[test]
    fn wait_for_job_rejects_non_event_stream_before_callback() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("daemon.sock");
        let raw = concat!(
            "HTTP/1.1 200 OK\r\n",
            "Content-Type: application/json\r\n",
            "\r\n",
            "data: {\"type\":\"job_completed\",\"job_id\":\"job-1\",\"duration_ms\":5}\n\n"
        );
        serve_responses(socket_path.clone(), vec![raw.to_string()]);

        let client = DaemonClient::with_socket_path(&socket_path);
        let events = AtomicUsize::new(0);
        let error = client
            .wait_for_job("job-1", |_| {
                events.fetch_add(1, Ordering::Relaxed);
            })
            .unwrap_err();

        assert_eq!(events.load(Ordering::Relaxed), 0);
        assert!(
            error
                .to_string()
                .contains("daemon response Content-Type is not text/event-stream"),
            "{error}"
        );
    }

    #[test]
    fn test_client_creation_uses_dedicated_daemon_socket_default() {
        let client = DaemonClient::new();
        assert_eq!(client.socket_path, DaemonConfig::default_socket_path());
    }

    #[test]
    fn test_client_with_custom_path() {
        let client = DaemonClient::with_socket_path("/tmp/test.sock");
        assert_eq!(client.socket_path, PathBuf::from("/tmp/test.sock"));
    }

    #[test]
    fn test_client_with_timeout() {
        let client = DaemonClient::new().with_timeout(Duration::from_secs(60));
        assert_eq!(client.timeout, Duration::from_secs(60));
    }

    #[test]
    fn test_install_options_default() {
        let options = InstallOptions::default();
        assert!(!options.allow_downgrade);
        assert!(!options.skip_deps);
    }

    #[test]
    fn test_remove_options_default() {
        let options = RemoveOptions::default();
        assert!(!options.cascade);
        assert!(!options.remove_orphans);
    }

    #[test]
    fn test_should_forward_with_env_var() {
        // Save current value
        let prev = std::env::var("CONARY_NO_DAEMON").ok();

        // Set env var
        // SAFETY: Tests are run single-threaded by default, env manipulation is safe
        unsafe {
            std::env::set_var("CONARY_NO_DAEMON", "1");
        }
        assert!(!should_forward_to_daemon());

        // Restore
        // SAFETY: Tests are run single-threaded by default, env manipulation is safe
        unsafe {
            if let Some(val) = prev {
                std::env::set_var("CONARY_NO_DAEMON", val);
            } else {
                std::env::remove_var("CONARY_NO_DAEMON");
            }
        }
    }
}
