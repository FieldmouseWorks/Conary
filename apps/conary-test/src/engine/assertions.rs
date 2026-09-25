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
    // Classify source number tokens before comparing. `serde_json` accepts a
    // token such as `1e-9223372036854775809` as a finite zero even though the
    // exact comparator cannot canonicalize its exponent, and it rejects a token
    // beyond finite `f64` range as a syntax error. Both are the same range
    // limitation, so scan once and name it. The scan must succeed and every
    // token must be a well-formed number; any other defect keeps the generic
    // syntax diagnostic.
    let scanned = find_json_number_tokens(stdout);
    if let Ok(numbers) = &scanned
        && let Some(number) = first_unsupported_number(numbers)
    {
        bail!(
            "stdout is valid JSON but contains a number beyond the supported range at \"{}\"",
            number.pointer
        );
    }
    let document: JsonValue = serde_json::from_str(stdout)
        .map_err(|error| anyhow::anyhow!("stdout is not valid JSON: {error}"))?;
    // `serde_json` stores every decimal token as `f64`, and an integer token
    // outside `i64`/`u64` as `f64` too. Record the source token for every
    // number at its pointer so a check compares exact decimal values instead
    // of rounded floats.
    let actual_numbers: HashMap<String, String> = scanned
        .map_err(|error| anyhow::anyhow!("stdout is not valid JSON: {error}"))?
        .into_iter()
        .map(|number| (number.pointer, number.token))
        .collect();
    for check in checks {
        let Some(actual) = document.pointer(&check.pointer) else {
            bail!("stdout JSON has no value at pointer \"{}\"", check.pointer);
        };
        match &check.expected {
            JsonExpectation::Equals(expected) => {
                let expected_numbers = expected_number_index(check);
                match compare_json_values(
                    expected,
                    actual,
                    &check.pointer,
                    &expected_numbers,
                    &actual_numbers,
                ) {
                    Ok(()) => {}
                    Err(JsonComparisonError::Mismatch) => bail!(
                        "stdout JSON at pointer \"{}\" did not match expected value\nexpected: {}\nactual: {}",
                        check.pointer,
                        serde_json::to_string_pretty(expected)?,
                        serde_json::to_string_pretty(actual)?,
                    ),
                    Err(JsonComparisonError::MissingActualNumber { pointer }) => bail!(
                        "stdout JSON number at pointer \"{pointer}\" has no recorded source token; \
                         refusing to compare an unverifiable value"
                    ),
                    Err(JsonComparisonError::UnsupportedNumber { pointer }) => bail!(
                        "stdout is valid JSON but contains a number beyond the supported range at \"{pointer}\""
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

/// Build the expected number index for one check.
///
/// `JsonAssertion::numbers` stores tokens by their RFC 6901 pointer relative to
/// the expected document, while comparison walks the value at `check.pointer`
/// and threads absolute pointers. Prefixing each inner pointer keeps the two
/// key spaces aligned even when the check pointer is nested or templated.
fn expected_number_index(check: &JsonAssertion) -> HashMap<String, String> {
    check
        .numbers
        .iter()
        .map(|(inner, token)| (format!("{}{}", check.pointer, inner), token.clone()))
        .collect()
}

/// Why a typed JSON comparison did not succeed.
enum JsonComparisonError {
    /// The values differ.
    Mismatch,
    /// The actual value is a number but the walker did not record its token.
    MissingActualNumber { pointer: String },
    /// A number token could not be reduced to an exact decimal.
    UnsupportedNumber { pointer: String },
}

/// Compare two JSON values structurally.
///
/// The RFC 6901 pointer to the current position is threaded through the walk,
/// and numbers are compared by their exact decimal source tokens: `expected`
/// tokens come from `equals_json` (or the value's shortest round-trip form when
/// the expectation came from TOML), and `actual` tokens come from the stdout
/// text. Integer and decimal tokens are different kinds, so `1` never equals
/// `1.0`.
fn compare_json_values(
    expected: &JsonValue,
    actual: &JsonValue,
    pointer: &str,
    expected_numbers: &HashMap<String, String>,
    actual_numbers: &HashMap<String, String>,
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
                compare_json_values(expected, actual, &child, expected_numbers, actual_numbers)?;
            }
            Ok(())
        }
        (JsonValue::Array(expected), JsonValue::Array(actual)) => {
            if expected.len() != actual.len() {
                return Err(JsonComparisonError::Mismatch);
            }
            for (index, (expected, actual)) in expected.iter().zip(actual).enumerate() {
                let child = format!("{pointer}/{index}");
                compare_json_values(expected, actual, &child, expected_numbers, actual_numbers)?;
            }
            Ok(())
        }
        (JsonValue::Number(expected), JsonValue::Number(_)) => {
            let Some(actual_token) = actual_numbers.get(pointer) else {
                return Err(JsonComparisonError::MissingActualNumber {
                    pointer: pointer.to_string(),
                });
            };
            let expected_token = expected_numbers.get(pointer).map(String::as_str);
            match ExactNumber::matches(expected, expected_token, actual_token) {
                Ok(true) => Ok(()),
                Ok(false) => Err(JsonComparisonError::Mismatch),
                Err(_) => Err(JsonComparisonError::UnsupportedNumber {
                    pointer: pointer.to_string(),
                }),
            }
        }
        (JsonValue::String(expected), JsonValue::String(actual)) if expected == actual => Ok(()),
        (JsonValue::Bool(expected), JsonValue::Bool(actual)) if expected == actual => Ok(()),
        (JsonValue::Null, JsonValue::Null) => Ok(()),
        _ => Err(JsonComparisonError::Mismatch),
    }
}

/// The syntactically distinct kinds of JSON number (RFC 8259 §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsonNumberKind {
    /// No fraction and no exponent, for example `1` or `-2`.
    Integer,
    /// A fraction, an exponent, or both, for example `1.0` or `1e2`.
    Decimal,
}

impl JsonNumberKind {
    /// Classify a JSON number source token by its grammar shape.
    pub(crate) fn from_token(token: &str) -> Self {
        if token.bytes().any(|byte| matches!(byte, b'.' | b'e' | b'E')) {
            Self::Decimal
        } else {
            Self::Integer
        }
    }
}

/// A JSON number reduced to its exact decimal value.
///
/// The value is `(-1)^negative * digits * 10^exponent`, where `digits` is a
/// decimal string with no leading or trailing zeros and is `"0"` for zero.
/// Equal decimal values have identical canonical forms, so `1.50`, `1.5`,
/// `15e-1`, and `0.15E1` all normalize to `digits = "15"`, `exponent = -1`.
/// Zero is never negative, so `-0`, `-0.0`, and `0` share one canonical zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalDecimal {
    negative: bool,
    digits: String,
    exponent: i64,
}

impl CanonicalDecimal {
    /// Parse a JSON number source token into canonical form.
    ///
    /// The scanner is hand-written against the RFC 8259 §6 grammar; it does not
    /// use a regular expression. Callers only reduce tokens they have already
    /// classified as JSON numbers, but the parser still reports a malformed or
    /// out-of-range-exponent token as a typed error rather than panicking.
    pub(crate) fn parse(token: &str) -> std::result::Result<Self, CanonicalDecimalError> {
        let (negative, unsigned) = match token.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, token),
        };
        let (mantissa, exponent_text) = match unsigned.split_once(['e', 'E']) {
            Some((mantissa, exponent)) => (mantissa, Some(exponent)),
            None => (unsigned, None),
        };
        let exponent = match exponent_text {
            Some(text) => parse_exponent(text).ok_or_else(|| invalid_number_token(token))?,
            None => 0,
        };
        let (integer_digits, fraction_digits) = match mantissa.split_once('.') {
            Some((integer, fraction)) => {
                if fraction.is_empty() {
                    return Err(invalid_number_token(token));
                }
                (integer, fraction)
            }
            None => (mantissa, ""),
        };
        if !is_integer_part(integer_digits)
            || !fraction_digits.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(invalid_number_token(token));
        }
        // `digits` is the integer and fraction digits concatenated without the
        // point; the value is `digits * 10^(exponent - fraction_len)`.
        let mut digits = String::with_capacity(integer_digits.len() + fraction_digits.len());
        digits.push_str(integer_digits);
        digits.push_str(fraction_digits);
        let mut exponent = exponent
            .checked_sub(
                i64::try_from(fraction_digits.len()).map_err(|_| invalid_number_token(token))?,
            )
            .ok_or_else(|| invalid_number_token(token))?;
        let Some(first_significant) = digits.bytes().position(|byte| byte != b'0') else {
            return Ok(Self {
                negative: false,
                digits: "0".to_string(),
                exponent: 0,
            });
        };
        let mut significant = digits[first_significant..].to_string();
        let trailing_zeros = significant
            .bytes()
            .rev()
            .take_while(|byte| *byte == b'0')
            .count();
        if trailing_zeros > 0 {
            significant.truncate(significant.len() - trailing_zeros);
            exponent = exponent
                .checked_add(
                    i64::try_from(trailing_zeros).map_err(|_| invalid_number_token(token))?,
                )
                .ok_or_else(|| invalid_number_token(token))?;
        }
        Ok(Self {
            negative,
            digits: significant,
            exponent,
        })
    }
}

/// Whether the exact comparator supports `token`.
///
/// A token is supported when `CanonicalDecimal::parse` reduces it exactly and
/// its magnitude lies within finite `f64`. This is the one rule shared by
/// manifest loading and runtime evaluation. Canonicalizability is part of the
/// rule because `parse` rejects an exponent outside `i64` while `serde_json`
/// accepts such a token as a finite zero (for example
/// `1e-9223372036854775809`); without that clause a manifest would load and then
/// fail during comparison.
pub(crate) fn is_supported_json_number_token(token: &str) -> bool {
    let Ok(value) = CanonicalDecimal::parse(token) else {
        return false;
    };
    if canonical_order(&value).is_some_and(|order| order > F64_MAX_DECIMAL_EXPONENT) {
        return false;
    }
    // The leading-digit order settles every value above 308; at exactly 308 the
    // leading digits still decide, so ask whether the token rounds to infinity.
    token.parse::<f64>().is_ok_and(f64::is_finite)
}

/// Build the error for a token that does not parse as a JSON number.
fn invalid_number_token(token: &str) -> CanonicalDecimalError {
    CanonicalDecimalError {
        token: token.to_string(),
    }
}

/// Whether `digits` is the integer part of an RFC 8259 number.
fn is_integer_part(digits: &str) -> bool {
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    // RFC 8259 forbids a leading zero unless the integer part is exactly "0".
    digits == "0" || !digits.starts_with('0')
}

/// Parse the exponent digits after `e`/`E` into an `i64`.
fn parse_exponent(text: &str) -> Option<i64> {
    let (negative, digits) = if let Some(rest) = text.strip_prefix('+') {
        (false, rest)
    } else if let Some(rest) = text.strip_prefix('-') {
        (true, rest)
    } else {
        (false, text)
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let magnitude = digits.parse::<i128>().ok()?;
    let signed = if negative { -magnitude } else { magnitude };
    i64::try_from(signed).ok()
}

/// The decimal exponent of `f64::MAX` (`1.797...e308`).
const F64_MAX_DECIMAL_EXPONENT: i64 = 308;

/// The first unsupported number among a complete scan.
///
/// The all-valid guard lets a caller excuse a parse failure only when an
/// unsupported number is the *only* defect. A document that also contains a
/// malformed number yields `None` so its syntax error stays authoritative.
pub(crate) fn first_unsupported_number(numbers: &[JsonNumberToken]) -> Option<&JsonNumberToken> {
    if !numbers
        .iter()
        .all(|number| is_json_number_token(&number.token))
    {
        return None;
    }
    numbers
        .iter()
        .find(|number| !is_supported_json_number_token(&number.token))
}

/// The decimal exponent of a canonical value's leading digit.
///
/// Returns `None` when the exponent and digit count cannot be added without
/// overflowing, which already means the value is far outside `f64` range.
fn canonical_order(value: &CanonicalDecimal) -> Option<i64> {
    let digits = i64::try_from(value.digits.len()).ok()?;
    value.exponent.checked_add(digits)?.checked_sub(1)
}

/// Whether `token` matches the RFC 8259 §6 number grammar.
///
/// `CanonicalDecimal::parse` validates the same grammar but also rejects an
/// exponent that does not fit `i64`; this independent check keeps a
/// syntactically valid number with an astronomically large exponent a range
/// problem rather than a syntax problem. It is a hand-written scanner, not a
/// regular expression.
fn is_json_number_token(token: &str) -> bool {
    let unsigned = token.strip_prefix('-').unwrap_or(token);
    let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, Some(exponent)),
        None => (unsigned, None),
    };
    if exponent.is_some_and(|exponent| !is_exponent_part(exponent)) {
        return false;
    }
    let (integer, fraction) = match mantissa.split_once('.') {
        Some((integer, fraction)) => (integer, Some(fraction)),
        None => (mantissa, None),
    };
    if !is_integer_part(integer) {
        return false;
    }
    fraction.is_none_or(|fraction| {
        !fraction.is_empty() && fraction.bytes().all(|byte| byte.is_ascii_digit())
    })
}

/// Whether `text` is the exponent part of an RFC 8259 number.
///
/// Unlike `parse_exponent`, this does not bound the magnitude, so an exponent
/// too large for `i64` is still recognized as part of a valid number.
fn is_exponent_part(text: &str) -> bool {
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

/// A number token that is not a valid RFC 8259 number.
#[derive(Debug, thiserror::Error)]
#[error("invalid JSON number token `{token}`")]
pub(crate) struct CanonicalDecimalError {
    token: String,
}

/// A JSON number paired with its syntactic kind and exact decimal value.
struct ExactNumber {
    kind: JsonNumberKind,
    value: CanonicalDecimal,
}

impl ExactNumber {
    /// Reduce a source token, preserving whether it is an integer or decimal.
    fn from_token(token: &str) -> std::result::Result<Self, CanonicalDecimalError> {
        Ok(Self {
            kind: JsonNumberKind::from_token(token),
            value: CanonicalDecimal::parse(token)?,
        })
    }

    /// Reduce a `serde_json` value that has no source token.
    ///
    /// Used for `equals` expectations from TOML: TOML has no JSON text, so an
    /// integer becomes its decimal digits and a float becomes Rust's shortest
    /// round-trip representation (`format!("{value}")`). TOML floats are
    /// therefore limited to the decimals `f64` can represent exactly; use
    /// `equals_json` for the exact form.
    fn from_serde_number(
        number: &serde_json::Number,
    ) -> std::result::Result<Self, CanonicalDecimalError> {
        if let Some(value) = number.as_i64() {
            return Self::from_integer_token(value.to_string());
        }
        if let Some(value) = number.as_u64() {
            return Self::from_integer_token(value.to_string());
        }
        if let Some(value) = number.as_f64() {
            // The kind comes from the typed `serde_json` float, not from the
            // formatted text: `format!("{}", 1.0_f64)` is `"1"`, which would
            // otherwise read as an integer token.
            return Ok(Self {
                kind: JsonNumberKind::Decimal,
                value: CanonicalDecimal::parse(&format!("{value}"))?,
            });
        }
        Err(invalid_number_token(&number.to_string()))
    }

    fn from_integer_token(token: String) -> std::result::Result<Self, CanonicalDecimalError> {
        Ok(Self {
            kind: JsonNumberKind::Integer,
            value: CanonicalDecimal::parse(&token)?,
        })
    }

    /// Whether `expected` equals `actual_token` by exact decimal value.
    ///
    /// Cross-kind matches are refused: an integer token never equals a decimal
    /// token, so `1` does not equal `1.0` even though their canonical decimal
    /// values agree. `expected_token` is the source token when the expectation
    /// came from `equals_json`; otherwise the `serde_json` value is reduced via
    /// its shortest round-trip representation.
    fn matches(
        expected: &serde_json::Number,
        expected_token: Option<&str>,
        actual_token: &str,
    ) -> std::result::Result<bool, CanonicalDecimalError> {
        let expected = match expected_token {
            Some(token) => Self::from_token(token)?,
            None => Self::from_serde_number(expected)?,
        };
        let actual = Self::from_token(actual_token)?;
        Ok(expected.kind == actual.kind && expected.value == actual.value)
    }
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

/// One JSON number's source token and the RFC 6901 pointer that addresses it.
#[derive(Debug, Clone)]
pub(crate) struct JsonNumberToken {
    /// RFC 6901 pointer to the number within the scanned document.
    pub(crate) pointer: String,
    /// The number's source token, exactly as it appears in the text.
    pub(crate) token: String,
}

/// Find every number literal in `json` with the source token at its pointer.
///
/// `serde_json` keeps an in-range integer token exact but represents every
/// decimal token, and any integer outside `i64`/`u64`, as `f64`, which cannot
/// hold them exactly. Rather than enable an optional `serde_json` feature that
/// exposes source tokens, this re-scans the text with a small typed walker.
/// The walker skips strings and tracks RFC 6901 pointers, so it cannot mistake
/// a digit sequence inside a string for a JSON number, and it recognizes JSON
/// structure rather than scanning for a pattern. Number grammar is validated
/// separately where it matters.
///
/// Callers pass both text `serde_json` already accepted and raw text whose
/// parse failed. Malformed input therefore returns a typed error instead of
/// panicking, which lets the caller fall back to `serde_json`'s diagnostic.
pub(crate) fn find_json_number_tokens(json: &str) -> Result<Vec<JsonNumberToken>> {
    let mut walker = JsonNumberWalker {
        bytes: json.as_bytes(),
        pos: 0,
        pointer: String::new(),
        numbers: Vec::new(),
    };
    walker.walk_document()?;
    Ok(walker.numbers)
}

/// A minimal exact walker over JSON text.
///
/// It exists only to pair each number token with its RFC 6901 pointer; it does
/// not build values, and it reports malformed input as a typed error so a
/// caller can decide whether to retry or fall back.
struct JsonNumberWalker<'a> {
    bytes: &'a [u8],
    pos: usize,
    pointer: String,
    numbers: Vec<JsonNumberToken>,
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
                // RFC 8259 allows `0x20` and above unescaped, so a raw control
                // character is malformed and must not be mistaken for text.
                byte if byte < 0x20 => {
                    bail!("unescaped control character in JSON string")
                }
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
        self.numbers.push(JsonNumberToken {
            pointer: self.pointer.clone(),
            token: token.to_string(),
        });
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
