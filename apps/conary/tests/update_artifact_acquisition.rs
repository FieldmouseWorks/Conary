// apps/conary/tests/update_artifact_acquisition.rs
#![cfg(feature = "test-hooks")]

//! Issue #965 regression: `update` applies the signed full artifact admitted by
//! preview and requests no advertised delta afterwards. A loopback counter
//! server accounts full-artifact traffic separately from the advertised valid
//! and invalid delta rows.

pub mod common;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use conary_core::db::models::{InstallSource, PackageDelta, Trove, TroveType};
use conary_core::repository::versioning::VersionScheme;
use std::net::SocketAddr;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Traffic {
    requests: AtomicUsize,
    bytes: AtomicUsize,
}

#[derive(Default)]
struct Counters {
    full: Traffic,
    valid_delta: Traffic,
    invalid_delta: Traffic,
}

struct ServerState {
    counters: Arc<Counters>,
    full: Bytes,
    valid_delta: Bytes,
    invalid_delta: Bytes,
}

async fn serve(State(state): State<Arc<ServerState>>, uri: Uri) -> Response {
    let (body, traffic) = match uri.path() {
        "/demo-x86_64.ccs" => (&state.full, &state.counters.full),
        "/valid.delta" => (&state.valid_delta, &state.counters.valid_delta),
        "/invalid.delta" => (&state.invalid_delta, &state.counters.invalid_delta),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    traffic.requests.fetch_add(1, Ordering::SeqCst);
    traffic.bytes.fetch_add(body.len(), Ordering::SeqCst);
    (StatusCode::OK, body.clone()).into_response()
}

struct CounterServer {
    addr: SocketAddr,
    counters: Arc<Counters>,
    _runtime: tokio::runtime::Runtime,
}

impl CounterServer {
    fn start(full: Bytes, valid_delta: Bytes, invalid_delta: Bytes) -> Self {
        let counters = Arc::new(Counters::default());
        let state = Arc::new(ServerState {
            counters: Arc::clone(&counters),
            full,
            valid_delta,
            invalid_delta,
        });
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        runtime.spawn(async move {
            let app: Router = Router::new().fallback(serve).with_state(state);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            ready_tx.send(listener.local_addr().unwrap()).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            addr: ready_rx.recv().unwrap(),
            counters,
            _runtime: runtime,
        }
    }
}

fn run_update(warm_cas: bool, valid: bool) {
    let (temp, db, conn) = common::create_test_db();
    let root = temp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let (repo_id, key) = common::update_ccs::repository(&conn);
    let mut installed = Trove::new_with_source(
        "demo".into(),
        "1.0-1".into(),
        TroveType::Package,
        InstallSource::Repository,
        VersionScheme::Rpm,
    );
    installed.package_release = Some("2".into());
    installed.architecture = Some("x86_64".into());
    installed.source_profile = Some("fedora-44".into());
    installed.installed_from_repository_id = Some(repo_id);
    installed.insert(&conn).unwrap();
    common::update_ccs::candidate(&conn, temp.path(), repo_id, &key, "demo", "x86_64");
    let artifact = std::fs::read(temp.path().join("demo-x86_64.ccs")).unwrap();
    let artifact_hash = conary_core::hash::sha256(&artifact);

    // Build the advertised delta in its own CAS so the runtime CAS stays cold
    // unless this run explicitly pre-stores the signed full artifact.
    let delta_objects = temp.path().join("delta-objects");
    let delta_cas = conary_core::filesystem::CasStore::new(&delta_objects).unwrap();
    let old_hash = delta_cas.store(b"demo 1.0-1 payload").unwrap();
    assert_eq!(delta_cas.store(&artifact).unwrap(), artifact_hash);
    let delta_path = temp.path().join("demo.delta");
    conary_core::delta::DeltaGenerator::new(&delta_objects)
        .unwrap()
        .generate_delta(&old_hash, &artifact_hash, &delta_path)
        .unwrap();
    let valid_delta = std::fs::read(&delta_path).unwrap();
    let invalid_delta = b"advertised delta whose checksum cannot match".to_vec();

    let objects = conary_core::db::paths::objects_dir(&db);
    let cas = conary_core::filesystem::CasStore::new(&objects).unwrap();
    assert_eq!(cas.store(b"demo 1.0-1 payload").unwrap(), old_hash);
    if warm_cas {
        assert_eq!(cas.store(&artifact).unwrap(), artifact_hash);
    }
    for hash in [&old_hash, &artifact_hash] {
        conn.execute(
            "INSERT INTO file_contents (sha256_hash, content_path, size) VALUES (?1, ?2, 0)",
            rusqlite::params![hash, format!("objects/{hash}")],
        )
        .unwrap();
    }

    let server = CounterServer::start(
        Bytes::from(artifact.clone()),
        Bytes::from(valid_delta.clone()),
        Bytes::from(invalid_delta.clone()),
    );
    let base = format!("http://{}", server.addr);
    conn.execute(
        "UPDATE repository_packages SET download_url = ?1 \
         WHERE repository_id = ?2 AND name = 'demo' AND version = '1.0-2'",
        rusqlite::params![format!("{base}/demo-x86_64.ccs"), repo_id],
    )
    .unwrap();
    let valid_checksum = conary_core::hash::sha256(&valid_delta);
    let invalid_checksum = conary_core::hash::sha256(b"not the advertised bytes");
    let (path, bytes, checksum) = if valid {
        ("/valid.delta", &valid_delta, valid_checksum)
    } else {
        ("/invalid.delta", &invalid_delta, invalid_checksum)
    };
    PackageDelta::new(
        "demo".into(),
        "1.0-1".into(),
        "1.0-2".into(),
        old_hash.clone(),
        artifact_hash.clone(),
        format!("{base}{path}"),
        bytes.len() as i64,
        checksum,
        artifact.len() as i64,
    )
    .insert(&conn)
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_conary"))
        .args([
            "update",
            "demo",
            "--release",
            "2",
            "--yes",
            "--db-path",
            &db,
            "--root",
        ])
        .arg(&root)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("NO_COLOR", "1")
        .env("CONARY_TEST_SKIP_GENERATION_MOUNT", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "warm={warm_cas}: {output:?}");

    let rows = Trove::find_by_name(&conn, "demo").unwrap();
    assert_eq!(
        rows.iter().filter(|row| row.version == "1.0-2").count(),
        1,
        "warm={warm_cas}: {rows:?}"
    );
    let full = &server.counters.full;
    assert_eq!(
        full.requests.load(Ordering::SeqCst),
        1,
        "warm={warm_cas}, valid={valid}"
    );
    assert_eq!(
        full.bytes.load(Ordering::SeqCst),
        artifact.len(),
        "warm={warm_cas}, valid={valid}"
    );

    for (label, traffic) in [
        ("valid delta", &server.counters.valid_delta),
        ("invalid delta", &server.counters.invalid_delta),
    ] {
        let requests = traffic.requests.load(Ordering::SeqCst);
        let bytes = traffic.bytes.load(Ordering::SeqCst);
        assert_eq!(
            (requests, bytes),
            (0, 0),
            "warm={warm_cas}: apply requested advertised {label} bytes"
        );
    }
    println!(
        "ACQUISITION warm_cas={warm_cas} valid_delta={valid} full_requests={} full_bytes={} delta_requests={} delta_bytes={}",
        full.requests.load(Ordering::SeqCst),
        full.bytes.load(Ordering::SeqCst),
        server.counters.valid_delta.requests.load(Ordering::SeqCst)
            + server
                .counters
                .invalid_delta
                .requests
                .load(Ordering::SeqCst),
        server.counters.valid_delta.bytes.load(Ordering::SeqCst)
            + server.counters.invalid_delta.bytes.load(Ordering::SeqCst),
    );
}

#[test]
fn update_apply_reuses_preview_artifact_without_delta_requests() {
    for warm_cas in [false, true] {
        for valid in [false, true] {
            run_update(warm_cas, valid);
        }
    }
}
