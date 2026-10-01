// apps/conary-test/src/config/tests/native_corpus/typed_sql/fixtures.rs
#![cfg(test)]

use serde_json::{Value, json};

pub(super) const VERSION: &str = "1.0.0-1";
pub(super) const FIDELITY: &str = "native-free";
const RPM_DEP: &str = r#"depends|rpm|{"kind":"Depends","behavior":"Hard","expression":{"operator":"atom","operands":{"name":"config(phase4-runtime-fixture)","capability_kind":null,"version_constraint":"= 1.0.0-1","architecture_qualifier":{"kind":"unqualified"},"native_text":null}},"alternatives":[{"name":"config(phase4-runtime-fixture)","capability_kind":null,"version_constraint":"= 1.0.0-1","architecture_qualifier":{"kind":"unqualified"},"native_text":null}],"description":null,"native_text":"config(phase4-runtime-fixture) = 1.0.0-1"}"#;

pub(super) struct Lane {
    pub(super) distro: &'static str,
    pub(super) target: &'static str,
    pub(super) arch: &'static str,
    pub(super) scheme: &'static str,
    pub(super) profile: &'static str,
    pub(super) dependency_count: &'static str,
    pub(super) dependency_probe: &'static str,
    pub(super) config_source: &'static str,
    pub(super) provider_count: &'static str,
}

pub(super) const LANES: &[Lane] = &[
    Lane {
        distro: "fedora44",
        target: "rpm",
        arch: "x86_64",
        scheme: "rpm",
        profile: "fedora-44",
        dependency_count: "1",
        dependency_probe: RPM_DEP,
        config_source: "rpm",
        provider_count: "7",
    },
    Lane {
        distro: "ubuntu-26.04",
        target: "deb",
        arch: "amd64",
        scheme: "debian",
        profile: "ubuntu-26.04",
        dependency_count: "0",
        dependency_probe: "no-deps",
        config_source: "deb",
        provider_count: "4",
    },
    Lane {
        distro: "arch",
        target: "arch",
        arch: "x86_64",
        scheme: "arch",
        profile: "arch",
        dependency_count: "0",
        dependency_probe: "no-deps",
        config_source: "arch",
        provider_count: "4",
    },
];

pub(super) fn expected_documents(lane: &Lane) -> Vec<Value> {
    let files = json!([
        {"path":"/etc/phase4-runtime-fixture/app.conf","content_sha256":"1da0b50cb027387347265437a11956c1433788d045e4c63b379a1e0740882e7c","content_size":61,"kind":"regular","mode":33188},
        {"path":"/usr/bin/phase4-runtime-fixture","content_sha256":"517631de24336343a6aaf1a8f704d326299c14c619fe8c8d75d17824d074bd7f","content_size":44,"kind":"regular","mode":33188},
        {"path":"/usr/include/phase4-runtime-fixture/api.h","content_sha256":"88904c275ae0f26a566c03f488ae82869c47ac921fb6f0cf7979d5845f199c88","content_size":130,"kind":"regular","mode":33188}
    ]);
    vec![
        json!([{"troves_count":1}]),
        json!([{"name":"phase4-runtime-fixture","version":VERSION,"architecture":lane.arch,"version_scheme":lane.scheme,"source_profile":lane.profile,"install_source":"file","install_reason":"explicit"}]),
        json!([{"regular_files":3}]),
        files,
        json!([{"count_delta":0}]),
        json!([{"file_provides":3}]),
        json!([{"capability":"phase4-runtime-fixture","version":VERSION,"kind":"package"}]),
        json!([{"capability":"/etc/phase4-runtime-fixture/app.conf"},{"capability":"/usr/bin/phase4-runtime-fixture"},{"capability":"/usr/include/phase4-runtime-fixture/api.h"}]),
        json!([{"count_delta":0}]),
        json!([{"dependency_probe":lane.dependency_probe}]),
        json!([{"config_rows":1}]),
        json!([{"path":"/etc/phase4-runtime-fixture/app.conf","original_hash":"1da0b50cb027387347265437a11956c1433788d045e4c63b379a1e0740882e7c","current_hash":"1da0b50cb027387347265437a11956c1433788d045e4c63b379a1e0740882e7c","noreplace":1,"status":"pristine","source":lane.config_source}]),
        json!([{"lifecycle_bundles":1}]),
        json!([{"source_format":lane.target,"source_package":"phase4-runtime-fixture","source_version":VERSION,"scriptlet_fidelity":FIDELITY,"lifecycle_state":"installed"}]),
        json!([{"status":"failed","last_error":"forced generation rebuild failure for test: slice-d-forced","retry_count":1,"recoverable":1}]),
    ]
}
