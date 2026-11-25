//! The mark beside the log, `log/verified.rs`.

use std::cell::Cell;

use super::key;
use crate::id::ChangeId;
use crate::log::verified::Mark;
use crate::log::*;

thread_local! {
    /// How many ops' signatures this thread checked.
    static CHECKED: Cell<usize> = const { Cell::new(0) };
}

pub(in crate::log) fn checked() {
    CHECKED.set(CHECKED.get() + 1);
}

fn open(dir: &Path, key: SigningKey) -> OpLog {
    OpLog::open(Lock::take(dir).unwrap(), key, None).unwrap().0
}

fn append(log: &mut OpLog, count: usize) {
    for at in 0..count {
        let change = ChangeId::new().unwrap();
        let (mut writer, _) = log.writer().unwrap();
        writer.append(Body::Close { change }, at as i64).unwrap();
    }
}

#[test]
fn n_syncs_in_one_process_verify_the_log_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = open(dir.path(), key());
    append(&mut log, 20);
    let mut anchor = log.head();
    drop(log);

    CHECKED.set(0);
    // Each sync opens the log as a writer, records and moves the anchor.
    for _ in 0..5 {
        let lock = Lock::take(dir.path()).unwrap();
        let (mut log, _) = OpLog::open(lock, key(), anchor).unwrap();
        append(&mut log, 1);
        anchor = log.head();
        save_anchor(log.writer().unwrap().0.lock(), anchor, "c1").unwrap();
    }
    // The 20 once, and once each op an earlier sync appended.
    assert_eq!(CHECKED.get(), 20 + 4);

    // A reader checks only what came after the last writer's open.
    CHECKED.set(0);
    let ops = read_anchored(dir.path(), &key().verifying_key()).unwrap();
    assert_eq!((ops.len(), CHECKED.get()), (25, 1));
}

type Seen = Result<Vec<OpId>, String>;

fn seen(read: Result<Vec<Op>, LogError>) -> Seen {
    read.map(|ops| ops.iter().map(|op| op.id).collect())
        .map_err(|e| e.to_string())
}

/// A log of three ops, all marked, and its head.
fn marked() -> (tempfile::TempDir, Anchor) {
    let dir = tempfile::tempdir().unwrap();
    let mut log = open(dir.path(), key());
    append(&mut log, 3);
    drop(log);
    let head = open(dir.path(), key()).head().unwrap();
    CHECKED.set(0);
    read_marked(dir.path(), &key().verifying_key(), None).unwrap();
    assert_eq!(CHECKED.get(), 0, "the log isn't marked");
    (dir, head)
}

#[test]
fn a_mark_vouches_for_no_prefix_with_a_byte_changed() {
    let (dir, _) = marked();
    let whole = fs::read(log_path(dir.path())).unwrap();
    let mark = Mark::load(dir.path()).unwrap();
    let public = key().verifying_key();
    assert_eq!(mark.covers(&whole, &public), whole.len());
    for at in 0..whole.len() {
        let mut changed = whole.clone();
        changed[at] ^= 1;
        assert_eq!(mark.covers(&changed, &public), 0, "byte {at}");
    }
}

#[test]
fn a_marked_read_says_what_a_full_one_does_of_a_changed_log() {
    let (dir, _) = marked();
    let mut log = open(dir.path(), key());
    append(&mut log, 1); // One op after the mark.
    drop(log);
    let path = log_path(dir.path());
    let whole = fs::read(&path).unwrap();
    let public = key().verifying_key();
    // Every 37th byte: each case costs two reads, and the test above takes
    // every byte of the mark's own check.
    for at in (0..whole.len()).step_by(37) {
        let mut flipped = whole.clone();
        flipped[at] ^= 1;
        // Hex reads the same in either case, so this may still verify.
        let mut recased = whole.clone();
        recased[at] ^= 0x20;
        let dropped = [&whole[..at], &whole[at + 1..]].concat();
        let cut = whole[..at].to_vec();
        for bytes in [flipped, recased, dropped, cut] {
            fs::write(&path, bytes).unwrap();
            assert_eq!(
                seen(read_marked(dir.path(), &public, None)),
                seen(read(dir.path(), &public, None)),
                "byte {at}"
            );
        }
    }
}

#[test]
fn a_prefix_rewritten_behind_the_mark_is_refused() {
    let (dir, head) = marked();
    let path = log_path(dir.path());
    let honest = fs::read(&path).unwrap();
    let mark = fs::read(dir.path().join("verified")).unwrap();

    // Another key's log of the same length, chained and marked by that key.
    let thief = SigningKey::from_bytes(&[9; 32]);
    let theirs = tempfile::tempdir().unwrap();
    let mut log = open(theirs.path(), thief.clone());
    append(&mut log, 3);
    let their_head = log.head();
    drop(log);
    drop(open(theirs.path(), thief.clone()));
    let forged = fs::read(log_path(theirs.path())).unwrap();
    let forged_mark = fs::read(theirs.path().join("verified")).unwrap();
    assert_eq!(forged.len(), honest.len(), "a length alone would pass it");

    // The anchored ops themselves, their ids unmoved, signed by another key.
    let mut resigned = String::new();
    for op in read(dir.path(), &key().verifying_key(), None).unwrap() {
        let sig = thief.sign(op.id.as_bytes());
        let json = serde_json::to_string(&op.op).unwrap();
        resigned += &format!(
            "{{\"id\":\"{}\",\"sig\":\"{}\",\"op\":{json}}}\n",
            op.id,
            hex(&sig.to_bytes())
        );
    }
    assert_eq!(resigned.len(), honest.len());

    for (log, anchor) in [(forged, their_head), (resigned.into_bytes(), Some(head))] {
        for mark in [&mark, &forged_mark] {
            fs::write(&path, &log).unwrap();
            fs::write(dir.path().join("verified"), mark).unwrap();
            let read = read_marked(dir.path(), &key().verifying_key(), anchor);
            let Err(LogError::Corrupt { line: 1, .. }) = read else {
                panic!("a reader took a rewritten prefix: {read:?}");
            };
            let lock = Lock::take(dir.path()).unwrap();
            let Err(LogError::Corrupt { line: 1, .. }) = OpLog::open(lock, key(), anchor) else {
                panic!("a writer took a rewritten prefix");
            };
            assert_eq!(fs::read(&path).unwrap(), log, "refusing changed the log");
        }
    }
}

#[test]
fn a_marked_log_is_still_held_to_its_anchor() {
    let (dir, head) = marked();
    let ahead = Some(Anchor {
        seq: head.seq + 1,
        op: head.op,
    });
    let read = read_marked(dir.path(), &key().verifying_key(), ahead);
    assert!(matches!(read, Err(LogError::Unanchored(_))), "{read:?}");
    let open = OpLog::open(Lock::take(dir.path()).unwrap(), key(), ahead);
    assert!(matches!(open, Err(LogError::Unanchored(_))));
}

#[test]
fn a_full_read_checks_every_signature_whatever_is_marked() {
    let (dir, head) = marked();
    CHECKED.set(0);
    read(dir.path(), &key().verifying_key(), Some(head)).unwrap();
    assert_eq!(CHECKED.get(), 3);
}

#[test]
fn a_mark_that_doesnt_hold_is_ignored_and_the_writer_marks_again() {
    let (dir, head) = marked();
    let path = dir.path().join("verified");
    let good = fs::read_to_string(&path).unwrap();
    let (len, sig) = good.split_once(' ').unwrap();
    let short = len.parse::<usize>().unwrap() - 1;
    for bad in [
        String::new(),
        "not a mark".to_owned(),
        format!("{} {sig}", usize::MAX),
        format!("{short} {sig}"),
        format!("{len} {}", "0".repeat(128)),
    ] {
        fs::write(&path, &bad).unwrap();
        CHECKED.set(0);
        read_marked(dir.path(), &key().verifying_key(), Some(head)).unwrap();
        assert_eq!(CHECKED.get(), 3, "{bad:?}");
        drop(open(dir.path(), key()));
        assert_eq!(fs::read_to_string(&path).unwrap(), good, "{bad:?}");
    }
}

#[test]
fn a_tail_torn_after_the_mark_is_cut_and_checks_nothing_again() {
    let (dir, head) = marked();
    let path = log_path(dir.path());
    let whole = fs::read(&path).unwrap();
    let torn = [&whole[..], b"{\"id\":\"1234"].concat();
    fs::write(&path, torn).unwrap();
    CHECKED.set(0);
    let lock = Lock::take(dir.path()).unwrap();
    let (log, _) = OpLog::open(lock, key(), Some(head)).unwrap();
    assert_eq!((log.head(), CHECKED.get()), (Some(head), 0));
    assert_eq!(fs::read(&path).unwrap(), whole);
}

#[test]
fn a_writer_catching_up_checks_each_appended_signature_and_leaves_the_mark() {
    let (dir, _) = marked();
    let (mut behind, mut other) = (open(dir.path(), key()), open(dir.path(), key()));
    let mark = fs::read(dir.path().join("verified")).unwrap();
    append(&mut other, 2);
    CHECKED.set(0);
    assert_eq!(behind.writer().unwrap().1.len(), 2);
    // Those two and none before: it read only their bytes, so it can't mark.
    assert_eq!(CHECKED.get(), 2);
    assert_eq!(fs::read(dir.path().join("verified")).unwrap(), mark);

    // A mark another writer kept past this one's head doesn't vouch for
    // bytes read from that head on: it's over the log from its start.
    append(&mut other, 1);
    drop(open(dir.path(), key()));
    CHECKED.set(0);
    assert_eq!(behind.writer().unwrap().1.len(), 1);
    assert_eq!(CHECKED.get(), 1);

    // An appended line whose id and parent hold but not its signature.
    let path = log_path(dir.path());
    let whole = fs::read_to_string(&path).unwrap();
    append(&mut other, 1);
    let line = fs::read_to_string(&path).unwrap().split_off(whole.len());
    let sig = &line[line.find("\"sig\":\"").unwrap() + 7..][..128];
    let forged = line.replace(sig, &"0".repeat(128));
    fs::write(&path, whole + &forged).unwrap();
    assert!(matches!(
        behind.writer(),
        Err(LogError::Corrupt { line: 7, .. })
    ));
}

#[test]
fn a_prepared_open_under_the_lock_reads_only_what_was_appended_since() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = open(dir.path(), key());
    append(&mut log, 20);
    drop(log);
    // Mark the first 20 ops.
    drop(open(dir.path(), key()));

    let prepared = OpLog::prepare(dir.path(), key()).unwrap();
    // Another writer appends 3 ops after prepare and before we take the lock.
    let mut other = open(dir.path(), key());
    append(&mut other, 3);
    drop(other);

    CHECKED.set(0);
    let before = lines_read();
    let lock = Lock::take(dir.path()).unwrap();
    let opening = OpLog::open_prepared(Some(prepared), lock, key()).unwrap();
    let (log, ops) = opening.finish().unwrap();
    assert_eq!(lines_read() - before, 3);
    assert_eq!(CHECKED.get(), 3);
    assert_eq!(ops.len(), 23);
    assert_eq!(log.head().map(|h| h.seq), Some(23));

    // The verified mark now covers all 23 ops.
    CHECKED.set(0);
    assert_eq!(
        read_marked(dir.path(), &key().verifying_key(), log.head())
            .unwrap()
            .len(),
        23
    );
    assert_eq!(CHECKED.get(), 0);
}

#[test]
fn a_prepared_open_falls_back_when_the_log_was_replaced_truncated_or_edited_in_place() {
    let (dir, head) = marked();
    let path = log_path(dir.path());
    let honest = fs::read(&path).unwrap();

    // 1. Truncated between prepare and lock.
    let prepared = OpLog::prepare(dir.path(), key()).unwrap();
    let cut = honest[..honest.len() - 1]
        .iter()
        .rposition(|&b| b == b'\n')
        .unwrap();
    fs::write(&path, &honest[..=cut]).unwrap();
    let opening =
        OpLog::open_prepared(Some(prepared), Lock::take(dir.path()).unwrap(), key()).unwrap();
    assert!(matches!(opening.check(head), Err(LogError::Unanchored(_))));
    drop(opening);
    fs::write(&path, &honest).unwrap();

    // 2. Replaced with a new inode of the same length, signed by another key.
    let thief = SigningKey::from_bytes(&[9; 32]);
    let theirs = tempfile::tempdir().unwrap();
    let mut tlog = open(theirs.path(), thief.clone());
    append(&mut tlog, 3);
    drop(tlog);
    let forged = fs::read(log_path(theirs.path())).unwrap();
    assert_eq!(forged.len(), honest.len());

    let prepared = OpLog::prepare(dir.path(), key()).unwrap();
    fs::remove_file(&path).unwrap();
    fs::write(&path, &forged).unwrap();
    assert!(matches!(
        OpLog::open_prepared(Some(prepared), Lock::take(dir.path()).unwrap(), key()),
        Err(LogError::Corrupt { line: 1, .. })
    ));
    fs::write(&path, &honest).unwrap();

    // 3. Overwritten in place (same inode, same length) between prepare and lock.
    let prepared = OpLog::prepare(dir.path(), key()).unwrap();
    let mut edited = honest.clone();
    edited[10] ^= 1;
    let mut file = OpenOptions::new().write(true).open(&path).unwrap();
    file.write_all(&edited).unwrap();
    file.sync_data().unwrap();
    drop(file);
    assert!(matches!(
        OpLog::open_prepared(Some(prepared), Lock::take(dir.path()).unwrap(), key()),
        Err(LogError::Corrupt { line: 1, .. })
    ));
    fs::write(&path, &honest).unwrap();

    // 4. Prepared under another key is never reused under this key.
    let prepared = OpLog::prepare(dir.path(), key()).unwrap();
    assert!(matches!(
        OpLog::open_prepared(Some(prepared), Lock::take(dir.path()).unwrap(), thief),
        Err(LogError::Corrupt { line: 1, .. })
    ));
}

#[test]
fn a_prepared_open_refuses_a_modified_prefix_even_when_the_log_also_grew() {
    use crate::Store;
    use crate::store::StoreError;

    let (dir, _) = marked();
    let path = log_path(dir.path());
    let honest = fs::read(&path).unwrap();

    // 1. Prefix byte modified (in line 1's signature, leaving all op IDs and
    //    the hash chain intact) AND a valid 4th op appended so size > p.len.
    let prepared = OpLog::prepare(dir.path(), key()).unwrap();
    let mut other = open(dir.path(), key());
    append(&mut other, 1);
    drop(other);
    let mut tampered_and_grown = fs::read(&path).unwrap();
    assert!(tampered_and_grown.len() > honest.len());
    let sig_pos = tampered_and_grown
        .windows(7)
        .position(|w| w == b"\"sig\":\"")
        .unwrap()
        + 7;
    tampered_and_grown[sig_pos] = if tampered_and_grown[sig_pos] == b'0' {
        b'1'
    } else {
        b'0'
    };
    let mut file = OpenOptions::new().write(true).open(&path).unwrap();
    file.write_all(&tampered_and_grown).unwrap();
    drop(file);
    assert!(matches!(
        OpLog::open_prepared(Some(prepared), Lock::take(dir.path()).unwrap(), key()),
        Err(LogError::Corrupt { line: 1, .. })
    ));
    assert_eq!(fs::read(&path).unwrap(), tampered_and_grown);
    fs::write(&path, &honest).unwrap();

    // 2. Same scenario through Store::open_prepared (with valid replayable ops).
    let store_dir = tempfile::tempdir().unwrap();
    let repo = crate::op::Repo {
        owner: "FieldmouseWorks".into(),
        name: "Conary".into(),
    };
    let observe = |store: &mut Store, pr: u64| {
        store
            .record(Body::Observe {
                change: ChangeId::new().unwrap(),
                repo: repo.clone(),
                pr,
                author: crate::op::Actor {
                    kind: crate::op::ActorKind::Person,
                    login: "TusanHomichi".into(),
                    session: None,
                },
            })
            .unwrap();
    };
    let mut store = Store::open(Lock::take(store_dir.path()).unwrap(), key(), None).unwrap();
    for pr in 1..=3 {
        observe(&mut store, pr);
    }
    drop(store);
    let prepared = Store::prepare(store_dir.path(), key()).unwrap();
    let mut other = Store::open(Lock::take(store_dir.path()).unwrap(), key(), None).unwrap();
    observe(&mut other, 4);
    drop(other);
    let store_path = log_path(store_dir.path());
    let mut store_tampered = fs::read(&store_path).unwrap();
    let sig_pos = store_tampered
        .windows(7)
        .position(|w| w == b"\"sig\":\"")
        .unwrap()
        + 7;
    store_tampered[sig_pos] = if store_tampered[sig_pos] == b'0' {
        b'1'
    } else {
        b'0'
    };
    let mut file = OpenOptions::new().write(true).open(&store_path).unwrap();
    file.write_all(&store_tampered).unwrap();
    drop(file);
    assert!(matches!(
        Store::open_prepared(Some(prepared), Lock::take(store_dir.path()).unwrap(), key()),
        Err(StoreError::Log(LogError::Corrupt { line: 1, .. }))
    ));
    assert_eq!(fs::read(&store_path).unwrap(), store_tampered);

    // 3. Prefix byte modified AND a torn tail appended so size > p.len:
    //    must fail and leave the file untouched (never mend the torn tail).
    let prepared = OpLog::prepare(dir.path(), key()).unwrap();
    let mut tampered_and_torn = honest.clone();
    tampered_and_torn[10] ^= 1;
    tampered_and_torn.extend_from_slice(b"{\"id\":\"1234");
    let mut file = OpenOptions::new().write(true).open(&path).unwrap();
    file.write_all(&tampered_and_torn).unwrap();
    drop(file);
    assert!(matches!(
        OpLog::open_prepared(Some(prepared), Lock::take(dir.path()).unwrap(), key()),
        Err(LogError::Corrupt { line: 1, .. })
    ));
    assert_eq!(fs::read(&path).unwrap(), tampered_and_torn);
    fs::write(&path, &honest).unwrap();

    // 4. Torn tail already present at prepare() time, then prefix edited in
    //    place without changing size (still > p.len).
    let honest_with_torn = [&honest[..], b"{\"id\":\"1234"].concat();
    fs::write(&path, &honest_with_torn).unwrap();
    let prepared = OpLog::prepare(dir.path(), key()).unwrap();
    let mut file = OpenOptions::new().write(true).open(&path).unwrap();
    file.write_all(&tampered_and_torn).unwrap();
    drop(file);
    assert!(matches!(
        OpLog::open_prepared(Some(prepared), Lock::take(dir.path()).unwrap(), key()),
        Err(LogError::Corrupt { line: 1, .. })
    ));
    assert_eq!(fs::read(&path).unwrap(), tampered_and_torn);
    fs::write(&path, &honest).unwrap();

    // 5. Same-length in-place overwrite right after prepare() without waiting
    //    for a filesystem timestamp tick.
    for _ in 0..8 {
        fs::write(&path, &honest).unwrap();
        let prepared = OpLog::prepare(dir.path(), key()).unwrap();
        let mut edited = honest.clone();
        edited[10] ^= 1;
        let mut file = OpenOptions::new().write(true).open(&path).unwrap();
        file.write_all(&edited).unwrap();
        drop(file);
        assert!(matches!(
            OpLog::open_prepared(Some(prepared), Lock::take(dir.path()).unwrap(), key()),
            Err(LogError::Corrupt { line: 1, .. })
        ));
    }
}

#[test]
fn a_prepared_open_handles_a_torn_or_unterminated_tail_without_rereading_the_prefix() {
    let (dir, head) = marked();
    let path = log_path(dir.path());
    let whole = fs::read(&path).unwrap();

    // 1. Torn tail present when prepare runs.
    fs::write(&path, [&whole[..], b"{\"id\":\"1234"].concat()).unwrap();
    let prepared = OpLog::prepare(dir.path(), key()).unwrap();
    CHECKED.set(0);
    let before = lines_read();
    let opening =
        OpLog::open_prepared(Some(prepared), Lock::take(dir.path()).unwrap(), key()).unwrap();
    opening.check(head).unwrap();
    let (log, ops) = opening.finish().unwrap();
    assert_eq!(lines_read() - before, 0);
    assert_eq!(CHECKED.get(), 0);
    assert_eq!((ops.len(), log.head()), (3, Some(head)));
    assert_eq!(fs::read(&path).unwrap(), whole);

    // 2. Unterminated 4th op present when prepare runs.
    let mut log = open(dir.path(), key());
    append(&mut log, 1);
    drop(log);
    let four = fs::read(&path).unwrap();
    let unterminated = &four[..four.len() - 1];
    fs::write(&path, unterminated).unwrap();

    let prepared = OpLog::prepare(dir.path(), key()).unwrap();
    CHECKED.set(0);
    let before = lines_read();
    let opening =
        OpLog::open_prepared(Some(prepared), Lock::take(dir.path()).unwrap(), key()).unwrap();
    let (log, ops) = opening.finish().unwrap();
    assert_eq!(lines_read() - before, 1);
    assert_eq!(CHECKED.get(), 1);
    assert_eq!((ops.len(), log.head().map(|h| h.seq)), (4, Some(4)));
    assert_eq!(fs::read(&path).unwrap(), four);
}

#[test]
fn a_prepared_store_replays_only_appended_tail_ops_under_the_lock() {
    use crate::Store;
    use crate::op::{Actor, ActorKind, Repo};

    let dir = tempfile::tempdir().unwrap();
    let repo = Repo {
        owner: "FieldmouseWorks".into(),
        name: "Conary".into(),
    };
    let observe = |store: &mut Store, pr: u64| {
        store
            .record(Body::Observe {
                change: ChangeId::new().unwrap(),
                repo: repo.clone(),
                pr,
                author: Actor {
                    kind: ActorKind::Person,
                    login: "TusanHomichi".into(),
                    session: None,
                },
            })
            .unwrap();
    };
    let mut store = Store::open(Lock::take(dir.path()).unwrap(), key(), None).unwrap();
    for pr in 1..=5 {
        observe(&mut store, pr);
    }
    drop(store);

    let prepared = Store::prepare(dir.path(), key()).unwrap();
    let mut other = Store::open(Lock::take(dir.path()).unwrap(), key(), None).unwrap();
    for pr in 6..=7 {
        observe(&mut other, pr);
    }
    drop(other);

    let before = lines_read();
    let opening =
        Store::open_prepared(Some(prepared), Lock::take(dir.path()).unwrap(), key()).unwrap();
    let store = opening.finish().unwrap();
    assert_eq!(lines_read() - before, 2);
    assert_eq!(store.head().map(|h| h.seq), Some(7));
    for pr in 1..=7 {
        assert!(store.ledger().by_pr(&repo, pr).is_some(), "missing PR {pr}");
    }
}
