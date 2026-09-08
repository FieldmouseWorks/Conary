// crates/conary-core/src/ccs/verify/tests.rs

use super::*;
use crate::ccs::builder::write_v3_ccs_package_from_bounded_memory_for_tests;
use crate::ccs::signing::SigningKeyPair;
use std::io::Read;

fn package(signer: &SigningKeyPair) -> (tempfile::TempDir, std::path::PathBuf, TrustPolicy) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("verified.ccs");
    let authority = crate::ccs::v3::test_support::package_authority_with_one_file("verified");
    let payloads = crate::ccs::v3::test_support::one_file_payloads_for_tests();
    write_v3_ccs_package_from_bounded_memory_for_tests(
        &authority, &payloads, &path, signer, None, None, None,
    )
    .unwrap();
    let policy = TrustPolicy::strict(vec![signer.public_key_base64()]);
    (temp, path, policy)
}

fn chunked_package(
    signer: &SigningKeyPair,
) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    TrustPolicy,
    Vec<u8>,
    usize,
) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("chunked.ccs");
    let bytes = (0..(crate::ccs::chunking::MAX_CHUNK_SIZE as usize * 4))
        .map(|index| ((index * 19 + index / 97) % 256) as u8)
        .collect::<Vec<_>>();
    let chunks = crate::ccs::chunking::Chunker::new()
        .chunk_bytes(&bytes)
        .iter()
        .map(crate::ccs::chunking::Chunk::reference)
        .collect::<Vec<_>>();
    let unique = chunks
        .iter()
        .map(|chunk| chunk.sha256.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let mut authority = crate::ccs::v3::test_support::package_authority_with_one_file("chunked");
    let crate::ccs::v3::PackageKindV3::Package(package) = &mut authority.kind else {
        unreachable!()
    };
    let file = &mut package.files[0];
    file.content = Some(crate::payload::PayloadContentAuthority {
        sha256: crate::hash::sha256(&bytes),
        size: bytes.len() as u64,
    });
    file.content_layout = crate::ccs::v3::FileContentLayoutV3::FastCdcV2020 {
        min_size: crate::ccs::chunking::MIN_CHUNK_SIZE,
        average_size: crate::ccs::chunking::AVG_CHUNK_SIZE,
        max_size: crate::ccs::chunking::MAX_CHUNK_SIZE,
        chunks,
    };
    authority.components.get_mut("main").unwrap().total_size = bytes.len() as u64;
    let payloads = std::collections::BTreeMap::from([(file.path.clone(), bytes.clone())]);
    write_v3_ccs_package_from_bounded_memory_for_tests(
        &authority, &payloads, &path, signer, None, None, None,
    )
    .unwrap();
    let policy = TrustPolicy::strict(vec![signer.public_key_base64()]);
    (temp, path, policy, bytes, unique)
}

#[test]
fn verified_value_requires_current_signed_trusted_authority() {
    let signer = SigningKeyPair::generate().with_key_id("release");
    let (_temp, path, policy) = package(&signer);
    let verified = verify_package(&path, &policy).unwrap();

    assert_eq!(verified.package_name(), "verified");
    assert_eq!(verified.files_checked(), 1);
    assert_eq!(verified.signature().key_id.as_deref(), Some("release"));
    assert_eq!(verified.verified_object_metrics(), None);
}

#[test]
fn verified_archive_identity_covers_the_exact_complete_compressed_file() {
    let signer = SigningKeyPair::generate().with_key_id("release");
    let (_temp, path, policy) = package(&signer);
    let independent_bytes = std::fs::read(&path).unwrap();
    let independent_size = std::fs::metadata(&path).unwrap().len();

    let verified = verify_package(&path, &policy).unwrap();

    assert_eq!(
        verified.archive_sha256(),
        Some(crate::hash::sha256(&independent_bytes).as_str())
    );
    assert_eq!(verified.archive_bytes(), Some(independent_size));
    assert_eq!(
        verified.archive_bytes(),
        Some(independent_bytes.len() as u64)
    );
}

#[test]
fn authenticated_decode_reports_exact_parallel_geometry_and_releases_admission() {
    let signer = SigningKeyPair::generate();
    let (_temp, path, policy) = package(&signer);
    let admission = crate::ccs::CcsArchiveCpuAdmission::with_capacity(4).unwrap();

    let verified = verify_package_with_archive_cpu_admission(&path, &policy, &admission).unwrap();
    let metrics = verified.archive_decode_metrics().unwrap();
    assert_eq!(metrics.workers, 4);
    assert_eq!(
        metrics.block_bytes,
        crate::ccs::CCS_BUDGET.archive_compression_block_bytes as u64
    );
    assert_eq!(
        metrics.blocks,
        metrics.decoded_bytes.div_ceil(metrics.block_bytes)
    );
    assert_eq!(
        metrics.buffer_ceiling_bytes,
        crate::ccs::CCS_BUDGET
            .archive_decode_buffer_ceiling_bytes(4)
            .unwrap()
    );
    drop(verified);

    let mut bytes = std::fs::read(&path).unwrap();
    bytes.push(0x1f);
    std::fs::write(&path, bytes).unwrap();
    assert!(verify_package_with_archive_cpu_admission(&path, &policy, &admission).is_err());
    let lease = admission.acquire().unwrap();
    assert_eq!(lease.workers(), 4);
}

#[test]
fn permanent_verification_writes_cold_objects_once_and_warm_objects_zero_times() {
    let signer = SigningKeyPair::generate().with_key_id("release");
    let (temp, path, policy) = package(&signer);
    let cas = crate::filesystem::CasStore::new(temp.path().join("permanent-objects")).unwrap();

    let cold = verify_package_into_cas(&path, &policy, &cas).unwrap();
    let cold_metrics = cold.verified_object_metrics().unwrap();
    assert_eq!(cold_metrics.misses, 1);
    assert_eq!(cold_metrics.hits, 0);
    assert_eq!(
        cold_metrics.persistent_bytes_written,
        cold_metrics.incoming_bytes_hashed
    );
    assert!(cold_metrics.persistent_bytes_written > 0);
    let cold_file = &cold.payload().files()[0];
    let content = cold_file.content_authority.as_ref().unwrap();
    assert_eq!(
        cold_file
            .source()
            .unwrap()
            .verified_cas_identity_for(&cas, content)
            .unwrap()
            .as_deref(),
        Some(content.sha256.as_str())
    );

    let warm = verify_package_into_cas(&path, &policy, &cas).unwrap();
    let warm_metrics = warm.verified_object_metrics().unwrap();
    assert_eq!(warm_metrics.hits, 1);
    assert_eq!(warm_metrics.misses, 0);
    assert_eq!(warm_metrics.persistent_bytes_written, 0);
    assert_eq!(warm_metrics.canonical_bytes_reread, 0);
    assert!(warm_metrics.incoming_bytes_hashed > 0);
}

#[test]
fn permanent_chunk_verification_reuses_objects_and_defers_whole_file_cas_to_installer() {
    let signer = SigningKeyPair::generate().with_key_id("release");
    let (temp, path, policy, bytes, unique) = chunked_package(&signer);
    let cas = crate::filesystem::CasStore::new(temp.path().join("permanent-objects")).unwrap();

    let cold = verify_package_into_cas(&path, &policy, &cas).unwrap();
    let cold_metrics = cold.verified_object_metrics().unwrap();
    assert_eq!(cold_metrics.misses, unique as u64);
    assert_eq!(cold_metrics.hits, 0);
    let file = &cold.payload().files()[0];
    assert_eq!(
        file.source()
            .unwrap()
            .verified_cas_identity_for(&cas, file.content_authority.as_ref().unwrap())
            .unwrap(),
        None,
        "a chunk set is not the whole-file CAS identity generation needs"
    );
    let mut reconstructed = Vec::new();
    file.open_content()
        .unwrap()
        .read_to_end(&mut reconstructed)
        .unwrap();
    assert_eq!(reconstructed, bytes);

    let warm = verify_package_into_cas(&path, &policy, &cas).unwrap();
    let warm_metrics = warm.verified_object_metrics().unwrap();
    assert_eq!(warm_metrics.hits, unique as u64);
    assert_eq!(warm_metrics.misses, 0);
    assert_eq!(warm_metrics.persistent_bytes_written, 0);
    assert_eq!(warm_metrics.canonical_bytes_reread, 0);
}

#[test]
fn permanent_payload_capability_fails_closed_for_another_cas() {
    let signer = SigningKeyPair::generate();
    let (temp, path, policy) = package(&signer);
    let cas = crate::filesystem::CasStore::new(temp.path().join("objects")).unwrap();
    let other = crate::filesystem::CasStore::new(temp.path().join("other-objects")).unwrap();
    let verified = verify_package_into_cas(&path, &policy, &cas).unwrap();
    let file = &verified.payload().files()[0];
    let error = file
        .source()
        .unwrap()
        .verified_cas_identity_for(&other, file.content_authority.as_ref().unwrap())
        .unwrap_err();
    assert!(error.to_string().contains("destination store"));
}

#[test]
fn empty_trust_anchor_set_fails_before_archive_acceptance() {
    let signer = SigningKeyPair::generate();
    let (_temp, path, _) = package(&signer);
    let error = verify_package(&path, &TrustPolicy::strict(Vec::new())).unwrap_err();
    assert_eq!(
        error.downcast_ref::<VerifyError>(),
        Some(&VerifyError::TrustViolation(TrustViolation::NoTrustedKeys))
    );
    assert_eq!(
        error.downcast_ref::<VerificationSubject>().unwrap().path,
        path
    );
}

#[test]
fn policy_rejects_removed_allow_unsigned_field() {
    let error = TrustPolicy::from_toml(
        r#"
trusted_keys = ["AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="]
allow_unsigned = true
"#,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("unknown field `allow_unsigned`"));
}

#[test]
fn untrusted_signer_is_a_typed_failure() {
    let signer = SigningKeyPair::generate();
    let other = SigningKeyPair::generate();
    let (_temp, path, _) = package(&signer);
    let error =
        verify_package(&path, &TrustPolicy::strict(vec![other.public_key_base64()])).unwrap_err();
    assert!(error.downcast_ref::<VerifyError>().is_some());
    assert!(format!("{error:#}").contains("not trusted"));
}

#[test]
fn signer_claim_and_input_path_survive_context_without_cas_objects() {
    for signer in [
        SigningKeyPair::generate(),
        SigningKeyPair::generate().with_key_id("claimed-release"),
    ] {
        let (temp, path, _) = package(&signer);
        let objects = temp.path().join("objects");
        let cas = crate::filesystem::CasStore::new(&objects).unwrap();
        let policy = TrustPolicy::strict(vec![SigningKeyPair::generate().public_key_base64()]);
        let error = verify_package_into_cas(&path, &policy, &cas)
            .unwrap_err()
            .context("install context");
        assert_eq!(
            error.downcast_ref::<VerificationSubject>().unwrap().path,
            path
        );
        assert_eq!(
            error.downcast_ref::<VerifyError>(),
            Some(&VerifyError::TrustViolation(
                TrustViolation::UntrustedSigner {
                    key_id: signer.key_id().map(str::to_owned),
                    public_key: signer.public_key_base64(),
                }
            ))
        );
        assert!(
            !walkdir::WalkDir::new(objects)
                .into_iter()
                .filter_map(Result::ok)
                .any(|entry| entry.file_type().is_file())
        );
    }
}

#[test]
fn timestamp_policy_refusals_preserve_exact_facts() {
    let signer = SigningKeyPair::generate();
    let policy = TrustPolicy::strict(vec![signer.public_key_base64()]).with_max_signature_age(60);
    let mut signature = signer.sign(b"manifest");
    signature.timestamp = None;
    let error = verify_manifest_signature(b"manifest", &signature, &policy).unwrap_err();
    assert_eq!(
        error.downcast_ref::<VerifyError>(),
        Some(&VerifyError::TrustViolation(
            TrustViolation::MissingTimestamp
        ))
    );
    assert!(
        verify_manifest_signature(
            b"manifest",
            &signature,
            &policy.clone().with_timestamp_required(false)
        )
        .is_ok()
    );
    signature.timestamp = Some("bad timestamp".into());
    let error = verify_manifest_signature(b"manifest", &signature, &policy).unwrap_err();
    assert_eq!(
        error.downcast_ref::<VerifyError>(),
        Some(&VerifyError::TrustViolation(
            TrustViolation::InvalidTimestamp {
                timestamp: "bad timestamp".into()
            }
        ))
    );
    let future = (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339();
    signature.timestamp = Some(future.clone());
    let error = verify_manifest_signature(b"manifest", &signature, &policy).unwrap_err();
    assert_eq!(
        error.downcast_ref::<VerifyError>(),
        Some(&VerifyError::TrustViolation(
            TrustViolation::FutureTimestamp { timestamp: future }
        ))
    );
    let past = (chrono::Utc::now() - chrono::Duration::days(1)).to_rfc3339();
    signature.timestamp = Some(past.clone());
    let error = verify_manifest_signature(b"manifest", &signature, &policy).unwrap_err();
    let Some(VerifyError::TrustViolation(TrustViolation::ExpiredSignature {
        timestamp,
        age_seconds,
        max_age_seconds,
    })) = error.downcast_ref::<VerifyError>()
    else {
        panic!("{error:?}");
    };
    assert_eq!(timestamp, &past);
    assert!(*age_seconds >= 86400);
    assert_eq!(*max_age_seconds, 60);
    // The public u64 policy range must not wrap into a negative i64 age limit.
    assert!(
        verify_manifest_signature(
            b"manifest",
            &signature,
            &policy.with_max_signature_age(u64::MAX)
        )
        .is_ok()
    );
}

#[test]
fn signature_and_policy_validation_keep_distinct_typed_causes() {
    let signer = SigningKeyPair::generate();
    let key = signer.public_key_base64();
    let duplicate = TrustPolicy::strict(vec![key.clone(), key.clone()])
        .validate()
        .unwrap_err();
    assert_eq!(
        duplicate.downcast_ref::<VerifyError>(),
        Some(&VerifyError::TrustViolation(
            TrustViolation::DuplicateTrustedKey {
                public_key: key.clone()
            }
        ))
    );
    let policy = TrustPolicy::strict(vec![key]);
    let mut signature = signer.sign(b"manifest");
    let invalid = verify_manifest_signature(b"changed manifest", &signature, &policy).unwrap_err();
    assert!(matches!(
        invalid.downcast_ref::<VerifyError>(),
        Some(VerifyError::SignatureInvalid(_))
    ));
    signature.algorithm = "fixture-algorithm".into();
    let unsupported = verify_manifest_signature(b"manifest", &signature, &policy).unwrap_err();
    assert_eq!(
        unsupported.downcast_ref::<VerifyError>(),
        Some(&VerifyError::UnsupportedAlgorithm {
            algorithm: "fixture-algorithm".into()
        })
    );
}

#[test]
fn authority_validation_retains_every_diagnostic_through_the_reader() {
    let mut authority = crate::ccs::v3::test_support::package_authority_with_one_file("invalid");
    authority.format_version = 999;
    let expected = crate::ccs::v3::validate_authority(&authority).unwrap_err();
    let raw = authority.to_cbor().unwrap();
    let signer = SigningKeyPair::generate();
    let policy = TrustPolicy::strict(vec![signer.public_key_base64()]);
    let error =
        crate::ccs::v3::read_authority_document(&raw, None, None, None, None, &policy).unwrap_err();
    assert_eq!(
        error.downcast_ref::<crate::ccs::v3::V3ValidationError>(),
        Some(&expected)
    );
}

#[test]
fn invalid_policy_file_preserves_its_path_and_typed_cause() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("empty-policy.toml");
    std::fs::write(&path, "trusted_keys = []\n").unwrap();
    let error = TrustPolicy::from_file(&path)
        .unwrap_err()
        .context("load policy");
    assert_eq!(
        error.downcast_ref::<TrustPolicySubject>().unwrap().path,
        path
    );
    assert_eq!(
        error.downcast_ref::<VerifyError>(),
        Some(&VerifyError::TrustViolation(TrustViolation::NoTrustedKeys))
    );
}
