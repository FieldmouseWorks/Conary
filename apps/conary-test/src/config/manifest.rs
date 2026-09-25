// apps/conary-test/src/config/manifest.rs

use crate::engine::assertions::{
    JsonNumberToken, find_json_number_tokens, first_out_of_range_number,
};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::{HashMap, HashSet};

/// Top-level test manifest (one TOML file = one suite).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestManifest {
    pub suite: SuiteDef,
    pub test: Vec<TestDef>,
    #[serde(default)]
    pub distro_overrides: HashMap<String, HashMap<String, String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuiteDef {
    pub name: String,
    pub phase: u32,
    #[serde(default)]
    pub setup: Vec<TestStep>,
    #[serde(default)]
    pub mock_server: Option<MockServerConfig>,
    /// Suite-level timeout in seconds. If set, the entire suite must
    /// complete within this duration or remaining tests are cancelled.
    #[serde(default)]
    pub timeout: Option<u64>,
    /// Required typed semantic coverage for corpus suites.
    #[serde(default)]
    pub corpus: Option<super::corpus::CorpusSuiteDef>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestDef {
    pub id: String,
    pub name: String,
    pub description: String,
    pub timeout: u64,
    #[serde(default)]
    pub flaky: Option<bool>,
    #[serde(default)]
    pub retries: Option<u32>,
    /// Delay in milliseconds between retry attempts (default 0).
    #[serde(default)]
    pub retry_delay_ms: Option<u64>,
    #[serde(default)]
    pub step: Vec<TestStep>,
    #[serde(default)]
    pub resources: Option<ResourceConstraints>,
    #[serde(default)]
    pub depends_on: Option<Vec<String>>,
    #[serde(default)]
    pub fatal: Option<bool>,
    #[serde(default)]
    pub group: Option<String>,
    /// When set, the test is skipped with this reason string.
    #[serde(default)]
    pub skip: Option<String>,
    /// Runtime capabilities required inside the test container.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Attributable just-works corpus authority for this test.
    #[serde(default)]
    pub corpus: Option<super::corpus::CorpusCaseDef>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestStep {
    /// Per-step timeout override in seconds. Falls back to the test-level
    /// timeout when absent.
    #[serde(default)]
    pub timeout: Option<u64>,
    #[serde(default)]
    pub run: Option<String>,
    #[serde(default)]
    pub conary: Option<String>,
    #[serde(default)]
    pub kill_after_log: Option<KillAfterLog>,
    #[serde(default)]
    pub qemu_boot: Option<QemuBoot>,
    #[serde(default)]
    pub file_exists: Option<String>,
    #[serde(default)]
    pub file_not_exists: Option<String>,
    #[serde(default)]
    pub file_executable: Option<String>,
    #[serde(default)]
    pub file_checksum: Option<FileChecksum>,
    #[serde(default)]
    pub dir_exists: Option<String>,
    #[serde(default)]
    pub sleep: Option<u64>,
    #[serde(default)]
    pub assert: Option<Assertion>,
}

/// Derive step type from which field is populated.
#[derive(Debug, Clone)]
pub enum StepType {
    Run(String),
    Conary(String),
    KillAfterLog(KillAfterLog),
    QemuBoot(QemuBoot),
    FileExists(String),
    FileNotExists(String),
    FileExecutable(String),
    FileChecksum(FileChecksum),
    DirExists(String),
    Sleep(u64),
}

impl TestManifest {
    /// Returns true if every step in the manifest is a `qemu_boot` step.
    ///
    /// QEMU-only suites do not need a container runtime — they boot their
    /// own VMs. The CLI uses this to skip container image build/start.
    pub fn is_qemu_only(&self) -> bool {
        let has_tests = !self.test.is_empty();
        has_tests
            && self
                .test
                .iter()
                .all(|t| !t.step.is_empty() && t.step.iter().all(|s| s.qemu_boot.is_some()))
    }
}

impl TestStep {
    pub fn step_type(&self) -> Option<StepType> {
        if let Some(cmd) = &self.run {
            Some(StepType::Run(cmd.clone()))
        } else if let Some(cmd) = &self.conary {
            Some(StepType::Conary(cmd.clone()))
        } else if let Some(config) = &self.kill_after_log {
            Some(StepType::KillAfterLog(config.clone()))
        } else if let Some(config) = &self.qemu_boot {
            Some(StepType::QemuBoot(config.clone()))
        } else if let Some(path) = &self.file_exists {
            Some(StepType::FileExists(path.clone()))
        } else if let Some(path) = &self.file_not_exists {
            Some(StepType::FileNotExists(path.clone()))
        } else if let Some(path) = &self.file_executable {
            Some(StepType::FileExecutable(path.clone()))
        } else if let Some(chk) = &self.file_checksum {
            Some(StepType::FileChecksum(chk.clone()))
        } else if let Some(path) = &self.dir_exists {
            Some(StepType::DirExists(path.clone()))
        } else {
            self.sleep.map(StepType::Sleep)
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileChecksum {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KillAfterLog {
    pub conary: String,
    pub pattern: String,
    #[serde(default = "default_kill_timeout")]
    pub timeout_seconds: u64,
}

fn default_kill_timeout() -> u64 {
    60
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QemuGuestCopy {
    pub source: String,
    pub dest: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QemuImageFormat {
    #[default]
    Qcow2,
    Raw,
    Iso,
}

impl QemuImageFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Qcow2 => "qcow2",
            Self::Raw => "raw",
            Self::Iso => "iso",
        }
    }

    pub fn qemu_drive_format(self) -> Option<&'static str> {
        match self {
            Self::Qcow2 => Some("qcow2"),
            Self::Raw => Some("raw"),
            Self::Iso => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QemuBoot {
    pub image: String,
    #[serde(default)]
    pub local_image_path: Option<String>,
    #[serde(default)]
    pub image_format: QemuImageFormat,
    #[serde(default)]
    pub stage_conary: bool,
    #[serde(default)]
    pub scratch_disk_mb: Option<u64>,
    #[serde(default)]
    pub copy_to_guest: Vec<QemuGuestCopy>,
    #[serde(default)]
    pub copy_from_guest: Vec<QemuGuestCopy>,
    #[serde(default = "default_qemu_memory")]
    pub memory_mb: u32,
    #[serde(default = "default_qemu_timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "default_ssh_port")]
    pub ssh_port: u16,
    pub commands: Vec<String>,
    #[serde(default)]
    pub expect_output: Vec<String>,
}

fn default_qemu_memory() -> u32 {
    1024
}

fn default_qemu_timeout() -> u64 {
    300
}

fn default_ssh_port() -> u16 {
    2222
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MockServerConfig {
    pub port: u16,
    pub routes: Vec<MockRoute>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MockRoute {
    pub path: String,
    pub status: u16,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub body_file: Option<String>,
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default)]
    pub delay_ms: Option<u64>,
    #[serde(default)]
    pub truncate_at_bytes: Option<usize>,
}

#[cfg(test)]
#[path = "manifest/tests.rs"]
mod tests;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assertion {
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub exit_code_not: Option<i32>,
    #[serde(default)]
    pub stdout_contains: Option<String>,
    #[serde(default)]
    pub stdout_not_contains: Option<String>,
    /// All strings must appear in stdout.
    #[serde(default)]
    pub stdout_contains_all: Option<Vec<String>>,
    /// At least one string must appear in stdout.
    #[serde(default)]
    pub stdout_contains_any: Option<Vec<String>>,
    /// Check stdout contains this string only when exit code is 0.
    /// Non-zero exit is silently accepted (no assertion failure).
    #[serde(default)]
    pub stdout_contains_if_success: Option<String>,
    /// Check stdout contains any of these strings only when exit code is 0.
    /// Non-zero exit is silently accepted (no assertion failure).
    #[serde(default)]
    pub stdout_contains_any_if_success: Option<Vec<String>>,
    /// Typed checks against stdout parsed as a single JSON document.
    ///
    /// Each entry sets exactly one expectation form: `equals` (a TOML value),
    /// `equals_json` (a JSON text string for values TOML cannot express), or
    /// `null = true`.
    #[serde(default)]
    pub stdout_json: Option<Vec<JsonAssertion>>,
    #[serde(default)]
    pub stderr_contains: Option<String>,
    #[serde(default)]
    pub stderr_not_contains: Option<String>,
    #[serde(default)]
    pub file_exists: Option<String>,
    #[serde(default)]
    pub file_not_exists: Option<String>,
    #[serde(default)]
    pub file_checksum: Option<FileChecksum>,
}

/// One typed check against stdout parsed as a single JSON document.
///
/// The expected value is supplied by exactly one of the TOML entry's `equals`,
/// `equals_json`, or `null = true` fields.
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "RawJsonAssertion")]
pub struct JsonAssertion {
    /// RFC 6901 JSON pointer into the parsed stdout document ("" is the whole document).
    pub pointer: String,
    /// Expected value at the pointer.
    pub expected: JsonExpectation,
    /// Exact source token for every number in `expected`, keyed by its RFC 6901
    /// pointer relative to the expected document root.
    ///
    /// `equals_json` numbers keep the token from the manifest text, so a
    /// decimal is compared by its exact value rather than a rounded `f64`.
    /// `equals` numbers come from TOML, which has no JSON token; they have no
    /// entry here and the comparator uses the value's shortest round-trip
    /// representation instead.
    pub(crate) numbers: HashMap<String, String>,
}

/// Expected value for a `stdout_json` pointer.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonExpectation {
    /// Resolved value equals this JSON value.
    ///
    /// A number is compared by its exact decimal value; an integer token never
    /// equals a decimal token, so `1` does not equal `1.0`.
    Equals(JsonValue),
    /// Resolved value is JSON null.
    ///
    /// TOML has no null literal and its integers are `i64`, so use
    /// `equals_json` to express a nested null or an unsigned integer above
    /// `i64::MAX`.
    Null,
}

/// Raw TOML shape for a `stdout_json` entry, validated into a `JsonAssertion`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawJsonAssertion {
    pointer: String,
    #[serde(default)]
    equals: Option<toml::Value>,
    /// A JSON value literal, parsed at load time.
    ///
    /// TOML integers are `i64` and TOML has no null literal, so `equals` cannot
    /// express an unsigned integer above `i64::MAX` or a nested JSON null. The
    /// string holds the JSON text verbatim. Every number token is kept exactly
    /// from this text, so an integer outside `i64`/`u64` or any decimal is
    /// compared by its exact decimal value rather than a rounded `f64`.
    ///
    /// A number whose magnitude exceeds finite `f64` range (for example a
    /// 400-digit integer, or a value above `f64::MAX`) is not supported: the
    /// default `serde_json` parser cannot represent it. The loader rejects it
    /// with an error naming the pointer and the unsupported range rather than
    /// serde_json's generic parse failure.
    /// Mutually exclusive with `equals` and `null`.
    #[serde(default)]
    equals_json: Option<String>,
    #[serde(default)]
    null: Option<bool>,
}

/// Whether `input` contains a manifest `${NAME}` variable reference.
///
/// Delegates to the template grammar owner in `engine::variables`, so the
/// marker, name charset, and expansion rules cannot drift apart. A `${` that
/// does not form a well-formed reference still counts, so deferred validation
/// continues to catch malformed or truncated templates.
pub(crate) fn contains_variable_reference(input: &str) -> bool {
    crate::engine::variables::contains_variable_reference(input)
}

/// Validate the syntax of an RFC 6901 JSON pointer.
///
/// The empty string addresses the whole document. Otherwise the pointer must
/// begin with `/`, and every `~` must introduce the escape `~0` or `~1`. The
/// `~` character carries no other meaning, so `${VAR}` references are plain
/// characters for this check.
///
/// A pointer containing `${` is a template: the substituted value can change
/// whether the pointer is valid. Load-time validation therefore skips it and
/// the runner re-checks the expanded pointer before a test's first step.
pub(crate) fn validate_json_pointer(pointer: &str) -> std::result::Result<(), String> {
    if pointer.is_empty() {
        return Ok(());
    }
    if !pointer.starts_with('/') {
        return Err(format!(
            "stdout_json pointer {pointer:?} is not an RFC 6901 JSON pointer: \
             it must be empty or begin with '/'"
        ));
    }
    let mut chars = pointer.chars();
    while let Some(ch) = chars.next() {
        if ch == '~' {
            match chars.next() {
                Some('0' | '1') => {}
                Some(escape) => {
                    return Err(format!(
                        "stdout_json pointer {pointer:?} is not an RFC 6901 JSON pointer: \
                         '~' must be followed by '0' or '1', not {escape:?}"
                    ));
                }
                None => {
                    return Err(format!(
                        "stdout_json pointer {pointer:?} is not an RFC 6901 JSON pointer: \
                         a trailing '~' is not a valid escape"
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Split a validated RFC 6901 JSON pointer into its escaped reference tokens.
///
/// The root pointer (`""`) has no tokens; otherwise the leading `/` introduces
/// the first token. Tokens keep their `~0`/`~1` escapes, which RFC 6901 spells
/// uniquely, so token equality is pointer equality. Callers pass only pointers
/// that already passed `validate_json_pointer`.
pub(crate) fn json_pointer_tokens(pointer: &str) -> Vec<&str> {
    let Some(rest) = pointer.strip_prefix('/') else {
        return Vec::new();
    };
    rest.split('/').collect()
}

/// Whether two validated RFC 6901 pointers address overlapping values.
///
/// Pointers overlap when one equals the other or is a proper ancestor of it,
/// because a check at a pointer fully determines the value at and under it.
/// Comparing tokens rather than string prefixes keeps `/a` and `/ab` distinct.
pub(crate) fn pointers_overlap(a: &str, b: &str) -> bool {
    let a = json_pointer_tokens(a);
    let b = json_pointer_tokens(b);
    let shared = a.len().min(b.len());
    a[..shared] == b[..shared]
}

impl TryFrom<RawJsonAssertion> for JsonAssertion {
    type Error = String;

    fn try_from(raw: RawJsonAssertion) -> std::result::Result<Self, Self::Error> {
        // A templated pointer is validated after substitution, before the
        // test's first step runs.
        if !contains_variable_reference(&raw.pointer) {
            validate_json_pointer(&raw.pointer)?;
        }
        let (expected, numbers) = match (raw.equals, raw.equals_json, raw.null) {
            (Some(value), None, None) => {
                let value = toml_to_json(&value).map_err(|error| error.to_string())?;
                (JsonExpectation::Equals(value), HashMap::new())
            }
            (None, Some(text), None) => {
                // Scan the raw text before `serde_json` so a number beyond
                // finite `f64` range is reported as an explicit limitation
                // rather than serde_json's generic parse error. A walker error
                // means the text is malformed; fall through so serde_json owns
                // the syntax diagnostic.
                let scanned = find_json_number_tokens(&text);
                if let Ok(numbers) = &scanned
                    && let Some(number) = first_out_of_range_number(numbers)
                {
                    return Err(unsupported_equals_json_number(&raw.pointer, number));
                }
                let value = serde_json::from_str(&text)
                    .map_err(|error| invalid_equals_json(&raw.pointer, &error))?;
                let numbers = scanned
                    .map_err(|error| invalid_equals_json_text(&raw.pointer, &error))?
                    .into_iter()
                    .map(|number| (number.pointer, number.token))
                    .collect();
                (JsonExpectation::Equals(value), numbers)
            }
            (None, None, Some(true)) => (JsonExpectation::Null, HashMap::new()),
            (None, None, Some(false)) => {
                return Err(
                    "`null = false` is not an assertion; use `equals` or `equals_json`".to_string(),
                );
            }
            (None, None, None) => {
                return Err(
                    "set exactly one of `equals`, `equals_json`, or `null = true`".to_string(),
                );
            }
            _ => {
                return Err("set exactly one of `equals`, `equals_json`, or `null`".to_string());
            }
        };
        Ok(Self {
            pointer: raw.pointer,
            expected,
            numbers,
        })
    }
}

/// Convert a TOML value into its JSON equivalent for typed comparison.
///
/// Datetimes and non-finite floats have no JSON representation and are
/// rejected before a manifest is accepted.
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

/// Build the load error for `equals_json` text that is not valid JSON.
fn invalid_equals_json(pointer: &str, error: &serde_json::Error) -> String {
    format!("stdout_json pointer {pointer:?} has invalid `equals_json` JSON: {error}")
}

/// Build the load error for `equals_json` text containing a number beyond
/// finite `f64` range.
///
/// `serde_json` without `arbitrary_precision` cannot represent such a number
/// and would fail with a generic parse error. Naming the number's pointer and
/// the limitation makes the failure actionable.
fn unsupported_equals_json_number(pointer: &str, number: &JsonNumberToken) -> String {
    format!(
        "stdout_json pointer {pointer:?} has an `equals_json` number at {:?} beyond the \
         supported finite f64 range; such numbers are not supported",
        number.pointer
    )
}

/// Build the load error for `equals_json` text the number walker rejected.
///
/// The walker only reaches this path after `serde_json` accepted the same
/// text, so this is an internal invariant failure rather than user input
/// reaching a new state.
fn invalid_equals_json_text(pointer: &str, error: &anyhow::Error) -> String {
    format!("stdout_json pointer {pointer:?} has invalid `equals_json`: {error}")
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceConstraints {
    #[serde(default)]
    pub tmpfs_size_mb: Option<u64>,
    #[serde(default)]
    pub memory_limit_mb: Option<u64>,
    #[serde(default)]
    pub network_isolated: Option<bool>,
}

/// The manifest element that owns an assertion under validation.
///
/// The owner supplies only the error context; the validation rules are
/// identical for a test step and a `suite.setup` step.
#[derive(Debug, Clone, Copy)]
enum AssertionOwner<'a> {
    /// A step in `suite.setup`, by zero-based index.
    SuiteSetup { step: usize },
    /// A step in the named test, by zero-based index.
    Test { id: &'a str, step: usize },
}

impl AssertionOwner<'_> {
    fn context(&self) -> String {
        match self {
            Self::SuiteSetup { step } => format!("suite setup, step {step}"),
            Self::Test { id, step } => format!("test {id}, step {step}"),
        }
    }
}

impl Assertion {
    /// Validate that the assertion has no conflicting fields.
    ///
    /// Detects cases like setting both `exit_code` and `exit_code_not` to the
    /// same value, or `stdout_contains` and `stdout_not_contains` with the
    /// same string, which would make the assertion impossible to satisfy.
    pub fn validate(&self, test_id: &str, step_index: usize) -> Result<()> {
        self.validate_for(AssertionOwner::Test {
            id: test_id,
            step: step_index,
        })
    }

    /// Validate an assertion attached to a `suite.setup` step.
    ///
    /// Suite setup assertions must obey the same load-time rules as test-step
    /// assertions, so both route through `validate_for`.
    pub(crate) fn validate_suite_setup(&self, step_index: usize) -> Result<()> {
        self.validate_for(AssertionOwner::SuiteSetup { step: step_index })
    }

    fn validate_for(&self, owner: AssertionOwner<'_>) -> Result<()> {
        let ctx = || owner.context();

        // exit_code vs exit_code_not
        if let (Some(code), Some(not_code)) = (self.exit_code, self.exit_code_not)
            && code == not_code
        {
            bail!(
                "{}: conflicting assertion: exit_code={code} and exit_code_not={not_code}",
                ctx()
            );
        }

        // stdout_contains vs stdout_not_contains
        if let (Some(contains), Some(not_contains)) =
            (&self.stdout_contains, &self.stdout_not_contains)
            && contains == not_contains
        {
            bail!(
                "{}: conflicting assertion: stdout_contains and stdout_not_contains \
                 both set to {:?}",
                ctx(),
                contains
            );
        }

        // stdout_contains_all vs stdout_not_contains
        if let (Some(all), Some(not_contains)) =
            (&self.stdout_contains_all, &self.stdout_not_contains)
            && all.iter().any(|s| s == not_contains)
        {
            bail!(
                "{}: conflicting assertion: stdout_contains_all includes {:?} \
                 which is also set in stdout_not_contains",
                ctx(),
                not_contains
            );
        }

        // file_exists vs file_not_exists
        if let (Some(exists), Some(not_exists)) = (&self.file_exists, &self.file_not_exists)
            && exists == not_exists
        {
            bail!(
                "{}: conflicting assertion: file_exists and file_not_exists \
                 both set to {:?}",
                ctx(),
                exists
            );
        }

        // A non-templated pointer determines the value at and under it, so a
        // second check that equals or descends from it is redundant or
        // contradictory. Compare RFC 6901 tokens rather than string prefixes,
        // so `/ab` and `/a` do not overlap. Templated pointers are deferred to
        // the expanded preflight, as the load-time pointer validation already
        // does.
        if let Some(checks) = &self.stdout_json {
            let concrete: Vec<&str> = checks
                .iter()
                .map(|check| check.pointer.as_str())
                .filter(|pointer| !contains_variable_reference(pointer))
                .collect();
            for (index, pointer) in concrete.iter().copied().enumerate() {
                for other in concrete[index + 1..].iter().copied() {
                    if pointers_overlap(pointer, other) {
                        bail!(
                            "{}: overlapping stdout_json pointers {pointer:?} and {other:?}: \
                             a check on an ancestor already determines its descendants, \
                             so the second check is redundant or contradictory",
                            ctx()
                        );
                    }
                }
            }
        }

        Ok(())
    }
}

impl TestManifest {
    /// Validate all assertions in the manifest for conflicting fields.
    pub fn validate(&self) -> Result<()> {
        // Every `distro_overrides` inner key is a template variable name, so it
        // must satisfy the same grammar `${NAME}` accepts. A key the tokenizer
        // cannot reference would otherwise be merged into the variable map and
        // never substituted.
        for (distro, overrides) in &self.distro_overrides {
            for key in overrides.keys() {
                if !crate::engine::variables::is_template_name(key) {
                    bail!(
                        "manifest {:?}: distro {:?} distro_overrides key {:?} is not a valid \
                         template name ([A-Za-z_][A-Za-z0-9_]*)",
                        self.suite.name,
                        distro,
                        key
                    );
                }
            }
        }
        let corpus_tests = self
            .test
            .iter()
            .filter_map(|test| test.corpus.as_ref())
            .collect::<Vec<_>>();
        match (&self.suite.corpus, corpus_tests.is_empty()) {
            (None, true) => {}
            (None, false) => bail!("corpus tests require suite-level semantic coverage"),
            (Some(_), true) => bail!("suite corpus coverage requires at least one corpus test"),
            (Some(corpus), false) => {
                corpus.validate()?;
                let required = corpus.required.iter().copied().collect::<HashSet<_>>();
                let claimed = corpus_tests
                    .iter()
                    .flat_map(|case| case.coverage.iter().map(|claim| claim.semantic))
                    .collect::<HashSet<_>>();
                if claimed != required {
                    let mut missing = required.difference(&claimed).copied().collect::<Vec<_>>();
                    let mut undeclared = claimed.difference(&required).copied().collect::<Vec<_>>();
                    missing.sort();
                    undeclared.sort();
                    bail!(
                        "suite corpus coverage and case claims disagree: missing={missing:?}, undeclared={undeclared:?}"
                    );
                }
            }
        }
        // Suite setup assertions run before any test and must satisfy the
        // same load-time rules; their owner label identifies them as setup.
        for (i, step) in self.suite.setup.iter().enumerate() {
            if let Some(ref assertion) = step.assert {
                assertion.validate_suite_setup(i)?;
            }
        }
        for test in &self.test {
            if let Some(corpus) = &test.corpus {
                corpus.validate(&test.id)?;
                if test.resources.is_some() {
                    bail!(
                        "test {}: corpus evidence cannot use a resource-scoped disposable container",
                        test.id
                    );
                }
            }
            for requirement in &test.requires {
                if requirement != "composefs_runtime" {
                    bail!(
                        "test {} has unknown runtime requirement `{}`",
                        test.id,
                        requirement
                    );
                }
            }
            for (i, step) in test.step.iter().enumerate() {
                if let Some(ref assertion) = step.assert {
                    assertion.validate(&test.id, i)?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "manifest/tests/validation_tests.rs"]
mod validation_tests;
