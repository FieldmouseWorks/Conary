// apps/remi/src/server/native_oracle_input/retention/tests.rs

#![cfg(test)]

use super::*;
use crate::server::catalog_authority::test_support::ActiveCatalogFixture;
use crate::server::native_oracle_input::{
    NativeOracleInputConfig, materialize_native_oracle_inputs,
};
use conary_core::db::models::{RemiRuntimeSession, plan_catalog_collection};

async fn export(
    fixture: &ActiveCatalogFixture,
    directory: &Path,
    export_id: &str,
) -> NativeOracleInputRetention {
    let selections = conary_core::repository::supported_profiles::public_profiles()
        .iter()
        .enumerate()
        .map(|(index, profile)| ProfileRevisionSelection {
            source_profile: profile.id().to_string(),
            profile_revision_sha256: fixture.candidate(profile.id(), index as i64 + 1, Vec::new()),
        })
        .collect();
    materialize_native_oracle_inputs(&NativeOracleInputConfig {
        export_id: export_id.to_string(),
        db_path: fixture.db_path().to_path_buf(),
        catalog_dir: fixture.catalog_dir().to_path_buf(),
        candidates: selections,
        output_dir: directory.to_path_buf(),
    })
    .await
    .unwrap()
    .retention
}

#[tokio::test]
async fn export_survives_candidate_supersession_session_replacement_and_collection() {
    let fixture = ActiveCatalogFixture::new();
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let retained = export(&fixture, &input, "export-one").await;
    let manifest = reopen_native_oracle_input_bundle(&input).unwrap();
    for profile in &retained.profiles {
        let replacement = fixture.candidate(&profile.profile, 20, Vec::new());
        assert_ne!(replacement, profile.profile_revision_sha256);
    }
    let conn = fixture.connection();
    RemiRuntimeSession::begin(&conn, 50).unwrap();
    let plan = plan_catalog_collection(&conn).unwrap();
    for profile in &manifest.profiles {
        assert!(
            plan.reachability
                .contains_profile_revision(&profile.profile_revision_sha256)
        );
        for source in &profile.sources {
            assert!(
                plan.reachability
                    .contains_source_snapshot(&source.manifest_sha256().unwrap())
            );
        }
    }
    assert_eq!(
        inspect_native_oracle_input_retention(
            fixture.db_path(),
            fixture.catalog_dir(),
            &input,
            "export-one",
        )
        .unwrap(),
        retained
    );
    // Currentness remains a separate predicate and correctly refuses the old set.
    assert!(
        super::super::capture_current_candidates(fixture.db_path(), &retained.selections())
            .is_err()
    );
    release_native_oracle_input_retention(
        fixture.db_path(),
        "export-one",
        &retained.input_manifest_sha256,
    )
    .unwrap();
    // The completed inspection's reader-pin drops may be queued on Tokio.
    // A replacement session removes precisely those departed reader owners.
    RemiRuntimeSession::begin(&conn, 60).unwrap();
    let plan = plan_catalog_collection(&conn).unwrap();
    for profile in &retained.profiles {
        assert!(
            !plan
                .reachability
                .contains_profile_revision(&profile.profile_revision_sha256)
        );
    }
    assert!(
        inspect_native_oracle_input_retention(
            fixture.db_path(),
            fixture.catalog_dir(),
            &input,
            "export-one",
        )
        .unwrap_err()
        .to_string()
        .contains("absent or released")
    );
}

#[tokio::test]
async fn retain_conflict_rolls_back_the_entire_new_set() {
    let fixture = ActiveCatalogFixture::new();
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let retained = export(&fixture, &input, "existing").await;
    let manifest = reopen_native_oracle_input_bundle(&input).unwrap();
    let initial =
        super::super::capture_current_candidates(fixture.db_path(), &retained.selections())
            .unwrap();
    let conn = fixture.connection();
    let mut collision = RemiProfileRevisionPin::find(&conn, &pin_id("existing", "ubuntu-26.04"))
        .unwrap()
        .unwrap();
    collision.pin_id = pin_id("blocked", "ubuntu-26.04");
    collision.owner_identity = "other-work".to_string();
    collision.insert(&conn).unwrap();
    assert!(retain_export(fixture.db_path(), "blocked", &manifest, &initial).is_err());
    assert!(
        RemiProfileRevisionPin::find(&conn, &pin_id("blocked", "fedora-44"))
            .unwrap()
            .is_none()
    );
    assert!(
        RemiProfileRevisionPin::find(&conn, &pin_id("blocked", "arch"))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        RemiProfileRevisionPin::find(&conn, &collision.pin_id)
            .unwrap()
            .unwrap(),
        collision
    );
}

#[tokio::test]
async fn supersession_before_retention_cannot_pin_a_historical_export() {
    let fixture = ActiveCatalogFixture::new();
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let retained = export(&fixture, &input, "existing").await;
    let manifest = reopen_native_oracle_input_bundle(&input).unwrap();
    let initial =
        super::super::capture_current_candidates(fixture.db_path(), &retained.selections())
            .unwrap();
    fixture.candidate("arch", 20, Vec::new());
    assert!(retain_export(fixture.db_path(), "late", &manifest, &initial).is_err());
    for profile in &retained.profiles {
        assert!(
            RemiProfileRevisionPin::find(&fixture.connection(), &pin_id("late", &profile.profile))
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
async fn release_checks_all_owners_atomically_and_preserves_other_pins() {
    let fixture = ActiveCatalogFixture::new();
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let retained = export(&fixture, &input, "export-one").await;
    let conn = fixture.connection();
    let id = pin_id("export-one", "ubuntu-26.04");
    let original = RemiProfileRevisionPin::find(&conn, &id).unwrap().unwrap();
    RemiProfileRevisionPin::release(&conn, &id).unwrap();
    let mut replacement = original.clone();
    replacement.owner_identity = "other-work".to_string();
    replacement.insert(&conn).unwrap();
    assert!(
        release_native_oracle_input_retention(
            fixture.db_path(),
            "export-one",
            &retained.input_manifest_sha256
        )
        .is_err()
    );
    assert!(
        RemiProfileRevisionPin::find(&conn, &pin_id("export-one", "fedora-44"))
            .unwrap()
            .is_some()
    );
    assert!(
        inspect_native_oracle_input_retention(
            fixture.db_path(),
            fixture.catalog_dir(),
            &input,
            "export-one"
        )
        .is_err()
    );
    RemiProfileRevisionPin::release(&conn, &id).unwrap();
    original.insert(&conn).unwrap();
    let others = [
        RemiRevisionPinKind::Conversion,
        RemiRevisionPinKind::Work,
        RemiRevisionPinKind::Reader,
    ]
    .into_iter()
    .map(|kind| {
        let mut pin = original.clone();
        pin.pin_id = format!("unrelated-{}", kind.as_str());
        pin.owner_kind = kind;
        pin.owner_identity = format!("other-{}-owner", kind.as_str());
        pin.runtime_session_id = (kind == RemiRevisionPinKind::Reader).then(|| {
            RemiRuntimeSession::current(&conn)
                .unwrap()
                .unwrap()
                .session_id
        });
        pin.insert(&conn).unwrap();
        pin
    })
    .collect::<Vec<_>>();
    assert!(
        release_native_oracle_input_retention(fixture.db_path(), "export-one", &"a".repeat(64))
            .is_err()
    );
    let result = release_native_oracle_input_retention(
        fixture.db_path(),
        "export-one",
        &retained.input_manifest_sha256,
    )
    .unwrap();
    assert_eq!(result.released_profiles, 3);
    for other in others {
        assert_eq!(
            RemiProfileRevisionPin::find(&conn, &other.pin_id)
                .unwrap()
                .unwrap(),
            other
        );
    }
    assert!(
        super::super::capture_current_candidates(fixture.db_path(), &retained.selections()).is_ok()
    );
    assert!(
        release_native_oracle_input_retention(
            fixture.db_path(),
            "export-one",
            &retained.input_manifest_sha256
        )
        .is_err()
    );
}

#[tokio::test]
async fn wrong_export_partial_pins_and_tampered_input_are_refused() {
    let fixture = ActiveCatalogFixture::new();
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let retained = export(&fixture, &input, "export-one").await;
    assert!(
        inspect_native_oracle_input_retention(
            fixture.db_path(),
            fixture.catalog_dir(),
            &input,
            "other-export"
        )
        .is_err()
    );
    let conn = fixture.connection();
    RemiProfileRevisionPin::release(&conn, &pin_id("export-one", "ubuntu-26.04")).unwrap();
    assert!(
        inspect_native_oracle_input_retention(
            fixture.db_path(),
            fixture.catalog_dir(),
            &input,
            "export-one"
        )
        .is_err()
    );
    assert!(
        release_native_oracle_input_retention(
            fixture.db_path(),
            "export-one",
            &retained.input_manifest_sha256
        )
        .is_err()
    );
    assert!(
        RemiProfileRevisionPin::find(&conn, &pin_id("export-one", "fedora-44"))
            .unwrap()
            .is_some()
    );
    let manifest = reopen_native_oracle_input_bundle(&input).unwrap();
    std::fs::write(
        input.join("objects").join(&manifest.objects[0].sha256),
        b"tampered",
    )
    .unwrap();
    assert!(
        inspect_native_oracle_input_retention(
            fixture.db_path(),
            fixture.catalog_dir(),
            &input,
            "export-one"
        )
        .is_err()
    );
}

#[test]
fn export_identity_is_a_bounded_storage_component() {
    for id in [
        "",
        ".",
        "..",
        ".export",
        "Export",
        "aB",
        "../export",
        "a:b",
        "space here",
        "é",
        "a\n",
    ] {
        assert!(validate_export_id(id).is_err(), "{id:?}");
    }
    assert!(validate_export_id(&"a".repeat(129)).is_err());
    validate_export_id("slice6-100-200-1").unwrap();
}

#[tokio::test]
async fn release_uses_the_stored_set_cardinality_instead_of_current_public_profiles() {
    let fixture = ActiveCatalogFixture::new();
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let retained = export(&fixture, &input, "current").await;
    let conn = fixture.connection();
    // A retained set from a different public-profile catalog still has an
    // exhaustive, immutable owner count and exactly derived member pin IDs.
    let manifest_sha256 = "e".repeat(64);
    for profile in &retained.profiles[..2] {
        let mut pin = RemiProfileRevisionPin::find(&conn, &pin_id("current", &profile.profile))
            .unwrap()
            .unwrap();
        pin.pin_id = pin_id("historical", &profile.profile);
        pin.owner_identity = owner("historical", &manifest_sha256, 2).unwrap();
        pin.insert(&conn).unwrap();
    }
    let released =
        release_native_oracle_input_retention(fixture.db_path(), "historical", &manifest_sha256)
            .unwrap();
    assert_eq!(released.released_profiles, 2);
    require_pin_set(&conn, &retained).unwrap();
}

#[tokio::test]
async fn duplicate_export_identity_is_refused_before_any_new_bundle() {
    let fixture = ActiveCatalogFixture::new();
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let retained = export(&fixture, &input, "export-one").await;
    let new_output = temp.path().join("must-not-exist");
    let error = materialize_native_oracle_inputs(&NativeOracleInputConfig {
        export_id: "export-one".to_string(),
        db_path: fixture.db_path().to_path_buf(),
        catalog_dir: fixture.catalog_dir().to_path_buf(),
        candidates: retained.selections(),
        output_dir: new_output.clone(),
    })
    .await
    .unwrap_err();
    assert!(error.to_string().contains("already owns retained catalogs"));
    assert!(!new_output.exists());
    require_pin_set(&fixture.connection(), &retained).unwrap();
}
