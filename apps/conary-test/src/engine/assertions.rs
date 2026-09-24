// apps/conary-test/src/engine/assertions.rs

use crate::config::manifest::{Assertion, JsonAssertion, JsonExpectation};
use anyhow::{Result, bail};
use serde_json::Value as JsonValue;

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
    for check in checks {
        let Some(actual) = document.pointer(&check.pointer) else {
            bail!("stdout JSON has no value at pointer \"{}\"", check.pointer);
        };
        match &check.expected {
            JsonExpectation::Equals(expected) => {
                let expected = toml_to_json(expected)?;
                if !json_values_equal(&expected, actual) {
                    bail!(
                        "stdout JSON at pointer \"{}\" did not match expected value\nexpected: {}\nactual: {}",
                        check.pointer,
                        serde_json::to_string_pretty(&expected)?,
                        serde_json::to_string_pretty(actual)?,
                    );
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

/// Convert a TOML value into its JSON equivalent for typed comparison.
fn toml_to_json(value: &toml::Value) -> Result<JsonValue> {
    Ok(match value {
        toml::Value::String(value) => JsonValue::String(value.clone()),
        toml::Value::Integer(value) => JsonValue::from(*value),
        toml::Value::Float(value) => {
            let number = serde_json::Number::from_f64(*value).ok_or_else(|| {
                anyhow::anyhow!("non-finite float {value} cannot be represented in JSON")
            })?;
            JsonValue::Number(number)
        }
        toml::Value::Boolean(value) => JsonValue::Bool(*value),
        toml::Value::Datetime(_) => {
            bail!("datetime values are not supported in stdout_json")
        }
        toml::Value::Array(values) => JsonValue::Array(
            values
                .iter()
                .map(toml_to_json)
                .collect::<Result<Vec<_>>>()?,
        ),
        toml::Value::Table(table) => {
            let mut object = serde_json::Map::new();
            for (key, value) in table {
                object.insert(key.clone(), toml_to_json(value)?);
            }
            JsonValue::Object(object)
        }
    })
}

/// Compare two JSON values structurally, treating integers and floats by value.
fn json_values_equal(expected: &JsonValue, actual: &JsonValue) -> bool {
    match (expected, actual) {
        (JsonValue::Object(expected), JsonValue::Object(actual)) => {
            expected.len() == actual.len()
                && expected.iter().all(|(key, expected)| {
                    actual
                        .get(key)
                        .is_some_and(|actual| json_values_equal(expected, actual))
                })
        }
        (JsonValue::Array(expected), JsonValue::Array(actual)) => {
            expected.len() == actual.len()
                && expected
                    .iter()
                    .zip(actual)
                    .all(|(expected, actual)| json_values_equal(expected, actual))
        }
        (JsonValue::Number(expected), JsonValue::Number(actual)) => {
            json_numbers_equal(expected, actual)
        }
        (JsonValue::String(expected), JsonValue::String(actual)) => expected == actual,
        (JsonValue::Bool(expected), JsonValue::Bool(actual)) => expected == actual,
        (JsonValue::Null, JsonValue::Null) => true,
        _ => false,
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

#[cfg(test)]
#[path = "assertions/tests.rs"]
mod tests;
