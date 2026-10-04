// apps/conary-test/src/config/tests/native_corpus/typed_install_effect.rs

use super::super::{load_manifest, remi_manifest_path};
use crate::config::manifest::Assertion;
use crate::engine::assertions::evaluate_assertion;

const INSTALL_COMMAND: &str = r#". /tmp/native-pm-parity/native-fixture.env && CONARY_TEST_SKIP_GENERATION_MOUNT=1 ${CONARY_BIN} install "$NATIVE_PKG_FILE" --convert-to-ccs --from ${native_profile} --db-path ${DB_PATH} --root /conary --no-deps --yes --sandbox always"#;
const SELECTED_GENERATION_COMMAND: &str = concat!(
    "/opt/remi-tests/fixtures/native/assert-selected-generation.py --root /conary",
    " --expect-sha256 /etc/phase4-runtime-fixture/app.conf=1da0b50cb027387347265437a11956c1433788d045e4c63b379a1e0740882e7c",
    " --expect-sha256 /usr/bin/phase4-runtime-fixture=517631de24336343a6aaf1a8f704d326299c14c619fe8c8d75d17824d074bd7f",
    " --expect-sha256 /usr/include/phase4-runtime-fixture/api.h=88904c275ae0f26a566c03f488ae82869c47ac921fb6f0cf7979d5845f199c88",
);

#[test]
fn native_parity_install_effect_requires_exit_and_selected_payload() {
    let manifest = load_manifest(&remi_manifest_path("phase4-native-pm-parity.toml"))
        .expect("load native parity manifest");
    let install = manifest
        .test
        .iter()
        .find(|test| test.id == "TNPM03")
        .expect("TNPM03 must own the local native install");
    assert_eq!(install.step.len(), 2, "TNPM03 must keep exactly two steps");

    let first = &install.step[0];
    assert_eq!(first.run.as_deref(), Some(INSTALL_COMMAND));
    let assertion = first.assert.as_ref().expect("install needs an assertion");
    assert_exit_only(assertion);

    let legacy = Assertion {
        stdout_contains_any: Some(vec![
            "Successfully installed".into(),
            "Installed".into(),
            "phase4-runtime-fixture".into(),
        ]),
        ..assertion.clone()
    };
    assert!(
        evaluate_assertion(
            &legacy,
            0,
            "Failed to install phase4-runtime-fixture: selected generation unavailable",
            "",
        )
        .is_ok(),
        "the removed matcher accepted a failure sentence at exit zero"
    );
    assert!(
        evaluate_assertion(assertion, 1, "Installed phase4-runtime-fixture", "").is_err(),
        "the loaded assertion must reject a nonzero install exit"
    );

    let second = &install.step[1];
    assert_eq!(second.run.as_deref(), Some(SELECTED_GENERATION_COMMAND));
    assert_exit_only(
        second
            .assert
            .as_ref()
            .expect("selected payload needs an assertion"),
    );
}

fn assert_exit_only(assertion: &Assertion) {
    assert_eq!(assertion.exit_code, Some(0));
    assert!(assertion.exit_code_not.is_none());
    assert!(assertion.stdout_contains.is_none());
    assert!(assertion.stdout_not_contains.is_none());
    assert!(assertion.stdout_contains_all.is_none());
    assert!(assertion.stdout_contains_any.is_none());
    assert!(assertion.stdout_contains_if_success.is_none());
    assert!(assertion.stdout_contains_any_if_success.is_none());
    assert!(assertion.stdout_json.is_none());
    assert!(assertion.stderr_contains.is_none());
    assert!(assertion.stderr_not_contains.is_none());
    assert!(assertion.file_exists.is_none());
    assert!(assertion.file_not_exists.is_none());
    assert!(assertion.file_checksum.is_none());
}
