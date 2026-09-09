// apps/conary/src/commands/test_helpers/native_artifact.rs
//! Signed native RPM fixtures shared by acquisition and projected dependency tests.

use conary_core::repository::OpenPgpTrustRoot;
use sequoia_openpgp as openpgp;
use std::{io::Write, path::Path};

pub(crate) struct PgpAuthority {
    pub(crate) certificate: openpgp::Cert,
    pub(crate) root: OpenPgpTrustRoot,
}

pub(crate) fn pgp_authority(directory: &Path) -> PgpAuthority {
    use openpgp::cert::prelude::CertBuilder;
    use openpgp::serialize::Serialize;

    let (certificate, _) = CertBuilder::new()
        .add_userid("Exact Artifact Test Signer")
        .add_signing_subkey()
        .generate()
        .unwrap();
    let public = certificate.clone().strip_secret_key_material();
    let mut bytes = Vec::new();
    public.serialize(&mut bytes).unwrap();
    let path = directory.join("repository-signing-key.pgp");
    std::fs::write(&path, bytes).unwrap();
    PgpAuthority {
        root: OpenPgpTrustRoot {
            url: format!("file://{}", path.display()),
            fingerprint: certificate.fingerprint().to_hex(),
        },
        certificate,
    }
}

pub(crate) fn detached_signature(certificate: &openpgp::Cert, data: &[u8]) -> Vec<u8> {
    use openpgp::policy::StandardPolicy;
    use openpgp::serialize::stream::{Message, Signer};

    let policy = StandardPolicy::new();
    let keypair = certificate
        .keys()
        .unencrypted_secret()
        .with_policy(&policy, None)
        .supported()
        .alive()
        .revoked(false)
        .for_signing()
        .next()
        .unwrap()
        .key()
        .clone()
        .into_keypair()
        .unwrap();
    let mut signature = Vec::new();
    let message = Message::new(&mut signature);
    let mut signer = Signer::new(message, keypair)
        .unwrap()
        .detached()
        .build()
        .unwrap();
    signer.write_all(data).unwrap();
    signer.finalize().unwrap();
    signature
}

pub(crate) fn signed_rpm_bytes(authority: &PgpAuthority) -> Vec<u8> {
    let mut builder = rpm::PackageBuilder::new(
        "exact",
        "1.2.3",
        "MIT",
        "x86_64",
        "exact acquisition fixture",
    );
    builder.release("4");
    builder
        .with_file_contents(
            b"exact rpm payload".to_vec(),
            rpm::FileOptions::new("/usr/bin/exact").permissions(0o755),
        )
        .unwrap();
    let mut package = builder.build().unwrap();
    let signature = detached_signature(&authority.certificate, &package.header_bytes().unwrap());
    package.apply_signature(signature).unwrap();
    let mut bytes = Vec::new();
    package.write(&mut bytes).unwrap();
    bytes
}
