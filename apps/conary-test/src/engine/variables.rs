// apps/conary-test/src/engine/variables.rs

use std::collections::HashMap;

use crate::config::corpus::{CorpusCaseDef, CorpusTargetDef};
use crate::config::distro::GlobalConfig;
use crate::config::manifest::{
    Assertion, FileChecksum, JsonAssertion, JsonExpectation, QemuBoot, QemuGuestCopy, TestManifest,
};

/// Build the base variable map from global config and distro selection.
///
/// Populates variables from the Remi endpoint, paths, fixture config, and
/// distro-specific test packages. These variables are available to all tests
/// via `${VAR}` substitution in manifest fields.
pub fn build_variables(config: &GlobalConfig, distro: &str) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    let conary_binaries = config.paths.resolve_conary_binaries();
    vars.insert("DISTRO".to_string(), distro.to_string());
    vars.insert("REMI_ENDPOINT".to_string(), config.remi.endpoint.clone());
    vars.insert("DB_PATH".to_string(), config.paths.db.clone());
    vars.insert("CONARY_BIN".to_string(), conary_binaries.ordinary);
    vars.insert("CONARY_HOOKS_BIN".to_string(), conary_binaries.test_hooks);
    if let Some(fixture_dir) = &config.paths.fixture_dir {
        vars.insert("FIXTURE_DIR".to_string(), fixture_dir.clone());
        let fixture_root = std::path::Path::new(fixture_dir);
        vars.insert(
            "FIXTURE_CCS_KEY".to_string(),
            crate::paths::fixture_ccs_key_path_for(fixture_root)
                .to_string_lossy()
                .into_owned(),
        );
        vars.insert(
            "FIXTURE_CCS_PUBLIC_KEY".to_string(),
            crate::paths::fixture_ccs_public_key_path_for(fixture_root)
                .to_string_lossy()
                .into_owned(),
        );
        vars.insert(
            "FIXTURE_CCS_POLICY".to_string(),
            crate::paths::fixture_ccs_policy_path_for(fixture_root)
                .to_string_lossy()
                .into_owned(),
        );
        vars.insert(
            "FIXTURE_CCS_EXPIRED_POLICY".to_string(),
            crate::paths::fixture_ccs_expired_policy_path_for(fixture_root)
                .to_string_lossy()
                .into_owned(),
        );
    }

    if let Some(fixtures) = &config.fixtures {
        if let Some(value) = &fixtures.package {
            vars.insert("FIXTURE_PKG_NAME".to_string(), value.clone());
        }
        if let Some(value) = &fixtures.file {
            vars.insert("FIXTURE_FILE".to_string(), value.clone());
        }
        if let Some(value) = &fixtures.added_file {
            vars.insert("FIXTURE_ADDED_FILE".to_string(), value.clone());
        }
        if let Some(value) = &fixtures.marker {
            vars.insert("FIXTURE_MARKER".to_string(), value.clone());
        }
        if let Some(fixture_dir) = &config.paths.fixture_dir {
            if let Some(value) = &fixtures.v1_ccs_file {
                vars.insert(
                    "FIXTURE_V1_CCS".to_string(),
                    format!("{fixture_dir}/conary-test-fixture/v1/output/{value}"),
                );
            }
            if let Some(value) = &fixtures.v2_ccs_file {
                vars.insert(
                    "FIXTURE_V2_CCS".to_string(),
                    format!("{fixture_dir}/conary-test-fixture/v2/output/{value}"),
                );
            }
        }
        if let Some(value) = &fixtures.v1_hello_sha256 {
            vars.insert("FIXTURE_V1_HELLO_SHA256".to_string(), value.clone());
        }
        if let Some(value) = &fixtures.v2_hello_sha256 {
            vars.insert("FIXTURE_V2_HELLO_SHA256".to_string(), value.clone());
        }
        if let Some(value) = &fixtures.v2_added_sha256 {
            vars.insert("FIXTURE_V2_ADDED_SHA256".to_string(), value.clone());
        }
    }

    // Add distro-specific variables if present.
    if let Some(dc) = config.distros.get(distro) {
        vars.insert("REMI_DISTRO".to_string(), dc.remi_distro.clone());
        vars.insert("REPO_NAME".to_string(), dc.repo_name.clone());
        for (i, tp) in dc.test_packages.iter().enumerate() {
            let n = i + 1;
            vars.insert(format!("TEST_PACKAGE_{n}"), tp.package.clone());
            vars.insert(format!("TEST_BINARY_{n}"), tp.binary.clone());
        }
    }

    vars
}

/// Load distro-specific manifest overrides into an existing variable map.
pub fn load_manifest_overrides(
    vars: &mut HashMap<String, String>,
    manifest: &TestManifest,
    distro: &str,
) {
    if let Some(overrides) = manifest.distro_overrides.get(distro) {
        vars.extend(overrides.clone());
    }
}

/// Build the complete variable map the runner uses for `manifest` on `distro`.
///
/// This is the single authority for manifest variables: it starts from the
/// base config and distro variables and merges the manifest's
/// `distro_overrides` for `distro`. The runner and the early pointer preflight
/// both compute variables here, so they cannot drift.
pub fn build_manifest_variables(
    config: &GlobalConfig,
    distro: &str,
    manifest: &TestManifest,
) -> HashMap<String, String> {
    let mut vars = build_variables(config, distro);
    load_manifest_overrides(&mut vars, manifest, distro);
    vars
}

/// One token in the manifest `${NAME}` template grammar.
///
/// A reference is the literal `${`, a name, and a closing `}`. A name matches
/// `[A-Za-z_][A-Za-z0-9_]*`: an ASCII letter or underscore followed by ASCII
/// letters, digits, or underscores. That is exactly the shape of every name the
/// harness defines today, both the built-ins inserted by [`build_variables`]
/// and every `distro_overrides` key in the integration manifests. Anything
/// else, including a bare `$`, an unterminated `${`, and a shell parameter
/// expansion such as `${GEN:-0}` or `${#VAR}`, is literal text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TemplateToken<'a> {
    /// Text with no `${` marker.
    Literal(&'a str),
    /// A well-formed `${NAME}` reference.
    Reference {
        /// The `NAME` between `${` and `}`.
        name: &'a str,
        /// The whole `${NAME}` text, emitted verbatim when `NAME` is unknown.
        raw: &'a str,
    },
    /// A `${` that does not introduce a well-formed reference.
    ///
    /// Emitted verbatim. Scanning resumes after the two marker characters, so a
    /// later well-formed reference in the same input still expands.
    Malformed(&'a str),
}

/// Left-to-right tokenizer for the manifest template grammar.
///
/// This is the single owner of the grammar; [`expand_variables`] and
/// [`contains_variable_reference`] both drive it.
struct TemplateTokenizer<'a> {
    input: &'a str,
    position: usize,
}

impl<'a> TemplateTokenizer<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, position: 0 }
    }
}

impl<'a> Iterator for TemplateTokenizer<'a> {
    type Item = TemplateToken<'a>;

    fn next(&mut self) -> Option<TemplateToken<'a>> {
        let input = self.input;
        if self.position >= input.len() {
            return None;
        }
        let start = self.position;
        let rest = &input[start..];
        let marker = match rest.find("${") {
            Some(relative) => start + relative,
            None => {
                self.position = input.len();
                return Some(TemplateToken::Literal(rest));
            }
        };
        // Emit any literal text before the marker first so the token stream is
        // contiguous and lossless.
        if marker > start {
            self.position = marker;
            return Some(TemplateToken::Literal(&input[start..marker]));
        }
        match parse_variable_reference(input, marker) {
            Some((name, end)) => {
                self.position = end;
                Some(TemplateToken::Reference {
                    name,
                    raw: &input[marker..end],
                })
            }
            None => {
                self.position = marker + 2;
                Some(TemplateToken::Malformed(&input[marker..marker + 2]))
            }
        }
    }
}

/// Parse the `${NAME}` reference at byte `marker`, if it is well formed.
///
/// `marker` indexes the `${` pair and must fall on a character boundary.
/// Returns the name and the index just past the closing `}`, or `None` when the
/// name is empty, contains a character outside the name charset, or is not
/// closed by `}`.
fn parse_variable_reference(input: &str, marker: usize) -> Option<(&str, usize)> {
    let name_start = marker + 2;
    let close = input[name_start..].find('}')? + name_start;
    let name = &input[name_start..close];
    is_template_name(name).then_some((name, close + 1))
}

/// Whether `name` is a valid manifest template name: `[A-Za-z_][A-Za-z0-9_]*`.
///
/// This is the single owner of the name rule. `parse_variable_reference` and
/// manifest load-time validation both consult it, so a `distro_overrides` key
/// that `${NAME}` can never address is rejected instead of silently ignored.
pub(crate) fn is_template_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    match bytes.next() {
        Some(first) if is_name_start(first) => bytes.all(is_name_continue),
        _ => false,
    }
}

/// Whether `byte` may begin a manifest variable name.
fn is_name_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

/// Whether `byte` may continue a manifest variable name.
fn is_name_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Whether `input` contains a manifest `${NAME}` template marker.
///
/// A marker is a `${`, whether or not it forms a well-formed reference, so a
/// malformed or truncated template is still reported rather than silently
/// accepted.
pub(crate) fn contains_variable_reference(input: &str) -> bool {
    TemplateTokenizer::new(input).any(|token| !matches!(token, TemplateToken::Literal(_)))
}

/// Replace `${VAR}` patterns in a string with values from the variable map.
///
/// The input is scanned once, left to right. A `${NAME}` whose `NAME` is in the
/// map is replaced by its value verbatim: the value is not rescanned, so a
/// value that itself contains `${...}` is emitted literally and nested
/// templates in values are not expanded. Unknown `${NAME}` references and
/// malformed markers are left as-is, which keeps unresolved references visible
/// for the existing unresolved-reference checks.
pub fn expand_variables(input: &str, vars: &HashMap<String, String>) -> String {
    let mut expanded = String::with_capacity(input.len());
    for token in TemplateTokenizer::new(input) {
        match token {
            TemplateToken::Literal(text) | TemplateToken::Malformed(text) => {
                expanded.push_str(text);
            }
            TemplateToken::Reference { name, raw } => {
                expanded.push_str(vars.get(name).map(String::as_str).unwrap_or(raw));
            }
        }
    }
    expanded
}

/// Resolve manifest variables in the string-bearing corpus authority before
/// recording target evidence.
pub fn expand_corpus_case(
    definition: &CorpusCaseDef,
    vars: &HashMap<String, String>,
) -> CorpusCaseDef {
    CorpusCaseDef {
        evidence_path: expand_variables(&definition.evidence_path, vars),
        source_profile: expand_variables(&definition.source_profile, vars),
        source_format: crate::config::corpus::CorpusSourceFormat::from_value(expand_variables(
            definition.source_format.as_str(),
            vars,
        )),
        digest_source: definition.digest_source,
        target: CorpusTargetDef {
            architecture: expand_variables(&definition.target.architecture, vars),
            init_system: expand_variables(&definition.target.init_system, vars),
            capabilities: definition
                .target
                .capabilities
                .iter()
                .map(|value| expand_variables(value, vars))
                .collect(),
        },
        coverage: definition.coverage.clone(),
        stages: definition.stages.clone(),
    }
}

/// Expand all variable references in an `Assertion`.
pub fn expand_assertion(assertion: &Assertion, vars: &HashMap<String, String>) -> Assertion {
    Assertion {
        exit_code: assertion.exit_code,
        exit_code_not: assertion.exit_code_not,
        stdout_contains: assertion
            .stdout_contains
            .as_ref()
            .map(|value| expand_variables(value, vars)),
        stdout_not_contains: assertion
            .stdout_not_contains
            .as_ref()
            .map(|value| expand_variables(value, vars)),
        stdout_contains_all: assertion.stdout_contains_all.as_ref().map(|values| {
            values
                .iter()
                .map(|value| expand_variables(value, vars))
                .collect()
        }),
        stdout_contains_any: assertion.stdout_contains_any.as_ref().map(|values| {
            values
                .iter()
                .map(|value| expand_variables(value, vars))
                .collect()
        }),
        stdout_contains_if_success: assertion
            .stdout_contains_if_success
            .as_ref()
            .map(|value| expand_variables(value, vars)),
        stdout_contains_any_if_success: assertion.stdout_contains_any_if_success.as_ref().map(
            |values| {
                values
                    .iter()
                    .map(|value| expand_variables(value, vars))
                    .collect()
            },
        ),
        stderr_contains: assertion
            .stderr_contains
            .as_ref()
            .map(|value| expand_variables(value, vars)),
        stderr_not_contains: assertion
            .stderr_not_contains
            .as_ref()
            .map(|value| expand_variables(value, vars)),
        file_exists: assertion
            .file_exists
            .as_ref()
            .map(|value| expand_variables(value, vars)),
        file_not_exists: assertion
            .file_not_exists
            .as_ref()
            .map(|value| expand_variables(value, vars)),
        file_checksum: assertion
            .file_checksum
            .as_ref()
            .map(|checksum| FileChecksum {
                path: expand_variables(&checksum.path, vars),
                sha256: expand_variables(&checksum.sha256, vars),
            }),
        stdout_json: assertion.stdout_json.as_ref().map(|checks| {
            checks
                .iter()
                .map(|check| JsonAssertion {
                    pointer: expand_variables(&check.pointer, vars),
                    expected: match &check.expected {
                        JsonExpectation::Equals(value) => {
                            JsonExpectation::Equals(expand_json_value(value, vars))
                        }
                        JsonExpectation::Null => JsonExpectation::Null,
                    },
                    // Number tokens are unaffected by variable expansion; an
                    // expanded pointer prefix is applied when the check runs.
                    numbers: check.numbers.clone(),
                })
                .collect()
        }),
    }
}

/// Expand `${VAR}` references in every string leaf of a JSON value.
///
/// Object keys are structural and are not expanded.
fn expand_json_value(
    value: &serde_json::Value,
    vars: &HashMap<String, String>,
) -> serde_json::Value {
    match value {
        serde_json::Value::String(value) => {
            serde_json::Value::String(expand_variables(value, vars))
        }
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .iter()
                .map(|value| expand_json_value(value, vars))
                .collect(),
        ),
        serde_json::Value::Object(object) => serde_json::Value::Object(
            object
                .iter()
                .map(|(key, value)| (key.clone(), expand_json_value(value, vars)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Expand all variable references in a `QemuBoot` configuration.
pub fn expand_qemu_boot(config: &QemuBoot, vars: &HashMap<String, String>) -> QemuBoot {
    QemuBoot {
        image: expand_variables(&config.image, vars),
        local_image_path: config
            .local_image_path
            .as_ref()
            .map(|path| expand_variables(path, vars)),
        image_format: config.image_format,
        stage_conary: config.stage_conary,
        scratch_disk_mb: config.scratch_disk_mb,
        copy_to_guest: config
            .copy_to_guest
            .iter()
            .map(|copy| QemuGuestCopy {
                source: expand_variables(&copy.source, vars),
                dest: expand_variables(&copy.dest, vars),
            })
            .collect(),
        copy_from_guest: config
            .copy_from_guest
            .iter()
            .map(|copy| QemuGuestCopy {
                source: expand_variables(&copy.source, vars),
                dest: expand_variables(&copy.dest, vars),
            })
            .collect(),
        memory_mb: config.memory_mb,
        timeout_seconds: config.timeout_seconds,
        ssh_port: config.ssh_port,
        commands: config
            .commands
            .iter()
            .map(|cmd| expand_variables(cmd, vars))
            .collect(),
        expect_output: config
            .expect_output
            .iter()
            .map(|s| expand_variables(s, vars))
            .collect(),
    }
}

#[cfg(test)]
#[path = "variables/tests.rs"]
mod tests;
