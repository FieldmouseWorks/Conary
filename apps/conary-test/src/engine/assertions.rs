// apps/conary-test/src/engine/assertions.rs

use crate::config::manifest::{Assertion, JsonAssertion, JsonExpectation};
use anyhow::{Result, bail};
use serde_json::Value as JsonValue;
use std::collections::HashMap;

pub fn evaluate_assertion(
    assertion: &Assertion,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
) -> Result<()> {
    if let Some(expected) = assertion.exit_code
        && exit_code != expected
    {
        bail!("expected exit code {expected}, got {exit_code}");
    }
    if let Some(not_expected) = assertion.exit_code_not
        && exit_code == not_expected
    {
        bail!("expected exit code other than {not_expected}, got {exit_code}");
    }
    if let Some(ref needle) = assertion.stdout_contains
        && !stdout.contains(needle.as_str())
    {
        bail!("stdout does not contain \"{needle}\"");
    }
    if let Some(ref needle) = assertion.stdout_not_contains
        && stdout.contains(needle.as_str())
    {
        bail!("stdout unexpectedly contains \"{needle}\"");
    }
    if let Some(ref needles) = assertion.stdout_contains_all {
        for needle in needles {
            if !stdout.contains(needle.as_str()) {
                bail!("stdout does not contain \"{needle}\" (stdout_contains_all)");
            }
        }
    }
    if let Some(ref needles) = assertion.stdout_contains_any
        && !needles.iter().any(|n| stdout.contains(n.as_str()))
    {
        bail!(
            "stdout does not contain any of {:?} (stdout_contains_any)",
            needles
        );
    }
    // Conditional assertions: only checked when exit code is 0.
    if exit_code == 0 {
        if let Some(ref needle) = assertion.stdout_contains_if_success
            && !stdout.contains(needle.as_str())
        {
            bail!("stdout does not contain \"{needle}\" (stdout_contains_if_success)");
        }
        if let Some(ref needles) = assertion.stdout_contains_any_if_success
            && !needles.iter().any(|n| stdout.contains(n.as_str()))
        {
            bail!(
                "stdout does not contain any of {:?} (stdout_contains_any_if_success)",
                needles
            );
        }
    }
    if let Some(ref checks) = assertion.stdout_json {
        evaluate_stdout_json(checks, stdout)?;
    }
    if let Some(ref needle) = assertion.stderr_contains
        && !stderr.contains(needle.as_str())
    {
        bail!("stderr does not contain \"{needle}\"");
    }
    if let Some(ref needle) = assertion.stderr_not_contains
        && stderr.contains(needle.as_str())
    {
        bail!("stderr unexpectedly contains \"{needle}\"");
    }
    Ok(())
}

/// Parse all of `stdout` as one JSON document and apply every typed check.
fn evaluate_stdout_json(checks: &[JsonAssertion], stdout: &str) -> Result<()> {
    let document: JsonValue = serde_json::from_str(stdout)
        .map_err(|error| anyhow::anyhow!("stdout is not valid JSON: {error}"))?;
    // `serde_json` stores an integer token outside i64/u64 as `f64`, which
    // cannot hold it exactly. Record the source token at each pointer so a
    // number check refuses the value instead of comparing rounded floats.
    let inexact_integers: HashMap<String, String> = find_inexact_json_integers(stdout)
        .map_err(|error| anyhow::anyhow!("stdout is not valid JSON: {error}"))?
        .into_iter()
        .map(|integer| (integer.pointer, integer.token))
        .collect();
    for check in checks {
        let Some(actual) = document.pointer(&check.pointer) else {
            bail!("stdout JSON has no value at pointer \"{}\"", check.pointer);
        };
        match &check.expected {
            JsonExpectation::Equals(expected) => {
                match compare_json_values(expected, actual, &check.pointer, &inexact_integers) {
                    Ok(()) => {}
                    Err(JsonComparisonError::Mismatch) => bail!(
                        "stdout JSON at pointer \"{}\" did not match expected value\nexpected: {}\nactual: {}",
                        check.pointer,
                        serde_json::to_string_pretty(expected)?,
                        serde_json::to_string_pretty(actual)?,
                    ),
                    Err(JsonComparisonError::InexactInteger { pointer, token }) => bail!(
                        "stdout JSON at pointer \"{}\" has integer `{}` at JSON pointer \"{}\" \
                         outside the exactly comparable i64/u64 range; refusing to compare it as a float",
                        check.pointer,
                        token,
                        pointer,
                    ),
                }
            }
            JsonExpectation::Null => {
                if !actual.is_null() {
                    bail!(
                        "stdout JSON at pointer \"{}\" is not null\nactual: {}",
                        check.pointer,
                        serde_json::to_string_pretty(actual)?,
                    );
                }
            }
        }
    }
    Ok(())
}

/// Why a typed JSON comparison did not succeed.
enum JsonComparisonError {
    /// The values differ.
    Mismatch,
    /// The actual value is an integer token outside the i64/u64 range, which
    /// `serde_json` stored as `f64`.
    InexactInteger { pointer: String, token: String },
}

/// Compare two JSON values structurally.
///
/// The RFC 6901 pointer to the current position is threaded through the walk
/// so an out-of-range actual integer can be named exactly instead of being
/// compared as a float.
fn compare_json_values(
    expected: &JsonValue,
    actual: &JsonValue,
    pointer: &str,
    inexact_integers: &HashMap<String, String>,
) -> std::result::Result<(), JsonComparisonError> {
    match (expected, actual) {
        (JsonValue::Object(expected), JsonValue::Object(actual)) => {
            if expected.len() != actual.len() {
                return Err(JsonComparisonError::Mismatch);
            }
            for (key, expected) in expected {
                let Some(actual) = actual.get(key) else {
                    return Err(JsonComparisonError::Mismatch);
                };
                let child = format!("{pointer}/{}", escape_json_pointer_token(key));
                compare_json_values(expected, actual, &child, inexact_integers)?;
            }
            Ok(())
        }
        (JsonValue::Array(expected), JsonValue::Array(actual)) => {
            if expected.len() != actual.len() {
                return Err(JsonComparisonError::Mismatch);
            }
            for (index, (expected, actual)) in expected.iter().zip(actual).enumerate() {
                let child = format!("{pointer}/{index}");
                compare_json_values(expected, actual, &child, inexact_integers)?;
            }
            Ok(())
        }
        (JsonValue::Number(expected), JsonValue::Number(actual)) => {
            if let Some(token) = inexact_integers.get(pointer) {
                return Err(JsonComparisonError::InexactInteger {
                    pointer: pointer.to_string(),
                    token: token.clone(),
                });
            }
            if json_numbers_equal(expected, actual) {
                Ok(())
            } else {
                Err(JsonComparisonError::Mismatch)
            }
        }
        (JsonValue::String(expected), JsonValue::String(actual)) if expected == actual => Ok(()),
        (JsonValue::Bool(expected), JsonValue::Bool(actual)) if expected == actual => Ok(()),
        (JsonValue::Null, JsonValue::Null) => Ok(()),
        _ => Err(JsonComparisonError::Mismatch),
    }
}

/// Numbers match only when both are integers with the same value or both are
/// floats with the same value; an integer never matches a float.
fn json_numbers_equal(expected: &serde_json::Number, actual: &serde_json::Number) -> bool {
    match (integer_value(expected), integer_value(actual)) {
        (Some(expected), Some(actual)) => expected == actual,
        (None, None) => match (expected.as_f64(), actual.as_f64()) {
            (Some(expected), Some(actual)) => expected == actual,
            _ => false,
        },
        _ => false,
    }
}

fn integer_value(number: &serde_json::Number) -> Option<i128> {
    number
        .as_i64()
        .map(i128::from)
        .or_else(|| number.as_u64().map(i128::from))
}

/// An integer literal in JSON text that `serde_json` cannot represent exactly.
///
/// `serde_json` stores an integer token outside `i64`/`u64` as an `f64`, whose
/// 53-bit significand cannot hold every such integer. The source token is kept
/// here so a check can refuse it by name instead of comparing rounded floats.
#[derive(Debug, Clone)]
pub(crate) struct InexactJsonInteger {
    /// RFC 6901 pointer to the number within the scanned document.
    pub(crate) pointer: String,
    /// The integer's source token.
    pub(crate) token: String,
}

/// Find every integer literal in `json` that does not fit `i64` or `u64`.
///
/// `serde_json` keeps an in-range integer token exact but represents an
/// out-of-range one as `f64`, so `18446744073709551616` and
/// `18446744073709551617` become the same number. Rather than enable an
/// optional `serde_json` feature that exposes source tokens, this re-scans the
/// validated text with a small typed walker. The walker skips strings and
/// tracks RFC 6901 pointers, so it cannot mistake a digit sequence inside a
/// string for a JSON number, and it matches the JSON grammar exactly rather
/// than by pattern.
///
/// `json` must already have parsed with `serde_json`; the walker assumes
/// well-formed JSON and fails only if that invariant is broken.
pub(crate) fn find_inexact_json_integers(json: &str) -> Result<Vec<InexactJsonInteger>> {
    let mut walker = JsonNumberWalker {
        bytes: json.as_bytes(),
        pos: 0,
        pointer: String::new(),
        inexact: Vec::new(),
    };
    walker.walk_document()?;
    Ok(walker.inexact)
}

/// Escape one RFC 6901 reference token so it can be appended to a pointer.
///
/// RFC 6901 escapes `~` as `~0` and `/` as `~1`; applying `~` first keeps an
/// existing `~1` from being read as an escape.
pub(crate) fn escape_json_pointer_token(token: &str) -> String {
    let mut escaped = String::with_capacity(token.len());
    for character in token.chars() {
        match character {
            '~' => escaped.push_str("~0"),
            '/' => escaped.push_str("~1"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// A minimal exact walker over already-validated JSON text.
///
/// It exists only to pair each number token with its RFC 6901 pointer; the
/// document has already been parsed by `serde_json`, so it does not build
/// values and treats malformed input as an internal error.
struct JsonNumberWalker<'a> {
    bytes: &'a [u8],
    pos: usize,
    pointer: String,
    inexact: Vec<InexactJsonInteger>,
}

impl JsonNumberWalker<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn bump(&mut self) -> Result<u8> {
        let byte = self
            .bytes
            .get(self.pos)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("unexpected end of JSON"))?;
        self.pos += 1;
        Ok(byte)
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn walk_document(&mut self) -> Result<()> {
        self.skip_whitespace();
        self.walk_value()?;
        self.skip_whitespace();
        if self.pos != self.bytes.len() {
            bail!("trailing bytes after JSON value");
        }
        Ok(())
    }

    fn walk_value(&mut self) -> Result<()> {
        match self.peek() {
            Some(b'{') => self.walk_object(),
            Some(b'[') => self.walk_array(),
            Some(b'"') => {
                self.walk_string()?;
                Ok(())
            }
            Some(b'-' | b'0'..=b'9') => self.walk_number(),
            Some(b't') => self.walk_literal(b"true"),
            Some(b'f') => self.walk_literal(b"false"),
            Some(b'n') => self.walk_literal(b"null"),
            _ => bail!("expected a JSON value"),
        }
    }

    fn walk_object(&mut self) -> Result<()> {
        self.bump()?; // `{`
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.bump()?;
            return Ok(());
        }
        loop {
            self.skip_whitespace();
            let key = self.walk_string()?;
            let saved = self.pointer.len();
            self.pointer.push('/');
            self.pointer.push_str(&escape_json_pointer_token(&key));
            self.skip_whitespace();
            if self.bump()? != b':' {
                bail!("expected ':' after JSON object key");
            }
            self.skip_whitespace();
            self.walk_value()?;
            self.pointer.truncate(saved);
            self.skip_whitespace();
            match self.bump()? {
                b',' => continue,
                b'}' => return Ok(()),
                _ => bail!("expected ',' or '}}' in JSON object"),
            }
        }
    }

    fn walk_array(&mut self) -> Result<()> {
        self.bump()?; // `[`
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.bump()?;
            return Ok(());
        }
        let mut index = 0usize;
        loop {
            let saved = self.pointer.len();
            self.pointer.push('/');
            self.pointer.push_str(&index.to_string());
            self.skip_whitespace();
            self.walk_value()?;
            self.pointer.truncate(saved);
            index += 1;
            self.skip_whitespace();
            match self.bump()? {
                b',' => continue,
                b']' => return Ok(()),
                _ => bail!("expected ',' or ']' in JSON array"),
            }
        }
    }

    /// Consume a JSON string and return its decoded contents.
    fn walk_string(&mut self) -> Result<String> {
        if self.bump()? != b'"' {
            bail!("expected '\"'");
        }
        let mut decoded = Vec::new();
        loop {
            match self.bump()? {
                b'"' => break,
                b'\\' => self.walk_escape(&mut decoded)?,
                byte => decoded.push(byte),
            }
        }
        String::from_utf8(decoded)
            .map_err(|error| anyhow::anyhow!("invalid UTF-8 in JSON string: {error}"))
    }

    fn walk_escape(&mut self, decoded: &mut Vec<u8>) -> Result<()> {
        match self.bump()? {
            b'"' => decoded.push(b'"'),
            b'\\' => decoded.push(b'\\'),
            b'/' => decoded.push(b'/'),
            b'b' => decoded.push(0x08),
            b'f' => decoded.push(0x0c),
            b'n' => decoded.push(b'\n'),
            b'r' => decoded.push(b'\r'),
            b't' => decoded.push(b'\t'),
            b'u' => {
                let first = self.read_hex4()?;
                let scalar = if (0xD800..=0xDBFF).contains(&first) {
                    if self.bump()? != b'\\' || self.bump()? != b'u' {
                        bail!("unpaired UTF-16 high surrogate escape");
                    }
                    let second = self.read_hex4()?;
                    if !(0xDC00..=0xDFFF).contains(&second) {
                        bail!("UTF-16 high surrogate not followed by a low surrogate");
                    }
                    0x1_0000 + ((u32::from(first) - 0xD800) << 10) + (u32::from(second) - 0xDC00)
                } else {
                    u32::from(first)
                };
                let character = char::from_u32(scalar)
                    .ok_or_else(|| anyhow::anyhow!("invalid Unicode escape"))?;
                let mut buffer = [0u8; 4];
                decoded.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
            }
            _ => bail!("invalid JSON string escape"),
        }
        Ok(())
    }

    fn read_hex4(&mut self) -> Result<u16> {
        let mut value = 0u16;
        for _ in 0..4 {
            let digit = match self.bump()? {
                byte @ b'0'..=b'9' => byte - b'0',
                byte @ b'a'..=b'f' => byte - b'a' + 10,
                byte @ b'A'..=b'F' => byte - b'A' + 10,
                _ => bail!("invalid hex digit in JSON escape"),
            };
            value = value * 16 + u16::from(digit);
        }
        Ok(value)
    }

    fn walk_number(&mut self) -> Result<()> {
        let start = self.pos;
        while matches!(
            self.peek(),
            Some(b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
        ) {
            self.pos += 1;
        }
        let token = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|error| anyhow::anyhow!("invalid UTF-8 in JSON number: {error}"))?;
        // An integer literal has no fraction or exponent. If it fits neither
        // `i64` nor `u64`, `serde_json` will have stored it as `f64`.
        if !token.bytes().any(|byte| matches!(byte, b'.' | b'e' | b'E'))
            && token.parse::<i64>().is_err()
            && token.parse::<u64>().is_err()
        {
            self.inexact.push(InexactJsonInteger {
                pointer: self.pointer.clone(),
                token: token.to_string(),
            });
        }
        Ok(())
    }

    fn walk_literal(&mut self, literal: &[u8]) -> Result<()> {
        if self.bytes[self.pos..].starts_with(literal) {
            self.pos += literal.len();
            Ok(())
        } else {
            bail!("invalid JSON literal")
        }
    }
}

#[cfg(test)]
#[path = "assertions/tests.rs"]
mod tests;
