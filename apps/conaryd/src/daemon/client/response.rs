// apps/conaryd/src/daemon/client/response.rs

//! Typed response media-type verification for the daemon client.
//!
//! The daemon serves successful results as `application/json`, RFC 7807
//! errors as `application/problem+json`, and SSE setup as
//! `text/event-stream`. Media types are parsed with the `mime` grammar and
//! verified before any body is deserialized, and response header reads are
//! bounded so a malformed stream cannot grow or block without limit.

use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::str::FromStr;
use std::time::{Duration, Instant};

/// Maximum number of response header lines accepted from the daemon.
pub(crate) const MAX_HEADER_LINES: usize = 128;
/// Maximum bytes accepted for a single response header line.
pub(crate) const MAX_HEADER_LINE_BYTES: usize = 8 * 1024;
/// Maximum total response header bytes accepted from the daemon.
pub(crate) const MAX_HEADER_BYTES: usize = 32 * 1024;

/// Fixed diagnostic for a response without a Content-Type header.
pub(crate) const MISSING_CONTENT_TYPE: &str = "daemon response is missing a Content-Type header";
/// Fixed diagnostic for an unparsable Content-Type header.
pub(crate) const MALFORMED_CONTENT_TYPE: &str =
    "daemon response has a malformed Content-Type header";
/// Fixed diagnostic for repeated Content-Type headers.
pub(crate) const DUPLICATE_CONTENT_TYPE: &str =
    "daemon response has duplicate Content-Type headers";
/// Fixed diagnostic for a response head that exceeds the accepted bounds.
pub(crate) const MALFORMED_HEADERS: &str = "daemon response headers are malformed or too large";

/// Media type the client expects for a response class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExpectedMediaType {
    /// Ordinary successful JSON result.
    Json,
    /// RFC 7807 error produced by `routes::errors`.
    ProblemJson,
    /// Server-sent event stream setup.
    EventStream,
}

impl ExpectedMediaType {
    /// Exact essence string expected for this response class.
    pub(crate) const fn essence(self) -> &'static str {
        match self {
            Self::Json => "application/json",
            Self::ProblemJson => "application/problem+json",
            Self::EventStream => "text/event-stream",
        }
    }
}

/// Parse one Content-Type value using the MIME grammar (parameters allowed).
pub(crate) fn parse_content_type(value: &str) -> Result<mime::Mime, &'static str> {
    // RFC 9110 sections 5.6.6 and 8.3.1 permit OWS before each semicolon
    // and empty parameter slots. mime 0.3.17 expects their canonical spelling.
    // Preserve quoted strings and quoted-pairs exactly; MIME remains the
    // grammar validator for the resulting type and parameter values.
    #[derive(Clone, Copy)]
    enum State {
        Bare,
        Quoted,
        Escaped,
    }
    let mut state = State::Bare;
    let mut start = 0;
    let mut parts = Vec::new();
    for (index, ch) in value.char_indices() {
        match (state, ch) {
            (State::Bare, ';') => {
                parts.push(&value[start..index]);
                start = index + 1;
            }
            (State::Bare, '"') => state = State::Quoted,
            (State::Quoted, '\\') => state = State::Escaped,
            (State::Quoted, '"') => state = State::Bare,
            (State::Escaped, _) => state = State::Quoted,
            _ => {}
        }
    }
    parts.push(&value[start..]);
    let mut parts = parts.into_iter().map(|part| part.trim_matches([' ', '\t']));
    let mut canonical = parts.next().unwrap_or_default().to_string();
    for parameter in parts.filter(|part| !part.is_empty()) {
        canonical.push(';');
        canonical.push_str(parameter);
    }
    mime::Mime::from_str(&canonical).map_err(|_| MALFORMED_CONTENT_TYPE)
}

/// Verify exactly one Content-Type header against the expected media type.
///
/// Header names are matched case-insensitively by the caller; the `mime`
/// parser normalizes type and subtype case, so `Application/JSON; charset="utf-8"`
/// satisfies [`ExpectedMediaType::Json`].
pub(crate) fn verify_content_type(
    values: &[String],
    expected: ExpectedMediaType,
) -> Result<(), String> {
    match values {
        [] => Err(MISSING_CONTENT_TYPE.to_string()),
        [value] => {
            let actual = parse_content_type(value).map_err(str::to_string)?;
            if actual.essence_str() == expected.essence() {
                Ok(())
            } else {
                Err(format!(
                    "daemon response Content-Type is not {}",
                    expected.essence()
                ))
            }
        }
        _ => Err(DUPLICATE_CONTENT_TYPE.to_string()),
    }
}

/// Read and parse a response status line plus headers from a buffered stream.
///
/// Reads at most [`MAX_HEADER_LINES`] lines and [`MAX_HEADER_BYTES`] total
/// bytes, and rejects any line that does not terminate within
/// [`MAX_HEADER_LINE_BYTES`]. Returns the status code and every
/// `Content-Type` value with the header name matched case-insensitively.
pub(crate) fn read_response_head<R: BufRead>(
    reader: &mut R,
    mut before_read: impl FnMut() -> Result<(), &'static str>,
) -> Result<(u16, Vec<String>), &'static str> {
    let mut bytes = Vec::new();
    for _ in 0..=MAX_HEADER_LINES {
        let mut line = String::new();
        read_line_bounded(reader, &mut line, &mut before_read)?;
        bytes.extend_from_slice(line.as_bytes());
        if bytes.len() > MAX_HEADER_BYTES {
            return Err(MALFORMED_HEADERS);
        }
        if line == "\r\n" || line == "\n" {
            let mut headers = [httparse::EMPTY_HEADER; MAX_HEADER_LINES];
            let mut response = httparse::Response::new(&mut headers);
            match response.parse(&bytes).map_err(|_| MALFORMED_HEADERS)? {
                httparse::Status::Complete(consumed) if consumed == bytes.len() => {}
                _ => return Err(MALFORMED_HEADERS),
            }
            let status = response.code.ok_or(MALFORMED_HEADERS)?;
            let values = response
                .headers
                .iter()
                .filter(|header| header.name.eq_ignore_ascii_case("content-type"))
                .map(|header| {
                    std::str::from_utf8(header.value)
                        .map(str::to_owned)
                        .map_err(|_| MALFORMED_CONTENT_TYPE)
                })
                .collect::<Result<Vec<_>, _>>()?;
            return Ok((status, values));
        }
    }
    Err(MALFORMED_HEADERS)
}

/// Bound the whole network header exchange, including a peer that sends a
/// trickle just before every individual socket-read timeout.
pub(crate) fn read_network_head(
    reader: &mut BufReader<UnixStream>,
    timeout: Duration,
) -> Result<(u16, Vec<String>), &'static str> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(MALFORMED_HEADERS)?;
    let timer = reader
        .get_ref()
        .try_clone()
        .map_err(|_| MALFORMED_HEADERS)?;
    read_response_head(reader, || {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or(MALFORMED_HEADERS)?;
        timer
            .set_read_timeout(Some(remaining))
            .map_err(|_| MALFORMED_HEADERS)
    })
}

/// Read one line, failing instead of buffering past the per-line bound.
fn read_line_bounded<R: BufRead>(
    reader: &mut R,
    out: &mut String,
    before_read: &mut impl FnMut() -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    let mut line = Vec::new();
    loop {
        before_read()?;
        let available = reader.fill_buf().map_err(|_| MALFORMED_HEADERS)?;
        if available.is_empty() {
            return Err(MALFORMED_HEADERS);
        }

        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        if line.len() + consumed > MAX_HEADER_LINE_BYTES {
            return Err(MALFORMED_HEADERS);
        }

        line.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);

        if newline.is_some() {
            let text = std::str::from_utf8(&line).map_err(|_| MALFORMED_HEADERS)?;
            out.push_str(text);
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    fn head(raw: &[u8]) -> Result<(u16, Vec<String>), &'static str> {
        let mut reader = BufReader::new(raw);
        read_response_head(&mut reader, || Ok(()))
    }

    #[test]
    fn accepts_valid_media_types_with_parameters_and_case() {
        let values = vec!["Application/JSON \t; ; charset=\"UTF-8\" ; note=\"a; b\";".to_string()];
        assert!(verify_content_type(&values, ExpectedMediaType::Json).is_ok());

        let values = vec!["Text/Event-Stream; charset=utf-8".to_string()];
        assert!(verify_content_type(&values, ExpectedMediaType::EventStream).is_ok());

        let values = vec!["application/problem+json".to_string()];
        assert!(verify_content_type(&values, ExpectedMediaType::ProblemJson).is_ok());
    }

    #[test]
    fn rejects_missing_malformed_duplicate_and_mismatched_types() {
        let missing = verify_content_type(&[], ExpectedMediaType::Json).unwrap_err();
        assert_eq!(missing, MISSING_CONTENT_TYPE);

        let malformed =
            verify_content_type(&["not a media type".to_string()], ExpectedMediaType::Json)
                .unwrap_err();
        assert_eq!(malformed, MALFORMED_CONTENT_TYPE);

        let duplicate = verify_content_type(
            &[
                "application/json".to_string(),
                "application/json".to_string(),
            ],
            ExpectedMediaType::Json,
        )
        .unwrap_err();
        assert_eq!(duplicate, DUPLICATE_CONTENT_TYPE);

        let mismatch =
            verify_content_type(&["text/plain".to_string()], ExpectedMediaType::Json).unwrap_err();
        assert_eq!(
            mismatch,
            "daemon response Content-Type is not application/json"
        );
    }

    #[test]
    fn parses_status_and_content_type_header_case_insensitively() {
        let raw = b"HTTP/1.1 200 OK\r\ncOnTeNt-TyPe: text/event-stream\r\n\r\n";
        let (status, content_types) = head(raw).unwrap();
        assert_eq!(status, 200);
        assert_eq!(content_types, vec!["text/event-stream".to_string()]);
    }

    #[test]
    fn rejects_header_line_without_terminator_within_bound() {
        let mut raw = b"HTTP/1.1 200 OK\r\nContent-Type: ".to_vec();
        raw.extend(std::iter::repeat_n(b'a', MAX_HEADER_LINE_BYTES));
        assert_eq!(head(&raw), Err(MALFORMED_HEADERS));
    }

    #[test]
    fn rejects_headers_without_terminating_blank_line() {
        let mut raw = b"HTTP/1.1 200 OK\r\n".to_vec();
        for _ in 0..=MAX_HEADER_LINES {
            raw.extend_from_slice(b"X-Pad: 1\r\n");
        }
        assert_eq!(head(&raw), Err(MALFORMED_HEADERS));
    }
}
