use super::*;
use crate::id::ChangeId;
use std::cell::RefCell;

thread_local! {
    /// Every directory this thread synced, in order.
    static SYNCED: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
    /// A directory whose next sync on this thread fails.
    static FAIL_SYNC: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

pub(super) fn synced(dir: &Path) -> io::Result<()> {
    if FAIL_SYNC.with_borrow(|f| f.as_deref() == Some(dir)) {
        FAIL_SYNC.set(None);
        return Err(io::Error::other("injected"));
    }
    SYNCED.with_borrow_mut(|s| s.push(dir.to_owned()));
    Ok(())
}

fn take_synced() -> Vec<PathBuf> {
    SYNCED.take()
}

/// Asserts `dir`'s entry, and every ancestor's, was synced into the
/// directory holding it, and `dir` itself for what's in it.
fn assert_published(synced: &[PathBuf], dir: &Path) {
    for d in dir.ancestors().skip(1).chain([dir]) {
        assert!(synced.iter().any(|s| s == d), "{} not synced", d.display());
    }
}

fn key() -> SigningKey {
    SigningKey::from_bytes(&[7; 32])
}

fn close() -> Body {
    Body::Close {
        change: ChangeId::new().unwrap(),
    }
}

#[test]
fn appended_ops_replay_in_order_and_chain() {
    let dir = tempfile::tempdir().unwrap();
    let (mut log, ops) = OpLog::open(dir.path(), key(), None).unwrap();
    assert!(ops.is_empty());
    let a = log.append(close(), 1).unwrap();
    let b = log.append(close(), 2).unwrap();
    drop(log);
    let ops = read(dir.path(), &key().verifying_key(), None).unwrap();
    assert_eq!(ops, vec![a.clone(), b.clone()]);
    assert_eq!(ops[1].op.parent, Some(a.id));
    let (_, reopened) = OpLog::open(dir.path(), key(), None).unwrap();
    assert_eq!(reopened.len(), 2);
}

fn tamper(dir: &Path, edit: impl Fn(String) -> String) -> LogError {
    let path = dir.join(LOG_FILE);
    fs::write(&path, edit(fs::read_to_string(&path).unwrap())).unwrap();
    read(dir, &key().verifying_key(), None).unwrap_err()
}

fn two_ops() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let (mut log, _) = OpLog::open(dir.path(), key(), None).unwrap();
    log.append(close(), 1).unwrap();
    log.append(close(), 2).unwrap();
    dir
}

#[test]
fn an_edited_op_is_refused() {
    let dir = two_ops();
    let err = tamper(dir.path(), |t| t.replacen("\"at\":1", "\"at\":9", 1));
    assert!(matches!(err, LogError::Corrupt { line: 1, .. }), "{err}");
}

#[test]
fn a_deleted_op_is_refused() {
    let dir = two_ops();
    let err = tamper(dir.path(), |t| t.lines().nth(1).unwrap().to_owned() + "\n");
    assert!(matches!(err, LogError::Corrupt { line: 1, .. }), "{err}");
}

#[test]
fn reordered_ops_are_refused() {
    let dir = two_ops();
    let err = tamper(dir.path(), |t| {
        let lines: Vec<&str> = t.lines().collect();
        format!("{}\n{}\n", lines[1], lines[0])
    });
    assert!(matches!(err, LogError::Corrupt { line: 1, .. }), "{err}");
}

#[test]
fn a_log_signed_by_another_key_is_refused() {
    let dir = two_ops();
    let other = SigningKey::from_bytes(&[8; 32]).verifying_key();
    let err = read(dir.path(), &other, None).unwrap_err();
    assert!(matches!(err, LogError::Corrupt { line: 1, .. }), "{err}");
}

#[test]
fn a_torn_last_line_is_left_out_then_cut_by_the_writer() {
    let dir = two_ops();
    let path = dir.path().join(LOG_FILE);
    let whole = fs::read_to_string(&path).unwrap();
    fs::write(&path, format!("{whole}{{\"id\":\"ab")).unwrap();
    assert_eq!(
        read(dir.path(), &key().verifying_key(), None)
            .unwrap()
            .len(),
        2
    );
    let (mut log, ops) = OpLog::open(dir.path(), key(), None).unwrap();
    assert_eq!(ops.len(), 2);
    log.append(close(), 3).unwrap();
    drop(log);
    assert_eq!(
        read(dir.path(), &key().verifying_key(), None)
            .unwrap()
            .len(),
        3
    );
}

fn intent(summary: &str) -> Body {
    Body::Intent {
        change: ChangeId::new().unwrap(),
        summary: summary.into(),
        body: String::new(),
        task: None,
    }
}

#[test]
fn a_write_torn_inside_a_character_is_left_out_then_cut() {
    let dir = two_ops();
    let path = dir.path().join(LOG_FILE);
    let whole = fs::read(&path).unwrap();
    let (mut log, _) = OpLog::open(dir.path(), key(), None).unwrap();
    log.append(intent("café"), 3).unwrap();
    drop(log);
    let full = fs::read(&path).unwrap();
    let e = full[whole.len()..]
        .windows(2)
        .position(|w| w == "é".as_bytes())
        .unwrap();
    // A line cut halfway through the two bytes of "é".
    let cut = whole.len() + e + 1;
    assert!(std::str::from_utf8(&full[..cut]).is_err());
    fs::write(&path, &full[..cut]).unwrap();
    assert_eq!(
        read(dir.path(), &key().verifying_key(), None)
            .unwrap()
            .len(),
        2
    );
    let (mut log, ops) = OpLog::open(dir.path(), key(), None).unwrap();
    assert_eq!(ops.len(), 2);
    log.append(close(), 4).unwrap();
    drop(log);
    assert_eq!(
        read(dir.path(), &key().verifying_key(), None)
            .unwrap()
            .len(),
        3
    );

    // A torn first op, too.
    fs::write(&path, &whole[..10]).unwrap();
    assert!(
        read(dir.path(), &key().verifying_key(), None)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_complete_line_that_isnt_utf8_is_refused() {
    let dir = two_ops();
    let path = dir.path().join(LOG_FILE);
    let mut bytes = fs::read(&path).unwrap();
    bytes[3] = 0xff;
    fs::write(&path, bytes).unwrap();
    let err = read(dir.path(), &key().verifying_key(), None).unwrap_err();
    assert!(matches!(err, LogError::Corrupt { line: 1, .. }), "{err}");
}

#[test]
fn a_last_op_missing_only_its_newline_is_kept() {
    let dir = two_ops();
    let path = dir.path().join(LOG_FILE);
    let whole = fs::read_to_string(&path).unwrap();
    fs::write(&path, whole.strip_suffix('\n').unwrap()).unwrap();
    assert_eq!(
        read(dir.path(), &key().verifying_key(), None)
            .unwrap()
            .len(),
        2
    );
    let (mut log, ops) = OpLog::open(dir.path(), key(), None).unwrap();
    assert_eq!(ops.len(), 2);
    log.append(close(), 3).unwrap();
    drop(log);
    assert_eq!(
        read(dir.path(), &key().verifying_key(), None)
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn a_failed_append_stops_the_writer_until_the_log_is_reopened() {
    let dir = two_ops();
    let (mut log, _) = OpLog::open(dir.path(), key(), None).unwrap();
    log.fail_next_write = true;
    assert!(matches!(log.append(close(), 3), Err(LogError::Io(_))));
    assert!(matches!(log.append(close(), 4), Err(LogError::Poisoned)));
    drop(log);
    // Reopening cuts the half line, and the next op chains onto the last
    // one that was written whole.
    let (mut log, ops) = OpLog::open(dir.path(), key(), None).unwrap();
    assert_eq!(ops.len(), 2);
    log.append(close(), 5).unwrap();
    drop(log);
    let ops = read(dir.path(), &key().verifying_key(), None).unwrap();
    assert_eq!(ops.len(), 3);
    assert_eq!(ops[2].op.at, 5);
}

#[test]
fn racing_first_users_all_get_the_one_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deep/er/key");
    let keys: Vec<[u8; 32]> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                s.spawn(|| {
                    let key = load_or_create_key(&path).unwrap().to_bytes();
                    // Every caller, winner or not, saw the key's entry synced.
                    assert_published(&take_synced(), path.parent().unwrap());
                    key
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert!(keys.iter().all(|k| *k == keys[0]));
    assert_eq!(load_or_create_key(&path).unwrap().to_bytes(), keys[0]);
    // No half-made key is left lying around.
    let names: Vec<_> = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    let mut names: Vec<_> = names.iter().map(|n| n.to_str().unwrap()).collect();
    names.sort();
    assert_eq!(names, ["key", "key.pub"]);
}

#[test]
fn a_key_is_made_once_then_loaded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("key");
    let made = load_or_create_key(&path).unwrap();
    assert_eq!(
        load_or_create_key(&path).unwrap().to_bytes(),
        made.to_bytes()
    );
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let public = load_public_key(&public_path(&path)).unwrap();
    assert_eq!(public, made.verifying_key());
    fs::write(public_path(&path), "ab\n").unwrap();
    assert!(load_public_key(&public_path(&path)).is_err());
    // Loading the key again puts its public half back.
    load_or_create_key(&path).unwrap();
    assert_eq!(load_public_key(&public_path(&path)).unwrap(), public);
}

/// Asserts the log at `dir` is refused, by readers and the writer
/// alike, and that refusing it changed nothing in it.
fn assert_refused(dir: &Path, anchor: Option<Anchor>) -> LogError {
    let path = dir.join(LOG_FILE);
    let before = fs::read(&path).ok();
    let read_err = read(dir, &key().verifying_key(), anchor).unwrap_err();
    let Err(open_err) = OpLog::open(dir, key(), anchor) else {
        panic!("the writer opened a log the reader refused: {read_err}");
    };
    assert_eq!(read_err.to_string(), open_err.to_string());
    if let Some(before) = before {
        assert_eq!(fs::read(&path).unwrap(), before, "refusing changed the log");
    }
    read_err
}

fn unterminate(dir: &Path) {
    let path = dir.join(LOG_FILE);
    let whole = fs::read_to_string(&path).unwrap();
    fs::write(&path, whole.strip_suffix('\n').unwrap()).unwrap();
}

#[test]
fn an_unterminated_first_op_under_another_key_is_refused_not_cut() {
    let dir = tempfile::tempdir().unwrap();
    let (mut log, _) = OpLog::open(dir.path(), SigningKey::from_bytes(&[8; 32]), None).unwrap();
    log.append(close(), 1).unwrap();
    drop(log);
    unterminate(dir.path());
    let err = assert_refused(dir.path(), None);
    assert!(matches!(err, LogError::Corrupt { line: 1, .. }), "{err}");
}

#[test]
fn an_unterminated_last_op_that_doesnt_verify_is_refused_not_cut() {
    type Edit = fn(&str, &str) -> String;
    let edits: [(&str, Edit); 4] = [
        ("id", |first, last| {
            format!("{first}\n{}", last.replacen("\"at\":2", "\"at\":9", 1))
        }),
        ("signature", |first, last| {
            let at = last.find("\"sig\":\"").unwrap() + 7;
            let flip = if &last[at..at + 1] == "0" { "1" } else { "0" };
            format!("{first}\n{}{flip}{}", &last[..at], &last[at + 1..])
        }),
        ("parent", |first, _| format!("{first}\n{first}")),
        ("not JSON at all", |first, _| format!("{first}\nnot a line")),
    ];
    for (what, edit) in edits {
        let dir = two_ops();
        let path = dir.path().join(LOG_FILE);
        let text = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        fs::write(&path, edit(lines[0], lines[1])).unwrap();
        let err = assert_refused(dir.path(), None);
        assert!(
            matches!(err, LogError::Corrupt { line: 2, .. }),
            "{what}: {err}"
        );
    }
}

#[test]
fn an_unfinished_tail_with_bytes_that_cant_be_utf8_is_refused() {
    let dir = two_ops();
    let path = dir.path().join(LOG_FILE);
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(b"{\"id\":\"\xff\xfeab");
    fs::write(&path, bytes).unwrap();
    let err = assert_refused(dir.path(), None);
    assert!(matches!(err, LogError::Corrupt { line: 3, .. }), "{err}");
}

#[test]
fn a_signed_op_of_an_unknown_schema_is_refused() {
    let json = r#"{"parent":null,"at":1,"body":{"kind":"unheard_of"}}"#;
    let id = OpId::of(json.as_bytes());
    let sig = hex(&key().sign(id.as_bytes()).to_bytes());
    let line = format!("{{\"id\":\"{id}\",\"sig\":\"{sig}\",\"op\":{json}}}");
    for ending in ["\n", ""] {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(LOG_FILE), format!("{line}{ending}")).unwrap();
        let err = assert_refused(dir.path(), None);
        assert!(matches!(err, LogError::Corrupt { line: 1, .. }), "{err}");
    }
}

fn anchored_two_ops() -> (tempfile::TempDir, Anchor) {
    let dir = tempfile::tempdir().unwrap();
    let (mut log, _) = OpLog::open(dir.path(), key(), None).unwrap();
    assert_eq!(log.head(), None);
    let a = log.append(close(), 1).unwrap();
    assert_eq!(log.head(), Some(Anchor { seq: 1, op: a.id }));
    let b = log.append(close(), 2).unwrap();
    let head = log.head().unwrap();
    assert_eq!(head, Anchor { seq: 2, op: b.id });
    drop(log);
    // Reopening finds the same head.
    let (log, _) = OpLog::open(dir.path(), key(), Some(head)).unwrap();
    assert_eq!(log.head(), Some(head));
    (dir, head)
}

#[test]
fn a_log_cut_short_by_whole_lines_is_refused_against_its_anchor() {
    let (dir, anchor) = anchored_two_ops();
    let path = dir.path().join(LOG_FILE);
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.lines().next().unwrap().to_owned() + "\n").unwrap();
    let err = assert_refused(dir.path(), Some(anchor));
    assert!(
        matches!(err, LogError::Unanchored(a) if a == anchor),
        "{err}"
    );
    // Without the anchor it still chains: why the anchor lives off the box.
    assert_eq!(
        read(dir.path(), &key().verifying_key(), None)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn an_anchored_op_cut_partway_is_refused_not_trimmed() {
    let (dir, anchor) = anchored_two_ops();
    let path = dir.path().join(LOG_FILE);
    let bytes = fs::read(&path).unwrap();
    // It now looks like a torn write, but it was acknowledged.
    fs::write(&path, &bytes[..bytes.len() - 10]).unwrap();
    let err = assert_refused(dir.path(), Some(anchor));
    assert!(matches!(err, LogError::Unanchored(_)), "{err}");
}

#[test]
fn a_missing_log_is_refused_against_its_anchor() {
    let (dir, anchor) = anchored_two_ops();
    fs::remove_file(dir.path().join(LOG_FILE)).unwrap();
    let err = assert_refused(dir.path(), Some(anchor));
    assert!(matches!(err, LogError::Unanchored(_)), "{err}");
    let err = read(
        &dir.path().join("elsewhere"),
        &key().verifying_key(),
        Some(anchor),
    );
    assert!(matches!(err, Err(LogError::Unanchored(_))));
}

#[test]
fn a_replaced_log_is_refused_against_its_anchor() {
    let (dir, anchor) = anchored_two_ops();
    // Another history, as long and signed by the same key.
    let other = two_ops();
    fs::copy(other.path().join(LOG_FILE), dir.path().join(LOG_FILE)).unwrap();
    let err = assert_refused(dir.path(), Some(anchor));
    assert!(matches!(err, LogError::Unanchored(_)), "{err}");
}

#[test]
fn a_log_grown_past_its_anchor_opens_and_a_torn_tail_is_still_cut() {
    let (dir, anchor) = anchored_two_ops();
    let first = read(dir.path(), &key().verifying_key(), None).unwrap()[0].id;
    let earlier = Anchor { seq: 1, op: first };
    let path = dir.path().join(LOG_FILE);
    let whole = fs::read_to_string(&path).unwrap();
    fs::write(&path, format!("{whole}{{\"id\":\"ab")).unwrap();
    for a in [earlier, anchor] {
        assert_eq!(
            read(dir.path(), &key().verifying_key(), Some(a))
                .unwrap()
                .len(),
            2
        );
    }
    let (mut log, ops) = OpLog::open(dir.path(), key(), Some(anchor)).unwrap();
    assert_eq!(ops.len(), 2);
    log.append(close(), 3).unwrap();
    let head = log.head().unwrap();
    drop(log);
    assert_eq!(head.seq, 3);
    assert_eq!(
        read(dir.path(), &key().verifying_key(), Some(head))
            .unwrap()
            .len(),
        3
    );
    // An anchor past the end, or at op 0, is never held.
    for a in [Anchor { seq: 4, ..head }, Anchor { seq: 0, ..head }] {
        let err = read(dir.path(), &key().verifying_key(), Some(a));
        assert!(matches!(err, Err(LogError::Unanchored(_))));
    }
}

#[test]
fn a_directory_someone_else_made_is_still_synced_into_its_parent() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a/b");
    // Made by another initializer that hasn't synced anything yet.
    fs::create_dir_all(&dir).unwrap();
    take_synced();
    OpLog::open(&dir, key(), None).unwrap();
    assert_published(&take_synced(), &dir);
}

#[test]
fn a_retry_after_a_failed_sync_completes_it() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a/b");
    FAIL_SYNC.set(Some(root.path().join("a")));
    assert!(matches!(
        OpLog::open(&dir, key(), None),
        Err(LogError::Io(_))
    ));
    // The directory was made, but its entry in `a` was never synced.
    assert!(dir.is_dir());
    take_synced();
    OpLog::open(&dir, key(), None).unwrap();
    assert_published(&take_synced(), &dir);

    let path = root.path().join("keys/key");
    FAIL_SYNC.set(Some(root.path().join("keys")));
    assert!(matches!(load_or_create_key(&path), Err(LogError::Io(_))));
    // The key was linked into place, but its entry was never synced.
    assert!(path.exists());
    take_synced();
    load_or_create_key(&path).unwrap();
    assert_published(&take_synced(), path.parent().unwrap());
}

#[test]
fn a_key_someone_else_made_is_synced_before_it_is_loaded() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("deep/er/key");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, [9u8; 32]).unwrap();
    take_synced();
    assert_eq!(load_or_create_key(&path).unwrap().to_bytes(), [9u8; 32]);
    assert_published(&take_synced(), path.parent().unwrap());
}

#[test]
fn a_key_and_a_log_in_different_directories_are_both_published() {
    let root = tempfile::tempdir().unwrap();
    let key_path = root.path().join("etc/conary/key");
    let log_dir = root.path().join("var/lib/conary");
    take_synced();
    let key = load_or_create_key(&key_path).unwrap();
    let (mut log, _) = OpLog::open(&log_dir, key, None).unwrap();
    log.append(close(), 1).unwrap();
    let synced = take_synced();
    assert_published(&synced, key_path.parent().unwrap());
    assert_published(&synced, &log_dir);
}

#[test]
fn an_ancestor_that_cant_be_opened_to_sync_is_an_error_every_time() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let blind = root.path().join("blind");
    fs::create_dir(&blind).unwrap();
    // Writable and searchable, but not readable: entries can be made in
    // it, and it can't be opened to sync them.
    fs::set_permissions(&blind, fs::Permissions::from_mode(0o300)).unwrap();
    if File::open(&blind).is_ok() {
        return; // Run as root, which permissions don't stop.
    }
    let dir = blind.join("ledger");
    let key_path = blind.join("keys/key");
    for _ in 0..2 {
        // The retry finds the directory made, and still can't sync it.
        let err = OpLog::open(&dir, key(), None).err().unwrap();
        assert!(matches!(&err, LogError::Io(e) if e.kind() == io::ErrorKind::PermissionDenied));
        let err = load_or_create_key(&key_path).err().unwrap();
        assert!(matches!(&err, LogError::Io(e) if e.kind() == io::ErrorKind::PermissionDenied));
    }
    assert!(dir.is_dir());
    fs::set_permissions(&blind, fs::Permissions::from_mode(0o700)).unwrap();
    OpLog::open(&dir, key(), None).unwrap();
    load_or_create_key(&key_path).unwrap();
}
