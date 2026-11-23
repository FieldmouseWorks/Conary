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
