// apps/conaryd/src/daemon/client/body.rs

//! RFC 9112 response body framing for the daemon client.
//!
//! [`BodyFraming`] is the typed framing decision derived from a parsed
//! response head; [`BodyReader`] decodes exactly that framing from a buffered
//! stream and never emits chunk metadata to its caller. Chunk sizes, chunk
//! extensions, and trailer sections are validated with typed grammars
//! (RFC 9112 sections 7.1 and 7.1.2) under fixed byte and line bounds, so a
//! peer cannot grow metadata without limit or re-frame the body.
//!
//! Fixed-length and bodyless readers finish after exactly the declared bytes
//! and never wait for connection EOF or consume a following message;
//! close-delimited readers end at connection EOF.

use std::io::{self, BufRead, Read};

/// Maximum bytes accepted for one chunk-size line, including its CRLF.
pub(crate) const MAX_CHUNK_SIZE_LINE_BYTES: usize = 8 * 1024;
/// Maximum bytes accepted for one trailer field line, including its CRLF.
pub(crate) const MAX_TRAILER_LINE_BYTES: usize = 8 * 1024;
/// Maximum total trailer section bytes, including the terminating CRLF.
pub(crate) const MAX_TRAILER_BYTES: usize = 32 * 1024;
/// Maximum trailer field lines accepted after the last chunk.
pub(crate) const MAX_TRAILER_LINES: usize = 128;

/// Fixed diagnostic for a Content-Length value outside the decimal grammar.
pub(crate) const MALFORMED_CONTENT_LENGTH: &str =
    "daemon response Content-Length is not a decimal byte count";
/// Fixed diagnostic for a Content-Length value larger than `u64::MAX`.
pub(crate) const OVERFLOWING_CONTENT_LENGTH: &str = "daemon response Content-Length overflows u64";
/// Fixed diagnostic for Content-Length values that disagree.
pub(crate) const CONFLICTING_CONTENT_LENGTH: &str =
    "daemon response Content-Length values conflict";
/// Fixed diagnostic for a Transfer-Encoding that is not one `chunked` coding.
pub(crate) const UNSUPPORTED_TRANSFER_CODING: &str =
    "daemon response Transfer-Encoding is not a single chunked coding";
/// Fixed diagnostic for a response that carries both length and coding framing.
pub(crate) const TRANSFER_ENCODING_WITH_CONTENT_LENGTH: &str =
    "daemon response carries both Transfer-Encoding and Content-Length";
/// Fixed diagnostic for a chunk-size line outside the hex grammar.
pub(crate) const MALFORMED_CHUNK_SIZE: &str = "daemon response chunk size is malformed";
/// Fixed diagnostic for a chunk size larger than `u64::MAX`.
pub(crate) const OVERFLOWING_CHUNK_SIZE: &str = "daemon response chunk size overflows u64";
/// Fixed diagnostic for a chunk extension outside the typed extension grammar.
pub(crate) const MALFORMED_CHUNK_EXTENSION: &str = "daemon response chunk extension is malformed";
/// Fixed diagnostic for chunk data not followed by CRLF.
pub(crate) const MALFORMED_CHUNK_TERMINATOR: &str =
    "daemon response chunk data is not terminated by CRLF";
/// Fixed diagnostic for a trailer section outside its grammar or bounds.
pub(crate) const MALFORMED_TRAILER: &str = "daemon response trailer section is malformed";
/// Fixed diagnostic for a trailer that tries to re-frame the message.
pub(crate) const TRAILER_FRAMING_FIELD: &str =
    "daemon response trailer carries a framing or Content-Type field";
/// Fixed diagnostic for a body that ended before its framing was satisfied.
pub(crate) const PREMATURE_BODY_EOF: &str =
    "daemon response body ended before its declared framing";

/// How the response body is delimited on the wire (RFC 9112 section 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyFraming {
    /// No body: informational, 204, and 304 responses.
    Empty,
    /// Exactly `n` body bytes; the message ends there.
    Fixed(u64),
    /// `chunked` transfer coding with an optional trailer section.
    Chunked,
    /// Body ends at connection close.
    CloseDelimited,
}

/// Derive the body framing from parsed response headers.
///
/// Bodyless statuses short-circuit to [`BodyFraming::Empty`]. Otherwise a
/// declared `Content-Length` (repeated identical decimal values, including
/// comma lists, are accepted) wins, a single `chunked` transfer coding means
/// [`BodyFraming::Chunked`], and the absence of both means the body is
/// close-delimited. Malformed, overflowing, conflicting, or doubly-declared
/// framing is rejected with a fixed diagnostic.
pub(crate) fn derive_framing(
    status: u16,
    headers: &[httparse::Header<'_>],
) -> Result<BodyFraming, &'static str> {
    if is_bodyless(status) {
        return Ok(BodyFraming::Empty);
    }

    let mut content_length: Option<u64> = None;
    let mut transfer_encoding = false;
    for header in headers {
        if header.name.eq_ignore_ascii_case("content-length") {
            let value = header_text(header.value)?;
            let declared = parse_content_length(value)?;
            match content_length {
                None => content_length = Some(declared),
                Some(previous) if previous == declared => {}
                Some(_) => return Err(CONFLICTING_CONTENT_LENGTH),
            }
        } else if header.name.eq_ignore_ascii_case("transfer-encoding") {
            if transfer_encoding {
                return Err(UNSUPPORTED_TRANSFER_CODING);
            }
            transfer_encoding = true;
            let value =
                std::str::from_utf8(header.value).map_err(|_| UNSUPPORTED_TRANSFER_CODING)?;
            parse_transfer_encoding(value)?;
        }
    }

    match (transfer_encoding, content_length) {
        (true, Some(_)) => Err(TRANSFER_ENCODING_WITH_CONTENT_LENGTH),
        (true, None) => Ok(BodyFraming::Chunked),
        (false, Some(length)) => Ok(BodyFraming::Fixed(length)),
        (false, None) => Ok(BodyFraming::CloseDelimited),
    }
}

/// Statuses whose response ends at the end of the header section.
const fn is_bodyless(status: u16) -> bool {
    matches!(status, 100..=199 | 204 | 304)
}

fn header_text(value: &[u8]) -> Result<&str, &'static str> {
    std::str::from_utf8(value).map_err(|_| MALFORMED_CONTENT_LENGTH)
}

/// Parse one `Content-Length` field value: a decimal byte count, or a comma
/// list of byte counts that must all be identical (RFC 9110 section 8.6).
fn parse_content_length(value: &str) -> Result<u64, &'static str> {
    let mut declared: Option<u64> = None;
    for element in value.split(',') {
        let element = element.trim_matches([' ', '\t']);
        if element.is_empty() || !element.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(MALFORMED_CONTENT_LENGTH);
        }
        let parsed = element
            .parse::<u64>()
            .map_err(|_| OVERFLOWING_CONTENT_LENGTH)?;
        match declared {
            None => declared = Some(parsed),
            Some(previous) if previous == parsed => {}
            Some(_) => return Err(CONFLICTING_CONTENT_LENGTH),
        }
    }
    declared.ok_or(MALFORMED_CONTENT_LENGTH)
}

/// Accept exactly one `chunked` transfer coding, case-insensitively.
fn parse_transfer_encoding(value: &str) -> Result<(), &'static str> {
    let mut codings = value
        .split(',')
        .map(|coding| coding.trim_matches([' ', '\t']));
    match codings.next() {
        Some(first) if first.eq_ignore_ascii_case("chunked") => {}
        _ => return Err(UNSUPPORTED_TRANSFER_CODING),
    }
    if codings.next().is_some() {
        return Err(UNSUPPORTED_TRANSFER_CODING);
    }
    Ok(())
}

/// Streaming decoder for one response body.
///
/// `Read` yields payload bytes only: chunk sizes, chunk extensions, CRLFs,
/// and trailer lines are consumed and validated internally, never returned to
/// the caller.
pub(crate) struct BodyReader<R: BufRead> {
    reader: R,
    state: FramingState,
    trailer_bytes: Vec<u8>,
    trailer_lines: usize,
}

/// Small, copyable framing state so `Read` can decode without borrowing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FramingState {
    Done,
    Fixed { remaining: u64 },
    CloseDelimited,
    ChunkSize,
    ChunkData { remaining: u64 },
    ChunkTerminator,
    Trailers,
}

impl<R: BufRead> BodyReader<R> {
    /// Wrap `reader`, which must be positioned at the first body byte.
    pub(crate) fn new(reader: R, framing: BodyFraming) -> Self {
        let state = match framing {
            BodyFraming::Empty | BodyFraming::Fixed(0) => FramingState::Done,
            BodyFraming::Fixed(remaining) => FramingState::Fixed { remaining },
            BodyFraming::Chunked => FramingState::ChunkSize,
            BodyFraming::CloseDelimited => FramingState::CloseDelimited,
        };
        Self {
            reader,
            state,
            trailer_bytes: Vec::new(),
            trailer_lines: 0,
        }
    }

    /// Read one line, bounded to `max` bytes including its terminating LF.
    fn read_line_bounded(&mut self, max: usize, over_bound: &'static str) -> io::Result<Vec<u8>> {
        let mut line = Vec::new();
        loop {
            let available = match self.reader.fill_buf() {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if available.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    PREMATURE_BODY_EOF,
                ));
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let consumed = newline.map_or(available.len(), |index| index + 1);
            if line.len() + consumed > max {
                return Err(invalid_framing(over_bound));
            }
            line.extend_from_slice(&available[..consumed]);
            self.reader.consume(consumed);
            if newline.is_some() {
                return Ok(line);
            }
        }
    }

    /// Consume one chunk-size line and advance to the chunk data or trailers.
    fn read_chunk_size_line(&mut self) -> io::Result<()> {
        let line = self.read_line_bounded(MAX_CHUNK_SIZE_LINE_BYTES, MALFORMED_CHUNK_SIZE)?;
        let size = parse_chunk_size_line(&line).map_err(invalid_framing)?;
        self.state = if size == 0 {
            FramingState::Trailers
        } else {
            FramingState::ChunkData { remaining: size }
        };
        Ok(())
    }

    /// Consume the trailer section, validate it with `httparse`, and finish.
    fn read_trailers(&mut self) -> io::Result<()> {
        loop {
            let line = self.read_line_bounded(MAX_TRAILER_LINE_BYTES, MALFORMED_TRAILER)?;
            if !line.ends_with(b"\r\n") {
                return Err(invalid_framing(MALFORMED_TRAILER));
            }
            if self.trailer_bytes.len() + line.len() > MAX_TRAILER_BYTES {
                return Err(invalid_framing(MALFORMED_TRAILER));
            }
            self.trailer_bytes.extend_from_slice(&line);
            if line == b"\r\n" {
                validate_trailer_section(&self.trailer_bytes).map_err(invalid_framing)?;
                self.trailer_bytes.clear();
                self.state = FramingState::Done;
                return Ok(());
            }
            if self.trailer_lines >= MAX_TRAILER_LINES {
                return Err(invalid_framing(MALFORMED_TRAILER));
            }
            self.trailer_lines += 1;
        }
    }
}

impl<R: BufRead> Read for BodyReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            match self.state {
                FramingState::Done => return Ok(0),
                FramingState::CloseDelimited => {
                    let available = self.reader.fill_buf()?;
                    if available.is_empty() {
                        self.state = FramingState::Done;
                        return Ok(0);
                    }
                    let take = available.len().min(buf.len());
                    buf[..take].copy_from_slice(&available[..take]);
                    self.reader.consume(take);
                    return Ok(take);
                }
                FramingState::Fixed { remaining } => {
                    let available = self.reader.fill_buf()?;
                    if available.is_empty() {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            PREMATURE_BODY_EOF,
                        ));
                    }
                    let take = remaining.min(available.len() as u64).min(buf.len() as u64) as usize;
                    buf[..take].copy_from_slice(&available[..take]);
                    self.reader.consume(take);
                    let left = remaining - take as u64;
                    self.state = if left == 0 {
                        FramingState::Done
                    } else {
                        FramingState::Fixed { remaining: left }
                    };
                    return Ok(take);
                }
                FramingState::ChunkSize => self.read_chunk_size_line()?,
                FramingState::ChunkData { remaining } => {
                    let available = self.reader.fill_buf()?;
                    if available.is_empty() {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            PREMATURE_BODY_EOF,
                        ));
                    }
                    let take = remaining.min(available.len() as u64).min(buf.len() as u64) as usize;
                    buf[..take].copy_from_slice(&available[..take]);
                    self.reader.consume(take);
                    let left = remaining - take as u64;
                    self.state = if left == 0 {
                        FramingState::ChunkTerminator
                    } else {
                        FramingState::ChunkData { remaining: left }
                    };
                    return Ok(take);
                }
                FramingState::ChunkTerminator => {
                    let line = self
                        .read_line_bounded(MAX_CHUNK_SIZE_LINE_BYTES, MALFORMED_CHUNK_TERMINATOR)?;
                    if line != b"\r\n" {
                        return Err(invalid_framing(MALFORMED_CHUNK_TERMINATOR));
                    }
                    self.state = FramingState::ChunkSize;
                }
                FramingState::Trailers => self.read_trailers()?,
            }
        }
    }
}

fn invalid_framing(diagnostic: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, diagnostic)
}

/// Parse one chunk-size line: `1*HEXDIG` size, typed extensions, CRLF.
///
/// The size is parsed with overflow checking: leading zeros of any length are
/// accepted, and a value above `u64::MAX` is rejected rather than truncated.
/// The extension region is validated by [`validate_chunk_extensions`].
fn parse_chunk_size_line(line: &[u8]) -> Result<u64, &'static str> {
    let body = match line.strip_suffix(b"\r\n") {
        Some(body) => body,
        None => return Err(MALFORMED_CHUNK_SIZE),
    };
    let digits = body
        .iter()
        .take_while(|byte| byte.is_ascii_hexdigit())
        .count();
    if digits == 0 {
        return Err(MALFORMED_CHUNK_SIZE);
    }
    let text = std::str::from_utf8(&body[..digits]).map_err(|_| MALFORMED_CHUNK_SIZE)?;
    let size = u64::from_str_radix(text, 16).map_err(|_| OVERFLOWING_CHUNK_SIZE)?;
    validate_chunk_extensions(&body[digits..])?;
    Ok(size)
}

/// Validate `*( BWS ";" BWS chunk-ext-name [ BWS "=" BWS chunk-ext-val ] )`.
///
/// `chunk-ext-val` is a token or a quoted-string with quoted-pairs
/// (RFC 9110 sections 5.6.2 and 5.6.4). Nothing here is guessed: each byte is
/// classified by the typed grammar, and `bytes` must be consumed exactly.
fn validate_chunk_extensions(bytes: &[u8]) -> Result<(), &'static str> {
    let mut index = 0;
    loop {
        if index == bytes.len() {
            return Ok(());
        }
        index = skip_bws(bytes, index);
        if bytes.get(index) != Some(&b';') {
            return Err(MALFORMED_CHUNK_EXTENSION);
        }
        index = skip_bws(bytes, index + 1);

        let name_end = token_end(bytes, index);
        if name_end == index {
            return Err(MALFORMED_CHUNK_EXTENSION);
        }
        index = name_end;
        let separator = skip_bws(bytes, index);

        if bytes.get(separator) == Some(&b'=') {
            index = skip_bws(bytes, separator + 1);
            match bytes.get(index) {
                Some(b'"') => index = quoted_string_end(bytes, index)?,
                Some(_) => {
                    let value_end = token_end(bytes, index);
                    if value_end == index {
                        return Err(MALFORMED_CHUNK_EXTENSION);
                    }
                    index = value_end;
                }
                None => return Err(MALFORMED_CHUNK_EXTENSION),
            }
        }
    }
}

fn skip_bws(bytes: &[u8], mut index: usize) -> usize {
    while matches!(bytes.get(index), Some(b' ') | Some(b'\t')) {
        index += 1;
    }
    index
}

/// End of a `token` starting at `index`; equal to `index` when empty.
fn token_end(bytes: &[u8], index: usize) -> usize {
    index
        + bytes[index.min(bytes.len())..]
            .iter()
            .take_while(|byte| is_tchar(**byte))
            .count()
}

/// End of a `quoted-string` starting at the opening DQUOTE.
fn quoted_string_end(bytes: &[u8], start: usize) -> Result<usize, &'static str> {
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => return Ok(index + 1),
            b'\\' => {
                index += 1;
                match bytes.get(index) {
                    Some(byte) if is_quoted_pair_octet(*byte) => index += 1,
                    _ => return Err(MALFORMED_CHUNK_EXTENSION),
                }
            }
            byte if is_qdtext(byte) => index += 1,
            _ => return Err(MALFORMED_CHUNK_EXTENSION),
        }
    }
    Err(MALFORMED_CHUNK_EXTENSION)
}

/// `tchar` (RFC 9110 section 5.6.2).
const fn is_tchar(byte: u8) -> bool {
    matches!(byte,
        b'!' | b'#'..=b'\'' | b'*' | b'+' | b'-' | b'.'
        | b'0'..=b'9' | b'A'..=b'Z' | b'^' | b'_' | b'`' | b'a'..=b'z' | b'|' | b'~')
}

/// `qdtext` (RFC 9110 section 5.6.4), obs-text included.
const fn is_qdtext(byte: u8) -> bool {
    matches!(byte, b'\t' | b' ' | b'!' | b'#'..=b'[' | b']'..=b'~' | 0x80..=0xFF)
}

/// Octets allowed after a quoted-pair backslash: HTAB / SP / VCHAR / obs-text.
const fn is_quoted_pair_octet(byte: u8) -> bool {
    matches!(byte, b'\t' | b' ' | b'!'..=b'~' | 0x80..=0xFF)
}

/// Validate a trailer section with `httparse` and refuse re-framing fields.
///
/// `bytes` includes the terminating blank line, so a complete parse that
/// consumes every byte is the only accepted shape. Trailer fields may not
/// restate framing or media-type authority that the head already declared.
fn validate_trailer_section(bytes: &[u8]) -> Result<(), &'static str> {
    let mut headers = [httparse::EMPTY_HEADER; MAX_TRAILER_LINES];
    let parsed = match httparse::parse_headers(bytes, &mut headers) {
        Ok(httparse::Status::Complete(parsed)) => parsed,
        Ok(httparse::Status::Partial) | Err(_) => return Err(MALFORMED_TRAILER),
    };
    if parsed.0 != bytes.len() || parsed.1.len() > MAX_TRAILER_LINES {
        return Err(MALFORMED_TRAILER);
    }
    for header in parsed.1 {
        if header.name.eq_ignore_ascii_case("content-length")
            || header.name.eq_ignore_ascii_case("transfer-encoding")
            || header.name.eq_ignore_ascii_case("content-type")
        {
            return Err(TRAILER_FRAMING_FIELD);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
