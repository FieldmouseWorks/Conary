// apps/conary-test/src/engine/runner/tests.rs

#![cfg(test)]

use super::*;
use crate::config::distro::{
    DistroConfig, FixtureConfig, GlobalConfig, PathsConfig, RemiConfig, SetupConfig, TestPackage,
};
use crate::config::manifest::{
    Assertion, FileChecksum, KillAfterLog, QemuBoot, QemuImageFormat, ResourceConstraints,
    StaticFixture, SuiteDef, TestDef, TestManifest, TestStep,
};
use crate::container::backend::ExecResult;
use crate::container::mock::MockBackend;
use crate::engine::variables::{FIXTURE_DIR_CONFIG_KEY, FixturePreflightError};

// -- Helpers --

fn test_config() -> GlobalConfig {
    let mut distros = HashMap::new();
    distros.insert(
        "fedora44".to_string(),
        DistroConfig {
            remi_distro: "fedora-44".to_string(),
            repo_name: "remi-fedora-44".to_string(),
            build_context: crate::config::DistroBuildContext::Binary,
            containerfile: None,
            test_packages: vec![TestPackage {
                package: "conary-test-fixture".to_string(),
                binary: "/usr/bin/true".to_string(),
            }],
            release_root: None,
            target_root: None,
        },
    );

    GlobalConfig {
        remi: RemiConfig {
            endpoint: "https://remi.conary.io".to_string(),
        },
        paths: PathsConfig {
            db: "/tmp/conary-test.db".to_string(),
            conary_bin: "/usr/local/bin/conary".to_string(),
            test_hooks_conary_bin: None,
            results_dir: "/tmp/results".to_string(),
            fixture_dir: Some("/opt/remi-tests/fixtures".to_string()),
        },
        setup: SetupConfig::default(),
        distros,
        fixtures: Some(FixtureConfig {
            package: Some("conary-test-fixture".to_string()),
            file: Some("/usr/share/conary-test/hello.txt".to_string()),
            added_file: Some("/usr/share/conary-test/added.txt".to_string()),
            marker: Some("/var/lib/conary-test/installed".to_string()),
            v1_version: Some("1.0.0".to_string()),
            v1_ccs_file: Some("conary-test-fixture-1.0.0-1.ccs".to_string()),
            v1_hello_sha256: Some(
                "18933c865fcf7230f8ea99b059747facc14285b7ed649758115f9c9a73f42a53".to_string(),
            ),
            v2_version: Some("2.0.0".to_string()),
            v2_ccs_file: Some("conary-test-fixture-2.0.0-1.ccs".to_string()),
            v2_hello_sha256: Some(
                "bd80c5e8a7138bd13d0f10e1358bda6f9727c266b6909d4b6c9293ab141ec1db".to_string(),
            ),
            v2_added_sha256: Some(
                "9767b0b4d55db9aee6638c9875b5cefea50c952cc77fbc5703ebc866b0daba3c".to_string(),
            ),
        }),
    }
}

fn simple_step_run(cmd: &str, assertion: Option<Assertion>) -> TestStep {
    TestStep {
        run: Some(cmd.to_string()),
        assert: assertion,
        ..TestStep::default()
    }
}

fn simple_step_kill_after_log(config: KillAfterLog, assertion: Option<Assertion>) -> TestStep {
    TestStep {
        kill_after_log: Some(config),
        assert: assertion,
        ..TestStep::default()
    }
}

fn simple_step_qemu_boot(config: QemuBoot, assertion: Option<Assertion>) -> TestStep {
    TestStep {
        qemu_boot: Some(config),
        assert: assertion,
        ..TestStep::default()
    }
}

fn make_assertion(exit_code: Option<i32>, stdout_contains: Option<&str>) -> Assertion {
    Assertion {
        exit_code,
        stdout_contains: stdout_contains.map(String::from),
        ..Assertion::default()
    }
}

fn make_manifest(tests: Vec<TestDef>) -> TestManifest {
    TestManifest {
        suite: SuiteDef {
            name: "test-suite".to_string(),
            phase: 1,
            setup: Vec::new(),
            requires_fixtures: Vec::new(),
            mock_server: None,
            timeout: None,
            corpus: None,
        },
        test: tests,
        distro_overrides: HashMap::new(),
    }
}

// -- Tests --

#[tokio::test]
async fn test_runner_passes_on_success() {
    let backend = MockBackend::new(vec![ExecResult {
        exit_code: 0,
        stdout: "ok".to_string(),
        stderr: String::new(),
    }]);

    let manifest = make_manifest(vec![TestDef {
        id: "T01".to_string(),
        name: "pass_test".to_string(),
        description: "should pass".to_string(),
        timeout: 30,
        flaky: None,
        retries: None,
        retry_delay_ms: None,
        step: vec![simple_step_run(
            "echo ok",
            Some(make_assertion(Some(0), Some("ok"))),
        )],
        resources: None,
        depends_on: None,
        fatal: None,
        group: None,
        skip: None,
        requires: Vec::new(),
        corpus: None,
    }]);

    let mut runner = TestRunner::new(test_config(), "fedora44".to_string());
    let suite = runner
        .run(&manifest, &backend, &"ctr-1".to_string(), None)
        .await
        .unwrap();

    assert_eq!(suite.passed(), 1);
    assert_eq!(suite.failed(), 0);
    assert_eq!(suite.results[0].status, TestStatus::Passed);
}

#[tokio::test]
async fn suite_setup_executes_before_tests() {
    let backend = MockBackend::new(vec![
        ExecResult {
            exit_code: 0,
            stdout: "setup ok".to_string(),
            stderr: String::new(),
        },
        ExecResult {
            exit_code: 0,
            stdout: "test ok".to_string(),
            stderr: String::new(),
        },
    ]);

    let manifest = TestManifest {
        suite: SuiteDef {
            name: "setup-suite".to_string(),
            phase: 4,
            setup: vec![simple_step_run(
                "echo setup ok",
                Some(make_assertion(Some(0), Some("setup ok"))),
            )],
            requires_fixtures: Vec::new(),
            mock_server: None,
            timeout: None,
            corpus: None,
        },
        test: vec![TestDef {
            id: "TSETUP".to_string(),
            name: "uses_setup".to_string(),
            description: "setup should run before tests".to_string(),
            timeout: 30,
            flaky: None,
            retries: None,
            retry_delay_ms: None,
            step: vec![simple_step_run(
                "echo test ok",
                Some(make_assertion(Some(0), Some("test ok"))),
            )],
            resources: None,
            depends_on: None,
            fatal: None,
            group: None,
            skip: None,
            requires: Vec::new(),
            corpus: None,
        }],
        distro_overrides: HashMap::new(),
    };

    let mut runner = TestRunner::new(test_config(), "fedora44".to_string());
    let suite = runner
        .run(&manifest, &backend, &"ctr-setup".to_string(), None)
        .await
        .unwrap();

    assert_eq!(suite.passed(), 1);
    assert_eq!(suite.failed(), 0);
}

#[tokio::test]
async fn suite_setup_assertion_failure_preserves_command_output() {
    let backend = MockBackend::new(vec![ExecResult {
        exit_code: 1,
        stdout: "setup stdout evidence".to_string(),
        stderr: "setup stderr evidence".to_string(),
    }]);

    let manifest = TestManifest {
        suite: SuiteDef {
            name: "setup-failure-suite".to_string(),
            phase: 4,
            setup: vec![simple_step_run(
                "failing setup",
                Some(make_assertion(Some(0), None)),
            )],
            requires_fixtures: Vec::new(),
            mock_server: None,
            timeout: None,
            corpus: None,
        },
        test: Vec::new(),
        distro_overrides: HashMap::new(),
    };

    let mut runner = TestRunner::new(test_config(), "fedora44".to_string());
    let error = runner
        .run(&manifest, &backend, &"ctr-setup-failure".to_string(), None)
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("suite setup assertion failed"));
    assert!(error.contains("setup stdout evidence"));
    assert!(error.contains("setup stderr evidence"));
}

fn trivial_manifest(name: &str, fixtures: Vec<StaticFixture>) -> TestManifest {
    let mut manifest = make_manifest(vec![TestDef {
        id: format!("T-{name}"),
        name: name.to_string(),
        description: "fixture install bookkeeping".to_string(),
        timeout: 30,
        flaky: None,
        retries: None,
        retry_delay_ms: None,
        step: vec![simple_step_run("true", None)],
        resources: None,
        depends_on: None,
        fatal: None,
        group: None,
        skip: None,
        requires: Vec::new(),
        corpus: None,
    }]);
    manifest.suite.name = name.to_string();
    manifest.suite.requires_fixtures = fixtures;
    manifest
}

/// The exact argv the harness's `sh -c` receives to install the shell fixture.
fn shell_fixture_install_argv(config: &GlobalConfig) -> Vec<String> {
    let fixture_dir = config
        .paths
        .fixture_dir
        .as_deref()
        .expect("fixture directory");
    vec![
        "sh".to_string(),
        "-c".to_string(),
        format!(
            "{} ccs install {fixture_dir}/conary-test-shell/output/conary-test-shell-1.0.0-1.ccs \
             --policy {fixture_dir}/ccs-test-authority/trust-policy.toml \
             --sandbox always --yes --db-path {}",
            config.paths.conary_bin, config.paths.db,
        ),
    ]
}

/// The exact argv the harness's `sh -c` receives to install the base fixture.
fn base_fixture_install_argv(config: &GlobalConfig) -> Vec<String> {
    let fixture_dir = config
        .paths
        .fixture_dir
        .as_deref()
        .expect("fixture directory");
    vec![
        "sh".to_string(),
        "-c".to_string(),
        format!(
            "{} ccs install {fixture_dir}/conary-test-base/output/conary-test-base-1.0.0-1.ccs \
             --policy {fixture_dir}/ccs-test-authority/trust-policy.toml \
             --sandbox always --yes --db-path {}",
            config.paths.conary_bin, config.paths.db,
        ),
    ]
}

/// Every exec the harness issues to install the declared shell fixture, matched
/// by the exact argv the setup path used before the harness owned installation.
fn shell_fixture_install_calls(backend: &MockBackend, config: &GlobalConfig) -> Vec<Vec<String>> {
    let expected_call = shell_fixture_install_argv(config);
    backend
        .exec_calls()
        .into_iter()
        .filter(|call| call == &expected_call)
        .collect()
}

#[tokio::test]
async fn declared_fixture_installs_once_per_container() {
    let config = test_config();
    let backend = MockBackend::new(Vec::new());
    let first = trivial_manifest("first", vec![StaticFixture::Shell]);
    let second = trivial_manifest("second", vec![StaticFixture::Shell]);
    let mut installed_fixtures = InstalledFixtures::default();
    let mut runner = TestRunner::new(config.clone(), "fedora44".to_string());

    for manifest in [&first, &second] {
        runner
            .run_with_cancel(
                manifest,
                &backend,
                &"ctr-shared".to_string(),
                None,
                None,
                None,
                None,
                &mut installed_fixtures,
            )
            .await
            .unwrap();
    }

    assert_eq!(
        shell_fixture_install_calls(&backend, &config).len(),
        1,
        "a fixture two manifests declare must install once per container"
    );
}

/// The harness installs declared fixtures in `StaticFixture::ALL` order, so the
/// `/bin/sh` provider is present before the base whose init may need it.
#[tokio::test]
async fn declared_fixtures_install_shell_before_base() {
    let config = test_config();
    let backend = MockBackend::new(Vec::new());
    let manifest = trivial_manifest("ordered", vec![StaticFixture::Shell, StaticFixture::Base]);
    let mut installed_fixtures = InstalledFixtures::default();
    let mut runner = TestRunner::new(config.clone(), "fedora44".to_string());

    runner
        .run_with_cancel(
            &manifest,
            &backend,
            &"ctr-ordered".to_string(),
            None,
            None,
            None,
            None,
            &mut installed_fixtures,
        )
        .await
        .unwrap();

    let calls = backend.exec_calls();
    let shell = shell_fixture_install_argv(&config);
    let base = base_fixture_install_argv(&config);
    let shell_index = calls
        .iter()
        .position(|call| call == &shell)
        .expect("the shell fixture must install");
    let base_index = calls
        .iter()
        .position(|call| call == &base)
        .expect("the base fixture must install");
    assert!(
        shell_index < base_index,
        "the harness must install /bin/sh before the base fixture"
    );
}

#[tokio::test]
async fn undeclared_manifest_installs_no_fixture() {
    let config = test_config();
    let backend = MockBackend::new(Vec::new());
    let manifest = trivial_manifest("plain", Vec::new());
    let mut installed_fixtures = InstalledFixtures::default();
    let mut runner = TestRunner::new(config.clone(), "fedora44".to_string());

    runner
        .run_with_cancel(
            &manifest,
            &backend,
            &"ctr-plain".to_string(),
            None,
            None,
            None,
            None,
            &mut installed_fixtures,
        )
        .await
        .unwrap();

    assert!(
        shell_fixture_install_calls(&backend, &config).is_empty(),
        "a manifest that declares no fixture must install none"
    );
}

/// A declared fixture with no `paths.fixture_dir` must fail the typed preflight
/// before any container exec, never at install time with a literal placeholder.
#[tokio::test]
async fn declared_fixture_without_fixture_dir_fails_preflight_before_exec() {
    let mut config = test_config();
    config.paths.fixture_dir = None;
    let backend = MockBackend::new(Vec::new());
    let manifest = trivial_manifest("missing-fixture-dir", vec![StaticFixture::Shell]);
    let mut installed_fixtures = InstalledFixtures::default();
    let mut runner = TestRunner::new(config, "fedora44".to_string());

    let error = runner
        .run_with_cancel(
            &manifest,
            &backend,
            &"ctr-missing-fixture-dir".to_string(),
            None,
            None,
            None,
            None,
            &mut installed_fixtures,
        )
        .await
        .expect_err("a declared fixture without fixture_dir must fail");

    assert!(
        matches!(
            error.downcast_ref::<FixturePreflightError>(),
            Some(FixturePreflightError::MissingFixtureDir {
                fixture: StaticFixture::Shell,
                config_key: FIXTURE_DIR_CONFIG_KEY,
                ..
            })
        ),
        "unexpected error: {error}"
    );
    assert!(
        backend.exec_calls().is_empty(),
        "preflight must fail before the harness issues any container exec"
    );
}

/// Positive control for the preflight: the same declared fixture with
/// `fixture_dir` set clears it and installs normally.
#[tokio::test]
async fn declared_fixture_with_fixture_dir_passes_preflight() {
    let config = test_config();
    assert!(config.paths.fixture_dir.is_some());
    let backend = MockBackend::new(Vec::new());
    let manifest = trivial_manifest("fixture-dir-set", vec![StaticFixture::Shell]);
    let mut installed_fixtures = InstalledFixtures::default();
    let mut runner = TestRunner::new(config, "fedora44".to_string());

    runner
        .run_with_cancel(
            &manifest,
            &backend,
            &"ctr-fixture-dir-set".to_string(),
            None,
            None,
            None,
            None,
            &mut installed_fixtures,
        )
        .await
        .expect("a declared fixture with fixture_dir set must clear preflight");
}

#[tokio::test]
async fn failed_fixture_install_reports_typed_error() {
    let backend = MockBackend::new(vec![ExecResult {
        exit_code: 17,
        stdout: String::new(),
        stderr: "ccs refused the install".to_string(),
    }]);
    let manifest = trivial_manifest("failing", vec![StaticFixture::Shell]);
    let mut installed_fixtures = InstalledFixtures::default();
    let mut runner = TestRunner::new(test_config(), "fedora44".to_string());

    let error = runner
        .run_with_cancel(
            &manifest,
            &backend,
            &"ctr-failing".to_string(),
            None,
            None,
            None,
            None,
            &mut installed_fixtures,
        )
        .await
        .unwrap_err();

    let typed = error
        .downcast_ref::<FixtureInstallError>()
        .expect("a declared fixture failure must stay typed");
    assert!(matches!(
        typed,
        FixtureInstallError::NonZeroExit {
            fixture: StaticFixture::Shell,
            exit_code: 17,
            ..
        }
    ));
}

#[tokio::test]
async fn resource_scoped_test_installs_declared_fixture_in_its_disposable_container() {
    let config = test_config();
    // `initialize_container_state` runs one init, one repo-removal per dropped
    // default, one removal per unselected public profile, and one seed
    // verification. A phase-1 suite keeps the packaged seed, so no Remi override
    // runs. The seed verification is last and must report exactly one source.
    let setup_exec_count = 1
        + config.setup.remove_default_repos.len()
        + conary_core::repository::supported_profiles::public_profiles()
            .len()
            .saturating_sub(1)
        + 1;

    let fixture_installed = || ExecResult {
        exit_code: 0,
        stdout: "fixture installed".to_string(),
        stderr: String::new(),
    };
    let mut exec_results = vec![fixture_installed()];
    exec_results.extend((0..setup_exec_count).map(|index| ExecResult {
        exit_code: 0,
        stdout: if index + 1 == setup_exec_count {
            "1\n".to_string()
        } else {
            String::new()
        },
        stderr: String::new(),
    }));
    exec_results.push(fixture_installed());
    exec_results.push(ExecResult {
        exit_code: 0,
        stdout: "resource ok".to_string(),
        stderr: String::new(),
    });
    let backend = MockBackend::new(exec_results);

    let manifest = TestManifest {
        suite: SuiteDef {
            name: "resource-fixture".to_string(),
            phase: 1,
            setup: Vec::new(),
            requires_fixtures: vec![StaticFixture::Shell],
            mock_server: None,
            timeout: None,
            corpus: None,
        },
        test: vec![TestDef {
            id: "T-resource-fixture".to_string(),
            name: "resource_fixture".to_string(),
            description: "the disposable container installs the declared fixture".to_string(),
            timeout: 30,
            flaky: None,
            retries: None,
            retry_delay_ms: None,
            step: vec![simple_step_run(
                "echo resource ok",
                Some(make_assertion(Some(0), Some("resource ok"))),
            )],
            resources: Some(ResourceConstraints {
                tmpfs_size_mb: None,
                memory_limit_mb: Some(512),
                network_isolated: Some(false),
            }),
            depends_on: None,
            fatal: None,
            group: None,
            skip: None,
            requires: Vec::new(),
            corpus: None,
        }],
        distro_overrides: HashMap::new(),
    };

    let base_container_config = ContainerConfig {
        image: "mock-image".to_string(),
        ..Default::default()
    };
    let mut runner = TestRunner::new(config.clone(), "fedora44".to_string());
    let suite = runner
        .run(
            &manifest,
            &backend,
            &"ctr-base".to_string(),
            Some(&base_container_config),
        )
        .await
        .unwrap();

    assert_eq!(suite.passed(), 1);
    assert_eq!(suite.failed(), 0);
    assert_eq!(
        backend.created_containers().len(),
        1,
        "the resource-scoped test runs in one disposable container"
    );

    let calls = backend.exec_calls();
    let install_argv = shell_fixture_install_argv(&config);
    let install_positions = calls
        .iter()
        .enumerate()
        .filter(|(_, call)| call.as_slice() == install_argv.as_slice())
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    assert_eq!(
        install_positions.len(),
        2,
        "the base container and the disposable container each install the declared fixture"
    );

    let disposable_init = calls
        .iter()
        .position(|call| {
            call.len() == 3
                && call[0].as_str() == "sh"
                && call[1].as_str() == "-c"
                && call[2].contains("system init")
        })
        .expect("the disposable container is initialized");
    let fixture_in_disposable = install_positions[1];
    let test_step = calls
        .iter()
        .position(|call| {
            call.len() == 3
                && call[0].as_str() == "sh"
                && call[1].as_str() == "-c"
                && call[2].as_str() == "echo resource ok"
        })
        .expect("the test's own step runs");

    assert!(
        install_positions[0] < disposable_init,
        "the shared container's install is not the disposable one"
    );
    assert!(
        disposable_init < fixture_in_disposable,
        "the disposable container is initialized before its fixture install"
    );
    assert_eq!(
        fixture_in_disposable + 1,
        test_step,
        "the fixture install is the last thing before the test's own step"
    );
}

#[tokio::test]
async fn test_runner_fails_on_bad_exit_code() {
    let backend = MockBackend::new(vec![ExecResult {
        exit_code: 1,
        stdout: String::new(),
        stderr: "error".to_string(),
    }]);

    let manifest = make_manifest(vec![TestDef {
        id: "T01".to_string(),
        name: "fail_test".to_string(),
        description: "should fail".to_string(),
        timeout: 30,
        flaky: None,
        retries: None,
        retry_delay_ms: None,
        step: vec![simple_step_run(
            "false",
            Some(make_assertion(Some(0), None)),
        )],
        resources: None,
        depends_on: None,
        fatal: None,
        group: None,
        skip: None,
        requires: Vec::new(),
        corpus: None,
    }]);

    let mut runner = TestRunner::new(test_config(), "fedora44".to_string());
    let suite = runner
        .run(&manifest, &backend, &"ctr-1".to_string(), None)
        .await
        .unwrap();

    assert_eq!(suite.passed(), 0);
    assert_eq!(suite.failed(), 1);
    assert_eq!(suite.results[0].status, TestStatus::Failed);
    assert!(
        suite.results[0]
            .message
            .as_ref()
            .unwrap()
            .contains("exit code")
    );
    assert!(
        suite.results[0]
            .message
            .as_ref()
            .unwrap()
            .contains("step 1")
    );
}

#[tokio::test]
async fn test_runner_skips_on_dep_failure() {
    // T01 fails, T02 depends on T01 => T02 skipped.
    let backend = MockBackend::new(vec![ExecResult {
        exit_code: 1,
        stdout: String::new(),
        stderr: String::new(),
    }]);

    let manifest = make_manifest(vec![
        TestDef {
            id: "T01".to_string(),
            name: "dep_fail".to_string(),
            description: "will fail".to_string(),
            timeout: 30,
            flaky: None,
            retries: None,
            retry_delay_ms: None,
            step: vec![simple_step_run(
                "false",
                Some(make_assertion(Some(0), None)),
            )],
            resources: None,
            depends_on: None,
            fatal: None,
            group: None,
            skip: None,
            requires: Vec::new(),
            corpus: None,
        },
        TestDef {
            id: "T02".to_string(),
            name: "depends_on_t01".to_string(),
            description: "should be skipped".to_string(),
            timeout: 30,
            flaky: None,
            retries: None,
            retry_delay_ms: None,
            step: vec![simple_step_run("echo hello", None)],
            resources: None,
            depends_on: Some(vec!["T01".to_string()]),
            fatal: None,
            group: None,
            skip: None,
            requires: Vec::new(),
            corpus: None,
        },
    ]);

    let mut runner = TestRunner::new(test_config(), "fedora44".to_string());
    let suite = runner
        .run(&manifest, &backend, &"ctr-1".to_string(), None)
        .await
        .unwrap();

    assert_eq!(suite.failed(), 1);
    assert_eq!(suite.skipped(), 1);
    assert_eq!(suite.results[1].status, TestStatus::Skipped);
    assert!(suite.results[1].message.as_ref().unwrap().contains("T01"));
}

#[tokio::test]
async fn test_runner_skips_when_composefs_runtime_requirement_is_missing() {
    let backend = MockBackend::new(vec![ExecResult {
        exit_code: 1,
        stdout: String::new(),
        stderr: "missing erofs".to_string(),
    }]);

    let manifest = make_manifest(vec![TestDef {
        id: "T51".to_string(),
        name: "build_generation".to_string(),
        description: "requires composefs".to_string(),
        timeout: 30,
        flaky: None,
        retries: None,
        retry_delay_ms: None,
        step: vec![simple_step_run(
            "conary system generation build",
            Some(make_assertion(Some(0), None)),
        )],
        resources: None,
        depends_on: None,
        fatal: None,
        group: None,
        skip: None,
        requires: vec!["composefs_runtime".to_string()],
        corpus: None,
    }]);

    let mut runner = TestRunner::new(test_config(), "fedora44".to_string());
    let suite = runner
        .run(&manifest, &backend, &"ctr-1".to_string(), None)
        .await
        .unwrap();

    assert_eq!(suite.failed(), 0);
    assert_eq!(suite.skipped(), 1);
    assert_eq!(suite.results[0].status, TestStatus::Skipped);
    assert!(
        suite.results[0]
            .message
            .as_ref()
            .unwrap()
            .contains("composefs runtime")
    );
    assert_eq!(backend.exec_calls().len(), 1, "only the probe should run");
}

#[tokio::test]
async fn test_runner_kill_after_log() {
    let backend = MockBackend::new(Vec::new()).with_detached_exec(
        "exec-1",
        vec!["Preparing install", "Deploying files", "more output"],
        ExecResult {
            exit_code: 137,
            stdout: "Preparing install\nDeploying files\n".to_string(),
            stderr: "Killed\n".to_string(),
        },
    );

    let manifest = make_manifest(vec![TestDef {
        id: "T87".to_string(),
        name: "sigkill_mid_install".to_string(),
        description: "kills the conary process after matching a log line".to_string(),
        timeout: 30,
        flaky: None,
        retries: None,
        retry_delay_ms: None,
        step: vec![simple_step_kill_after_log(
            KillAfterLog {
                conary: "ccs install ${PKG}".to_string(),
                pattern: "Deploying files".to_string(),
                timeout_seconds: 5,
            },
            Some(Assertion {
                exit_code_not: Some(0),
                ..Assertion::default()
            }),
        )],
        resources: None,
        depends_on: None,
        fatal: None,
        group: None,
        skip: None,
        requires: Vec::new(),
        corpus: None,
    }]);

    let mut runner = TestRunner::new(test_config(), "fedora44".to_string());
    let mut overrides = HashMap::new();
    overrides.insert("PKG".to_string(), "pkg.ccs".to_string());
    let mut manifest = manifest;
    manifest
        .distro_overrides
        .insert("fedora44".to_string(), overrides);

    let suite = runner
        .run(&manifest, &backend, &"ctr-1".to_string(), None)
        .await
        .unwrap();

    assert_eq!(suite.failed(), 0);
    assert_eq!(suite.passed(), 1);
    assert_eq!(
        backend.killed_execs().as_slice(),
        [("exec-1".to_string(), "SIGKILL".to_string())]
    );
    let detached_calls = backend.detached_calls();
    assert_eq!(detached_calls.len(), 1);
    assert!(
        detached_calls[0]
            .join(" ")
            .contains("/usr/local/bin/conary ccs install pkg.ccs")
    );
}

#[tokio::test]
async fn test_runner_flaky_majority_pass() {
    let backend = MockBackend::new(vec![
        ExecResult {
            exit_code: 1,
            stdout: String::new(),
            stderr: "first fail".to_string(),
        },
        ExecResult {
            exit_code: 0,
            stdout: "ok".to_string(),
            stderr: String::new(),
        },
        ExecResult {
            exit_code: 0,
            stdout: "ok".to_string(),
            stderr: String::new(),
        },
    ]);

    let manifest = make_manifest(vec![TestDef {
        id: "T94".to_string(),
        name: "flaky_majority_pass".to_string(),
        description: "passes when most attempts succeed".to_string(),
        timeout: 30,
        flaky: Some(true),
        retries: Some(3),
        retry_delay_ms: None,
        step: vec![simple_step_run(
            "echo ok",
            Some(make_assertion(Some(0), Some("ok"))),
        )],
        resources: None,
        depends_on: None,
        fatal: None,
        group: None,
        skip: None,
        requires: Vec::new(),
        corpus: None,
    }]);

    let mut runner = TestRunner::new(test_config(), "fedora44".to_string());
    let suite = runner
        .run(&manifest, &backend, &"ctr-1".to_string(), None)
        .await
        .unwrap();

    assert_eq!(suite.passed(), 1);
    assert_eq!(suite.failed(), 0);
    assert!(
        suite.results[0]
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("2/3")
    );
}

#[tokio::test]
async fn test_runner_flaky_majority_fail() {
    let backend = MockBackend::new(vec![
        ExecResult {
            exit_code: 1,
            stdout: String::new(),
            stderr: "first fail".to_string(),
        },
        ExecResult {
            exit_code: 1,
            stdout: String::new(),
            stderr: "second fail".to_string(),
        },
        ExecResult {
            exit_code: 0,
            stdout: "ok".to_string(),
            stderr: String::new(),
        },
    ]);

    let manifest = make_manifest(vec![TestDef {
        id: "T95".to_string(),
        name: "flaky_majority_fail".to_string(),
        description: "fails when most attempts fail".to_string(),
        timeout: 30,
        flaky: Some(true),
        retries: Some(3),
        retry_delay_ms: None,
        step: vec![simple_step_run(
            "echo ok",
            Some(make_assertion(Some(0), Some("ok"))),
        )],
        resources: None,
        depends_on: None,
        fatal: None,
        group: None,
        skip: None,
        requires: Vec::new(),
        corpus: None,
    }]);

    let mut runner = TestRunner::new(test_config(), "fedora44".to_string());
    let suite = runner
        .run(&manifest, &backend, &"ctr-1".to_string(), None)
        .await
        .unwrap();

    assert_eq!(suite.passed(), 0);
    assert_eq!(suite.failed(), 1);
    assert!(
        suite.results[0]
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("failed majority")
    );
}

#[tokio::test]
async fn test_resource_scoped_flaky_retries_use_fresh_container() {
    let config = test_config();
    let setup_exec_count = 1
        + config.setup.remove_default_repos.len()
        + conary_core::repository::supported_profiles::public_profiles()
            .len()
            .saturating_sub(1)
        + 1;
    let successful_setup = |index| ExecResult {
        exit_code: 0,
        stdout: if index + 1 == setup_exec_count {
            "1\n".to_owned()
        } else {
            String::new()
        },
        stderr: String::new(),
    };
    let mut exec_results = Vec::new();
    exec_results.extend((0..setup_exec_count).map(successful_setup));
    exec_results.push(ExecResult {
        exit_code: 1,
        stdout: String::new(),
        stderr: "first attempt".to_string(),
    });
    exec_results.extend((0..setup_exec_count).map(successful_setup));
    exec_results.push(ExecResult {
        exit_code: 1,
        stdout: "ok".to_string(),
        stderr: String::new(),
    });
    let backend = MockBackend::new(exec_results);

    let manifest = TestManifest {
        suite: SuiteDef {
            name: "resource-flaky".to_string(),
            phase: 2,
            setup: Vec::new(),
            requires_fixtures: Vec::new(),
            mock_server: None,
            timeout: None,
            corpus: None,
        },
        test: vec![TestDef {
            id: "T-resource-flaky".to_string(),
            name: "resource_flaky".to_string(),
            description: "retries in fresh containers".to_string(),
            timeout: 30,
            flaky: Some(true),
            retries: Some(3),
            retry_delay_ms: None,
            step: vec![simple_step_run(
                "echo ok",
                Some(make_assertion(Some(0), Some("ok"))),
            )],
            resources: Some(ResourceConstraints {
                tmpfs_size_mb: None,
                memory_limit_mb: Some(512),
                network_isolated: Some(false),
            }),
            depends_on: None,
            fatal: None,
            group: None,
            skip: None,
            requires: Vec::new(),
            corpus: None,
        }],
        distro_overrides: HashMap::new(),
    };

    let base_container_config = ContainerConfig {
        image: "mock-image".to_string(),
        ..Default::default()
    };
    let mut runner = TestRunner::new(config, "fedora44".to_string());
    let suite = runner
        .run(
            &manifest,
            &backend,
            &"ctr-1".to_string(),
            Some(&base_container_config),
        )
        .await
        .unwrap();

    assert_eq!(suite.failed(), 1);
    assert_eq!(backend.created_containers().len(), 2);
}

mod controls;

#[tokio::test]
async fn push_to_remi_buffers_failed_payload_that_round_trips() {
    let wal = Arc::new(tokio::sync::Mutex::new(Wal::open(":memory:").unwrap()));
    let data = build_push_result(
        "T-BUFFER",
        "buffer_failure",
        TestStatus::Failed,
        23,
        Some("message\nwith é"),
        Some(&ExecResult {
            exit_code: 1,
            stdout: "stdout\n多 byte".to_string(),
            stderr: "stderr\nошибка".to_string(),
        }),
    );
    let ctx = RemiStreamCtx {
        remi_run_id: 41,
        client: Arc::new(RemiClient::new(
            "http://127.0.0.1:1".to_string(),
            "test-token".to_string(),
        )),
        wal: Some(wal.clone()),
    };

    push_to_remi(&ctx, &data).await;

    let wal_guard = wal.lock().await;
    let items = wal_guard.pending_items().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].run_id, 41);
    let stored: PushResultData = serde_json::from_str(&items[0].payload).unwrap();
    assert_eq!(stored, data);
}

#[tokio::test]
async fn remi_failure_does_not_change_exit_outcome_or_json_report() {
    let manifest = make_manifest(vec![TestDef {
        id: "T-ISOLATION".to_string(),
        name: "failure_isolation".to_string(),
        description: "streaming must be observational".to_string(),
        timeout: 30,
        flaky: None,
        retries: None,
        retry_delay_ms: None,
        step: vec![simple_step_run(
            "echo stable",
            Some(make_assertion(Some(0), Some("stable"))),
        )],
        resources: None,
        depends_on: None,
        fatal: None,
        group: None,
        skip: None,
        requires: Vec::new(),
        corpus: None,
    }]);

    let no_remi_backend = MockBackend::new(vec![ExecResult {
        exit_code: 0,
        stdout: "stable".to_string(),
        stderr: String::new(),
    }]);
    let mut no_remi_runner = TestRunner::new(test_config(), "fedora44".to_string());
    let no_remi_suite = no_remi_runner
        .run(
            &manifest,
            &no_remi_backend,
            &"ctr-no-remi".to_string(),
            None,
        )
        .await
        .unwrap();

    let remi_backend = MockBackend::new(vec![ExecResult {
        exit_code: 0,
        stdout: "stable".to_string(),
        stderr: String::new(),
    }]);
    let wal = Arc::new(tokio::sync::Mutex::new(Wal::open(":memory:").unwrap()));
    let ctx = RemiStreamCtx {
        remi_run_id: 42,
        client: Arc::new(RemiClient::new(
            "http://127.0.0.1:1".to_string(),
            "test-token".to_string(),
        )),
        wal: Some(wal.clone()),
    };
    let mut remi_runner = TestRunner::new(test_config(), "fedora44".to_string());
    let mut installed_fixtures = InstalledFixtures::default();
    let remi_suite = remi_runner
        .run_with_cancel(
            &manifest,
            &remi_backend,
            &"ctr-remi".to_string(),
            None,
            None,
            None,
            Some(&ctx),
            &mut installed_fixtures,
        )
        .await
        .unwrap();

    assert_eq!(
        no_remi_suite.has_blocking_results(),
        remi_suite.has_blocking_results()
    );
    assert_eq!(
        comparable_json_report(&no_remi_suite),
        comparable_json_report(&remi_suite)
    );
    assert_eq!(wal.lock().await.pending_count().unwrap(), 1);
}

#[test]
fn every_runner_push_shape_round_trips_through_the_wal() {
    let wal = Wal::open(":memory:").unwrap();
    let statuses = [
        TestStatus::Passed,
        TestStatus::Failed,
        TestStatus::Skipped,
        TestStatus::Cancelled,
    ];

    for (index, status) in statuses.into_iter().enumerate() {
        let cases = [
            build_push_result(
                &format!("T-NONE-{index}"),
                "without execution",
                status,
                0,
                None,
                None,
            ),
            build_push_result(
                &format!("T-EXEC-{index}"),
                "with execution",
                status,
                17,
                Some("message\n多字节"),
                Some(&ExecResult {
                    exit_code: -1,
                    stdout: "stdout\né\n行".to_string(),
                    stderr: "stderr\nошибка".to_string(),
                }),
            ),
        ];

        for (offset, data) in cases.into_iter().enumerate() {
            let run_id = i64::try_from(index * 2 + offset).unwrap();
            wal.buffer(run_id, &serde_json::to_string(&data).unwrap())
                .unwrap();
            let item = wal.pending_items().unwrap().pop().unwrap();
            let decoded: PushResultData = serde_json::from_str(&item.payload).unwrap();
            assert_eq!(decoded, data);
            wal.remove(item.id).unwrap();
        }
    }

    let explicit_none = PushResultData {
        test_id: "T-OPTIONALS".to_string(),
        name: "all optional fields absent".to_string(),
        status: "passed".to_string(),
        duration_ms: None,
        message: None,
        attempt: None,
        steps: Vec::new(),
    };
    wal.buffer(99, &serde_json::to_string(&explicit_none).unwrap())
        .unwrap();
    let item = wal.pending_items().unwrap().pop().unwrap();
    let decoded: PushResultData = serde_json::from_str(&item.payload).unwrap();
    assert_eq!(decoded, explicit_none);
}

fn comparable_json_report(suite: &TestSuite) -> serde_json::Value {
    let mut report = crate::report::json::to_json_value(suite).unwrap();
    if let Some(results) = report["results"].as_array_mut() {
        for result in results {
            result
                .as_object_mut()
                .expect("result is an object")
                .remove("duration_ms");
        }
    }
    report
}
