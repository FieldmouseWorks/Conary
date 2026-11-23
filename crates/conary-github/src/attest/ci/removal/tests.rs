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
/// A passing test file edited as it moved: a byte-identical move adds no
/// test and asks for no negative at all (`adds_tests`), so a fixture that
/// is to reach the judging moves this instead.
const MOVED: &str = "true # moved\n";
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
    /// A tree's files in `dir`, as a negative run checks one out.
    fn extract(&self, tree: &str, dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        let archive = Command::new("git")
            .arg("-C")
            .arg(self.path())
            .args(["archive", tree])
            .output()
            .unwrap();
        let mut tar = Command::new("tar")
            .args(["-x", "-C"])
            .arg(dir)
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
    }
    /// The command on a tree, as a negative run would run it.
    fn passes(&self, tree: &str) -> bool {
        let dir = tempfile::tempdir().unwrap();
        self.extract(tree, dir.path());
        run_in(dir.path())
    }
    /// What the negative says of the PR from `base` to the head, and the
    /// trees it ran the command on.
    fn judged(&self, base: &str) -> (Outcome, usize) {
        let (outcome, runs) = self.judged_by(base, |tree| self.passes(tree));
        (outcome, runs.len())
    }
    /// The same with each run made by `passes`, and the trees in the order
    /// it ran them, checked to run no tree of old code alone after a tree
    /// with the new tests in it.
    fn judged_by(
        &self,
        base: &str,
        mut passes: impl FnMut(&str) -> bool,
    ) -> (Outcome, Vec<String>) {
        let head = git(self.path(), &["rev-parse", "HEAD"]).unwrap();
        let plan = plan(self.path(), base, head.trim()).unwrap();
        let negative = plan.tree.unwrap();
        let mut runs = vec![];
        let outcome = judge(&negative, plan.judging.as_ref(), |tree| {
            runs.push(tree.to_owned());
            Ok(passes(tree))
        })
        .unwrap();
        let put_back: Vec<_> = plan
            .judging
            .iter()
            .flat_map(|j| &j.restored)
            .map(|t| &t.tree)
            .collect();
        let after = runs.iter().skip_while(|t| **t != negative).skip(1);
        for tree in after {
            assert!(
                put_back.contains(&tree),
                "{tree} ran after the new tests did"
            );
        }
        (outcome, runs)
    }
    /// The same with every planned tree checked out beside the others before
    /// any runs, as the workflow has them, so that a test can reach the
    /// trees that run after it.
    fn judged_side_by_side(&self, base: &str) -> Outcome {
        let head = git(self.path(), &["rev-parse", "HEAD"]).unwrap();
        let plan = plan(self.path(), base, head.trim()).unwrap();
        let root = tempfile::tempdir().unwrap();
        let j = plan.judging.as_ref().unwrap();
        let trees = [plan.tree.clone().unwrap(), j.removed.clone()]
            .into_iter()
            .chain(
                j.restored
                    .iter()
                    .flat_map(|t| [t.control.clone(), t.tree.clone()]),
            );
        for tree in trees {
            self.extract(&tree, &root.path().join(&tree));
        }
        self.judged_by(base, |tree| run_in(&root.path().join(tree)))
            .0
    }
}
fn run_in(dir: &Path) -> bool {
    Command::new("bash")
        .arg("run.sh")
        .current_dir(dir)
        .status()
        .unwrap()
        .success()
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
        ("src/b/tests.rs", MOVED),
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
            ("src/tests/mod.rs", MOVED),
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
/// replacement, so the negative tree's failure is the new tests'. Each
/// runs the removal alone (the old content where it went) before the
/// negative tree, which holds the edited replacement.
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
    assert_eq!(split(MOVED), (Outcome::Pass, 2));
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
        ("src/b/tests.rs", MOVED),
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
    repo.write(&[("tests/x/y", MOVED), ("value", "new\n")]);
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

/// A new test the old code runs, which writes the test file the PR moved
/// into every other tree's checkout: a process left behind would, once that
/// checkout is the tests'. The removal alone ran first, so it still fails
/// for the missing file, and nothing is attested.
#[test]
fn a_new_test_never_reaches_the_removal_alone() {
    const PUTS_IT_BACK: &str = r#"for d in ../*/; do
  [ "$d" -ef . ] || { mkdir -p "$d/src/a" && echo true > "$d/src/a/tests.rs"; }
done
"#;
    let repo = Repo::new(&[
        ("mods", "c/tests\na/tests\n"),
        ("src/a/tests.rs", PASSES),
        ("src/c/tests.rs", PASSES),
    ]);
    let base = repo.commit();
    repo.remove("src/a/tests.rs");
    repo.write(&[
        ("src/b/tests.rs", PASSES),
        ("src/c/tests.rs", PUTS_IT_BACK),
        ("value", "new\n"),
    ]);
    repo.commit();
    let outcome = repo.judged_side_by_side(&base);
    assert!(unsure(&outcome), "{outcome:?}");
}

/// The same for a control: the new test takes the replacement out of each
/// old-code tree with both copies, so that one would pass, while the tree
/// with the new tests keeps the clash. The control ran first, so it still
/// fails, and nothing is attested.
#[test]
fn a_new_test_never_reaches_a_control() {
    const CLEARS_THE_CLASH: &str = r#"# new
for d in ../*/; do
  [ "$d" -ef . ] || [ ! -f "$d/src/tests.rs" ] || grep -q new "$d/src/c/tests.rs" ||
    rm -f "$d/src/tests/mod.rs"
done
"#;
    let repo = Repo::new(&[
        ("mods", "c/tests\npath:src/tests.rs\ntests\n"),
        ("src/tests.rs", PASSES),
        ("src/c/tests.rs", PASSES),
    ]);
    let base = repo.commit();
    repo.remove("src/tests.rs");
    repo.write(&[
        ("src/tests/mod.rs", PASSES),
        ("src/c/tests.rs", CLEARS_THE_CLASH),
        ("value", "new\n"),
    ]);
    repo.commit();
    let outcome = repo.judged_side_by_side(&base);
    assert!(unsure(&outcome), "{outcome:?}");
    // Untouched, the same PR is judged the same.
    assert!(unsure(&repo.judged(&base).0));
}
