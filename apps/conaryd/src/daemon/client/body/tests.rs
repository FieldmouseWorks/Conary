// apps/conaryd/src/daemon/client/body/tests.rs

//! Unit tests for response body framing derivation and chunked decoding.

use super::*;
use std::io::{ErrorKind, Read};

fn framing(status: u16, pairs: &[(&str, &[u8])]) -> Result<BodyFraming, &'static str> {
    let headers: Vec<httparse::Header<'_>> = pairs
        .iter()
        .map(|(name, value)| httparse::Header { name, value })
        .collect();
    derive_framing(status, &headers)
}

/// Read every payload byte using caller buffers of exactly `chunk` bytes.
fn read_all(framing: BodyFraming, raw: &[u8], chunk: usize) -> io::Result<Vec<u8>> {
    let mut source = raw;
    let mut body = BodyReader::new(&mut source, framing);
    let mut out = Vec::new();
    let mut buf = vec![0u8; chunk];
    loop {
        let read = body.read(&mut buf)?;
        if read == 0 {
            return Ok(out);
        }
        out.extend_from_slice(&buf[..read]);
    }
}

fn decode(framing: BodyFraming, raw: &[u8]) -> Vec<u8> {
    read_all(framing, raw, 64).expect("body decodes")
}

fn decode_error(framing: BodyFraming, raw: &[u8]) -> (ErrorKind, String) {
    let error = read_all(framing, raw, 64).expect_err("body must be rejected");
    (error.kind(), error.to_string())
}

#[test]
fn derives_fixed_from_repeated_identical_decimal_lengths() {
    assert_eq!(
        framing(200, &[("Content-Length", b"5")]),
        Ok(BodyFraming::Fixed(5))
    );
    assert_eq!(
        framing(200, &[("content-length", b"5")]),
        Ok(BodyFraming::Fixed(5))
    );
    assert_eq!(
        framing(200, &[("Content-Length", b" 5 , 5 ,5")]),
        Ok(BodyFraming::Fixed(5))
    );
    assert_eq!(
        framing(
            200,
            &[("Content-Length", b"0005"), ("Content-Length", b"5")]
        ),
        Ok(BodyFraming::Fixed(5))
    );
    assert_eq!(
        framing(200, &[("Content-Length", b"0")]),
        Ok(BodyFraming::Fixed(0))
    );
    // Trailing OWS is not part of the field value (RFC 9110 section 5.5).
    assert_eq!(
        framing(200, &[("Content-Length", b"5 \t")]),
        Ok(BodyFraming::Fixed(5))
    );
}

#[test]
fn rejects_malformed_conflicting_and_overflowing_lengths() {
    for value in [b"".as_slice(), b"-1", b"1.5", b"0x10", b"5, ", b",5", b"+5"] {
        assert_eq!(
            framing(200, &[("Content-Length", value)]),
            Err(MALFORMED_CONTENT_LENGTH),
            "value {value:?}"
        );
    }
    assert_eq!(
        framing(200, &[("Content-Length", b"99999999999999999999")]),
        Err(OVERFLOWING_CONTENT_LENGTH)
    );
    assert_eq!(
        framing(200, &[("Content-Length", b"5, 6")]),
        Err(CONFLICTING_CONTENT_LENGTH)
    );
    assert_eq!(
        framing(200, &[("Content-Length", b"5"), ("Content-Length", b"6")]),
        Err(CONFLICTING_CONTENT_LENGTH)
    );
}

#[test]
fn derives_chunked_only_for_a_single_chunked_coding() {
    assert_eq!(
        framing(200, &[("Transfer-Encoding", b"chunked")]),
        Ok(BodyFraming::Chunked)
    );
    assert_eq!(
        framing(200, &[("transfer-encoding", b"Chunked")]),
        Ok(BodyFraming::Chunked)
    );
    assert_eq!(
        framing(200, &[("Transfer-Encoding", b" chunked ")]),
        Ok(BodyFraming::Chunked)
    );
    for value in [
        b"gzip".as_slice(),
        b"chunked, chunked",
        b"chunked,",
        b"identity",
        b"",
    ] {
        assert_eq!(
            framing(200, &[("Transfer-Encoding", value)]),
            Err(UNSUPPORTED_TRANSFER_CODING),
            "value {value:?}"
        );
    }
    assert_eq!(
        framing(
            200,
            &[
                ("Transfer-Encoding", b"chunked"),
                ("Transfer-Encoding", b"chunked")
            ]
        ),
        Err(UNSUPPORTED_TRANSFER_CODING)
    );
    assert_eq!(
        framing(
            200,
            &[("Transfer-Encoding", b"chunked"), ("Content-Length", b"5")]
        ),
        Err(TRANSFER_ENCODING_WITH_CONTENT_LENGTH)
    );
}

#[test]
fn defaults_to_close_delimited_and_treats_bodyless_statuses_as_empty() {
    assert_eq!(framing(200, &[]), Ok(BodyFraming::CloseDelimited));
    assert_eq!(
        framing(500, &[("Content-Type", b"text/plain")]),
        Ok(BodyFraming::CloseDelimited)
    );
    for status in [100, 101, 199, 204, 304] {
        assert_eq!(
            framing(
                status,
                &[("Content-Length", b"5"), ("Transfer-Encoding", b"chunked")]
            ),
            Ok(BodyFraming::Empty),
            "status {status}"
        );
    }
}

#[test]
fn fixed_reader_stops_at_the_declared_length() {
    let mut source: &[u8] = b"helloPIPELINED";
    let mut body = BodyReader::new(&mut source, BodyFraming::Fixed(5));
    let mut out = String::new();
    body.read_to_string(&mut out).unwrap();
    assert_eq!(out, "hello");
    assert_eq!(source, b"PIPELINED");
}

#[test]
fn empty_reader_consumes_nothing() {
    let mut source: &[u8] = b"payload";
    let mut body = BodyReader::new(&mut source, BodyFraming::Empty);
    let mut buf = [0u8; 8];
    assert_eq!(body.read(&mut buf).unwrap(), 0);
    assert_eq!(source, b"payload");

    let mut source: &[u8] = b"payload";
    let mut body = BodyReader::new(&mut source, BodyFraming::Fixed(0));
    assert_eq!(body.read(&mut buf).unwrap(), 0);
    assert_eq!(source, b"payload");
}

#[test]
fn premature_fixed_eof_is_unexpected_eof() {
    assert_eq!(
        decode_error(BodyFraming::Fixed(6), b"short"),
        (ErrorKind::UnexpectedEof, PREMATURE_BODY_EOF.to_string())
    );
    assert_eq!(decode(BodyFraming::Fixed(5), b"hello"), b"hello");
    assert_eq!(decode(BodyFraming::Fixed(5), b"hello trailing"), b"hello");
}

#[test]
fn close_delimited_reads_until_eof() {
    assert_eq!(
        decode(BodyFraming::CloseDelimited, b"close-delimited body"),
        b"close-delimited body"
    );
    assert_eq!(decode(BodyFraming::CloseDelimited, b""), b"");
}

#[test]
fn decodes_chunked_bodies_for_every_buffer_size() {
    let raw: &[u8] =
        b"5; a=b; quoted=\"x; \\\"y\\\"\"\r\nhello\r\n6\r\n world\r\n0\r\nX-Sum: 11\r\n\r\n";
    for chunk in 1..=9 {
        assert_eq!(
            read_all(BodyFraming::Chunked, raw, chunk).unwrap(),
            b"hello world",
            "buffer size {chunk}"
        );
    }
}

#[test]
fn accepts_legal_chunk_extensions() {
    let raw: &[u8] =
        b"3;a=b; c = d ;e=\"f; \\\"g\\\"\";h=\"\";i=\"\xE9\";obs=\"\\\x7E\"\r\nabc\r\n0\r\n\r\n";
    assert_eq!(decode(BodyFraming::Chunked, raw), b"abc");
    assert!(validate_chunk_extensions(b"").is_ok());
    assert!(validate_chunk_extensions(b";a").is_ok());
    assert!(validate_chunk_extensions(b";a=b=c").is_err());
    assert!(validate_chunk_extensions(b";name=\"\xC3\xA9\"").is_ok());
}

#[test]
fn rejects_illegal_chunk_extensions() {
    for extensions in [
        ";",
        ";=b",
        ";a=",
        ";a=b c",
        ";a=\"unterminated",
        ";a=\"b\"c",
        ";a=\"x\\\"",
        ";a=\"\x01\"",
        "garbage",
    ] {
        assert_eq!(
            validate_chunk_extensions(extensions.as_bytes()),
            Err(MALFORMED_CHUNK_EXTENSION),
            "extensions {extensions:?}"
        );
    }
    let raw: &[u8] = b"3;a=\"unterminated\r\nabc\r\n0\r\n\r\n";
    assert_eq!(
        decode_error(BodyFraming::Chunked, raw),
        (
            ErrorKind::InvalidData,
            MALFORMED_CHUNK_EXTENSION.to_string()
        )
    );
}

#[test]
fn rejects_malformed_and_overflowing_chunk_sizes() {
    for raw in [
        b"zz\r\n".as_slice(),
        b"5\nhello\r\n0\r\n\r\n",
        b"\r\n",
        b" 5\r\n",
        b"-5\r\n",
        b"5x\r\n",
    ] {
        let (kind, message) = decode_error(BodyFraming::Chunked, raw);
        assert_eq!(kind, ErrorKind::InvalidData, "raw {raw:?}");
        assert!(
            message == MALFORMED_CHUNK_SIZE || message == MALFORMED_CHUNK_EXTENSION,
            "raw {raw:?} produced {message}"
        );
    }
    assert_eq!(
        decode_error(BodyFraming::Chunked, b"10000000000000000\r\n"),
        (ErrorKind::InvalidData, OVERFLOWING_CHUNK_SIZE.to_string())
    );
    // Leading zeros are legal at any length; the size keeps its hex value.
    assert_eq!(
        decode(BodyFraming::Chunked, b"00000003\r\nabc\r\n0\r\n\r\n"),
        b"abc"
    );
    assert_eq!(
        decode(
            BodyFraming::Chunked,
            b"0000000000000003\r\nabc\r\n0\r\n\r\n"
        ),
        b"abc"
    );
}

#[test]
fn rejects_missing_crlf_and_truncated_chunks() {
    assert_eq!(
        decode_error(BodyFraming::Chunked, b"5\r\nhelloX\r\n0\r\n\r\n"),
        (
            ErrorKind::InvalidData,
            MALFORMED_CHUNK_TERMINATOR.to_string()
        )
    );
    assert_eq!(
        decode_error(BodyFraming::Chunked, b"5\r\nhel"),
        (ErrorKind::UnexpectedEof, PREMATURE_BODY_EOF.to_string())
    );
    assert_eq!(
        decode_error(BodyFraming::Chunked, b"5"),
        (ErrorKind::UnexpectedEof, PREMATURE_BODY_EOF.to_string())
    );
    assert_eq!(
        decode_error(BodyFraming::Chunked, b"5\r\nhello\r\n3\r\nab"),
        (ErrorKind::UnexpectedEof, PREMATURE_BODY_EOF.to_string())
    );
}

#[test]
fn accepts_chunk_terminator_with_extension_and_empty_body() {
    assert_eq!(decode(BodyFraming::Chunked, b"0;done=yes\r\n\r\n"), b"");
    assert_eq!(decode(BodyFraming::Chunked, b"0\r\n\r\n"), b"");
    assert_eq!(
        decode(BodyFraming::Chunked, b"4\r\nabcd\r\n0\r\n\r\n"),
        b"abcd"
    );
}

#[test]
fn bounds_chunk_size_lines_at_eight_kibibytes() {
    let mut raw = b"5;".to_vec();
    raw.extend(std::iter::repeat_n(b'a', MAX_CHUNK_SIZE_LINE_BYTES));
    raw.extend_from_slice(b"\r\nhello\r\n0\r\n\r\n");
    assert_eq!(
        decode_error(BodyFraming::Chunked, &raw),
        (ErrorKind::InvalidData, MALFORMED_CHUNK_SIZE.to_string())
    );
}

#[test]
fn accepts_bounded_trailer_sections() {
    assert_eq!(
        decode(
            BodyFraming::Chunked,
            b"3\r\nabc\r\n0\r\nX-Sum: 3\r\nX-Note: ok\r\n\r\n"
        ),
        b"abc"
    );

    let mut raw = b"0\r\n".to_vec();
    for _ in 0..MAX_TRAILER_LINES {
        raw.extend_from_slice(b"X-T: 1\r\n");
    }
    raw.extend_from_slice(b"\r\n");
    assert_eq!(decode(BodyFraming::Chunked, &raw), b"");
}

#[test]
fn rejects_forbidden_and_malformed_trailers() {
    for name in ["Content-Length", "transfer-encoding", "Content-Type"] {
        let raw = format!("0\r\n{name}: 1\r\n\r\n");
        assert_eq!(
            decode_error(BodyFraming::Chunked, raw.as_bytes()),
            (ErrorKind::InvalidData, TRAILER_FRAMING_FIELD.to_string()),
            "trailer {name}"
        );
    }
    assert_eq!(
        decode_error(BodyFraming::Chunked, b"0\r\nbad trailer line\r\n\r\n"),
        (ErrorKind::InvalidData, MALFORMED_TRAILER.to_string())
    );
    assert_eq!(
        decode_error(BodyFraming::Chunked, b"0\r\nX: \x01\r\n\r\n"),
        (ErrorKind::InvalidData, MALFORMED_TRAILER.to_string())
    );
}

#[test]
fn bounds_trailer_lines_and_total_bytes() {
    let mut raw = b"0\r\n".to_vec();
    for _ in 0..=MAX_TRAILER_LINES {
        raw.extend_from_slice(b"X-T: 1\r\n");
    }
    raw.extend_from_slice(b"\r\n");
    assert_eq!(
        decode_error(BodyFraming::Chunked, &raw),
        (ErrorKind::InvalidData, MALFORMED_TRAILER.to_string())
    );

    let mut raw = b"0\r\n".to_vec();
    raw.extend_from_slice(b"X-Long: ");
    raw.extend(std::iter::repeat_n(b'a', MAX_TRAILER_LINE_BYTES));
    raw.extend_from_slice(b"\r\n\r\n");
    assert_eq!(
        decode_error(BodyFraming::Chunked, &raw),
        (ErrorKind::InvalidData, MALFORMED_TRAILER.to_string())
    );

    let field = format!("X-Pad: {}\r\n", "a".repeat(7900));
    let mut raw = b"0\r\n".to_vec();
    for _ in 0..5 {
        raw.extend_from_slice(field.as_bytes());
    }
    raw.extend_from_slice(b"\r\n");
    assert!(field.len() * 5 > MAX_TRAILER_BYTES);
    assert_eq!(
        decode_error(BodyFraming::Chunked, &raw),
        (ErrorKind::InvalidData, MALFORMED_TRAILER.to_string())
    );
}

#[test]
fn trailer_section_requires_a_complete_parse() {
    assert_eq!(validate_trailer_section(b"X-A: 1\r\n\r\n"), Ok(()));
    assert_eq!(
        validate_trailer_section(b"X-A: 1\r\n"),
        Err(MALFORMED_TRAILER)
    );
    assert_eq!(
        validate_trailer_section(b"X-A: 1\r\n\r\ntrailing"),
        Err(MALFORMED_TRAILER)
    );
}
