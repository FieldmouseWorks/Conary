//! burrow#1424's fixtures, with a stand-in for the old code's module
//! lookup: `mods` names each test module as `mod x;` does (`src/x.rs` or
//! `src/x/mod.rs`, both a clash) or as `#[path]` does (`path:FILE`).
use super::super::plan;
use super::*;

const RUN: &str = r#"while read -r m; do
  case $m in
    path:*) f=${m#path:}; [ -f "$f" ] || exit 1 ;;
    *) a=src/$m.rs; b=src/$m/mod.rs
       if [ -f "$a" ] && [ -f "$b" ]; then exit 1; fi
       if [ -f "$a" ]; then f=$a; elif [ -f "$b" ]; then f=$b; else exit 1; fi ;;
  esac
  bash "$f" || exit 1
done < mods
"#;
const PASSES: &str = "true\n";
const CATCHES: &str = "grep -qx new value\n";

struct Repo(tempfile::TempDir);

impl Repo {
    fn new(files: &[(&str, &str)]) -> Repo {
        let repo = Repo(tempfile::tempdir().unwrap());
        git(repo.path(), &["init", "-q"]).unwrap();
        repo.write(&[("run.sh", RUN), ("value", "old\n")]);
        repo.write(files);
        repo
    }
    fn path(&self) -> &Path {
        self.0.path()
    }
    fn write(&self, files: &[(&str, &str)]) {
        for (path, content) in files {
            let path = self.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
    }
    fn remove(&self, path: &str) {
        std::fs::remove_file(self.path().join(path)).unwrap();
    }
    fn commit(&self) -> String {
        let repo = self.path();
        git(repo, &["add", "-A"]).unwrap();
        let id = ["-c", "user.name=Test", "-c", "user.email=t@example.invalid"];
        git(repo, &[&id[..], &["commit", "-qm", "c"]].concat()).unwrap();
        git(repo, &["rev-parse", "HEAD"]).unwrap().trim().to_owned()
    }
    /// The command on a tree, as a negative run would run it.
    fn passes(&self, tree: &str) -> bool {
        let dir = tempfile::tempdir().unwrap();
        let archive = Command::new("git")
            .arg("-C")
            .arg(self.path())
            .args(["archive", tree])
            .output()
            .unwrap();
        let mut tar = Command::new("tar")
            .args(["-x", "-C"])
            .arg(dir.path())
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        tar.stdin
            .take()
            .unwrap()
            .write_all(&archive.stdout)
            .unwrap();
        assert!(tar.wait().unwrap().success());
        Command::new("bash")
            .arg("run.sh")
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success()
    }
    /// What the negative says of the PR from `base` to the head, and the
    /// trees it ran the command on.
    fn judged(&self, base: &str) -> (Outcome, usize) {
        let head = git(self.path(), &["rev-parse", "HEAD"]).unwrap();
        let plan = plan(self.path(), base, head.trim()).unwrap();
        let mut runs = 0;
        let outcome = judge(&plan.tree.unwrap(), plan.judging.as_ref(), |tree| {
            runs += 1;
            Ok(self.passes(tree))
        })
        .unwrap();
        (outcome, runs)
    }
}

fn unsure(outcome: &Outcome) -> bool {
    matches!(outcome, Outcome::Unsure(_))
}

/// A declared test module moved, its test passing on old and new code:
/// the negative tree's missing file isn't the new tests' failure.
#[test]
fn a_moved_module_that_passes_everywhere_is_never_a_catch() {
    let repo = Repo::new(&[("mods", "a/tests\n"), ("src/a/tests.rs", PASSES)]);
    let base = repo.commit();
    repo.remove("src/a/tests.rs");
    repo.write(&[
        ("mods", "b/tests\n"),
        ("src/b/tests.rs", PASSES),
        ("value", "new\n"),
    ]);
    repo.commit();
    let (outcome, _) = repo.judged(&base);
    assert!(unsure(&outcome), "{outcome:?}");
}

#[test]
fn a_move_beside_a_failing_test_in_a_file_that_stayed_is_a_catch() {
    let repo = Repo::new(&[
        ("mods", "a/tests\nc/tests\n"),
        ("src/a/tests.rs", PASSES),
        ("src/c/tests.rs", PASSES),
    ]);
    let base = repo.commit();
    repo.remove("src/a/tests.rs");
    repo.write(&[
        ("mods", "b/tests\nc/tests\n"),
        ("src/b/tests.rs", PASSES),
        ("src/c/tests.rs", CATCHES),
        ("value", "new\n"),
    ]);
    repo.commit();
    assert_eq!(repo.judged(&base).0, Outcome::Fail);
}

/// A failing test written into the file that moved isn't run by the old
/// code, so it can't be the catch.
#[test]
fn a_test_in_a_file_that_moved_out_of_reach_is_never_a_catch() {
    let repo = Repo::new(&[("mods", "a/tests\n"), ("src/a/tests.rs", PASSES)]);
    let base = repo.commit();
    repo.remove("src/a/tests.rs");
    repo.write(&[
        ("mods", "b/tests\n"),
        ("src/b/tests.rs", CATCHES),
        ("value", "new\n"),
    ]);
    repo.commit();
    assert!(unsure(&repo.judged(&base).0));
}

/// `x.rs` made `x/mod.rs` while the old code names `x.rs` by `#[path]`:
/// it goes back beside its replacement, and only a test that fails there
/// is a catch.
#[test]
fn a_replaced_file_the_old_code_names_by_path_goes_back_beside_it() {
    let files = [("mods", "path:src/tests.rs\n"), ("src/tests.rs", PASSES)];
    let moved = |stayed: &str| {
        let repo = Repo::new(&files);
        repo.write(&[("src/other/tests.rs", PASSES)]);
        repo.write(&[("mods", "path:src/tests.rs\nother/tests\n")]);
        let base = repo.commit();
        repo.remove("src/tests.rs");
        repo.write(&[
            ("mods", "path:src/tests/mod.rs\nother/tests\n"),
            ("src/tests/mod.rs", PASSES),
            ("src/other/tests.rs", stayed),
            ("value", "new\n"),
        ]);
        repo.commit();
        repo.judged(&base).0
    };
    assert!(unsure(&moved(PASSES)));
    assert_eq!(moved(CATCHES), Outcome::Fail);
}

/// `x.rs` made `x/mod.rs` under a plain `mod x;`: the old code finds the
/// replacement, so the negative tree's failure is the new tests'.
#[test]
fn a_replaced_file_the_old_code_finds_is_judged_where_it_went() {
    let split = |content: &str| {
        let repo = Repo::new(&[("mods", "tests\n"), ("src/tests.rs", PASSES)]);
        let base = repo.commit();
        repo.remove("src/tests.rs");
        repo.write(&[("src/tests/mod.rs", content), ("value", "new\n")]);
        repo.commit();
        repo.judged(&base)
    };
    assert_eq!(split(PASSES), (Outcome::Pass, 1));
    assert_eq!(split(CATCHES), (Outcome::Fail, 2));
}

/// With no test file deleted a failure is the new tests', as it was: no
/// tree but the negative one is run.
#[test]
fn without_a_deletion_a_failure_is_judged_on_the_negative_tree_alone() {
    let repo = Repo::new(&[("mods", "tests\n"), ("src/tests.rs", PASSES)]);
    let base = repo.commit();
    repo.write(&[("src/tests.rs", CATCHES), ("value", "new\n")]);
    repo.commit();
    let head = git(repo.path(), &["rev-parse", "HEAD"]).unwrap();
    let plan = plan(repo.path(), &base, head.trim()).unwrap();
    assert!(plan.judging.is_none());
    assert_eq!(repo.judged(&base), (Outcome::Fail, 1));
}

/// A run that breaks attests nothing, rather than counting as a failure.
#[test]
fn a_run_that_breaks_is_never_a_result() {
    let repo = Repo::new(&[("mods", "a/tests\n"), ("src/a/tests.rs", PASSES)]);
    let base = repo.commit();
    repo.remove("src/a/tests.rs");
    repo.write(&[
        ("mods", "b/tests\n"),
        ("src/b/tests.rs", PASSES),
        ("value", "new\n"),
    ]);
    let head = repo.commit();
    let plan = plan(repo.path(), &base, &head).unwrap();
    let mut runs = 0;
    let broke = judge(&plan.tree.unwrap(), plan.judging.as_ref(), |_| {
        runs += 1;
        if runs == 1 {
            Ok(false)
        } else {
            Err("the runner broke".into())
        }
    });
    assert!(broke.is_err());
}

/// A test file made a directory: the negative tree holds the directory's
/// files and nothing else where the file was (main's planner signed a
/// stray `tests/xy` beside them).
#[test]
fn a_test_file_made_a_directory_leaves_only_the_directory() {
    let repo = Repo::new(&[("mods", "path:tests/x\n"), ("tests/x", PASSES)]);
    let base = repo.commit();
    repo.remove("tests/x");
    repo.write(&[("tests/x/y", PASSES), ("value", "new\n")]);
    let head = repo.commit();
    let plan = plan(repo.path(), &base, &head).unwrap();
    let files = git(
        repo.path(),
        &["ls-tree", "-r", "--name-only", &plan.tree.unwrap()],
    )
    .unwrap();
    assert_eq!(files, "mods\nrun.sh\ntests/x/y\nvalue\n");
    // The old code reads the file, and it can't go back beside its
    // directory: a failure there can't be told from the move.
    assert!(unsure(&repo.judged(&base).0));
}
