//! What a PR's squash or merge commit says: its title, and one line for
//! each distinct trailer its commits carry. GitHub's default would list
//! every commit's whole message, trailers and all, and a one-commit PR's
//! squash would take that commit's subject in place of the PR's title.

/// A landing commit's title and body, as the merge API takes them
/// (`commit_title`, `commit_message`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub title: String,
    pub body: String,
}

impl Message {
    /// PR `pr` titled `title`, landing `commits`' messages, oldest first:
    /// `<title> (#N)`, and the trailers collected across the commits, one
    /// line per distinct one (keys compared case-insensitively, values as
    /// they are) in the order they first appear, spelled as they first
    /// appear.
    pub fn new<'a>(title: &str, pr: u64, commits: impl IntoIterator<Item = &'a str>) -> Self {
        let mut seen = std::collections::BTreeSet::new();
        let mut lines = Vec::new();
        for (key, value) in commits.into_iter().flat_map(trailers) {
            if seen.insert((key.to_ascii_lowercase(), value.clone())) {
                lines.push(format!("{key}: {value}").trim_end().to_owned());
            }
        }
        Self {
            title: format!("{} (#{pr})", title.trim()),
            body: lines.join("\n"),
        }
    }

    /// The whole commit message GitHub makes of it.
    pub fn whole(&self) -> String {
        if self.body.is_empty() {
            self.title.clone()
        } else {
            format!("{}\n\n{}", self.title, self.body)
        }
    }
}

/// The messages of `history`'s commits, oldest first, for [`Message::new`];
/// or why there's no saying their trailers: a history GitHub cut short, or
/// a commit it sent without a message. A landing commit that dropped a
/// commit's co-author or session would lose who wrote it.
pub fn messages(history: &crate::api::History) -> Result<Vec<String>, String> {
    if !history.complete {
        return Err(format!(
            "GitHub listed only {} of its commits, so its landing can't say all their trailers",
            history.commits.len()
        ));
    }
    history
        .commits
        .iter()
        .map(|c| {
            c.commit
                .as_ref()
                .and_then(|d| d.message.clone())
                .ok_or_else(|| format!("GitHub sent its commit {} without a message", c.sha))
        })
        .collect()
}

/// A commit message's trailers, as (key, value), read as Git reads them
/// (`git interpret-trailers --parse --no-divider`, `%(trailers)`): the
/// last paragraph after the subject is the trailer block when every line
/// of it is a trailer, or when it holds a Git-generated one
/// (`Signed-off-by: `) and at least a quarter of its lines are trailers. A
/// trailer is a key of letters, digits and dashes, optionally followed by
/// spaces or tabs, then `:`; lines that start with whitespace continue the
/// trailer before them, and its value is unfolded to one line. Lines
/// starting with `#` are comments, and the block's prose lines aren't
/// trailers.
fn trailers(message: &str) -> Vec<(String, String)> {
    let lines: Vec<&str> = message.lines().collect();
    // Trailing blank and comment lines aren't part of the message.
    let end = lines
        .iter()
        .rposition(|l| !blank(l) && !comment(l))
        .map_or(0, |i| i + 1);
    let lines = &lines[..end];
    // The subject, the first paragraph, can't be trailers.
    let title_end = lines
        .iter()
        .position(|l| !comment(l) && blank(l))
        .unwrap_or(lines.len());
    let Some(start) = block_start(lines, title_end) else {
        return Vec::new();
    };
    // Each trailer with its continuation lines.
    let mut folded: Vec<String> = Vec::new();
    let mut continues = false;
    for line in &lines[start..] {
        if continues
            && line.starts_with(git_space)
            && let Some(last) = folded.last_mut()
        {
            last.push('\n');
            last.push_str(line);
            continue;
        }
        folded.push((*line).to_owned());
        continues = separator(line).is_some();
    }
    folded
        .iter()
        .filter(|t| !comment(t))
        .filter_map(|t| {
            let at = separator(t)?;
            let key = t[..at].trim_matches(git_space).to_owned();
            Some((key, unfold(t[at + 1..].trim_matches(git_space))))
        })
        .collect()
}

/// Where the trailer block starts in `lines`, the subject ending at
/// `title_end`: Git's `find_trailer_block_start`.
fn block_start(lines: &[&str], title_end: usize) -> Option<usize> {
    const GIT_GENERATED: [&str; 2] = ["Signed-off-by: ", "(cherry picked from commit "];
    let (mut recognized, mut trailer_lines, mut prose, mut continuations) = (false, 0, 0, 0);
    for i in (title_end..lines.len()).rev() {
        let line = lines[i];
        if comment(line) {
            prose += continuations;
            continuations = 0;
        } else if blank(line) {
            prose += continuations;
            let all = trailer_lines > 0 && prose == 0;
            let enough = recognized && trailer_lines * 3 >= prose;
            return (all || enough).then_some(i + 1);
        } else if GIT_GENERATED.iter().any(|p| line.starts_with(p)) {
            trailer_lines += 1;
            continuations = 0;
            recognized = true;
        } else if separator(line).is_some() {
            trailer_lines += 1;
            continuations = 0;
        } else if line.starts_with(git_space) {
            continuations += 1;
        } else {
            prose += 1 + continuations;
            continuations = 0;
        }
    }
    None
}

/// Where a trailer's `:` is: after a key of ASCII letters, digits and
/// dashes, and any spaces or tabs after it. Git's `find_separator`.
fn separator(line: &str) -> Option<usize> {
    let mut spaced = false;
    for (i, c) in line.bytes().enumerate() {
        if c == b':' {
            return (i >= 1).then_some(i);
        }
        if !spaced && (c.is_ascii_alphanumeric() || c == b'-') {
            continue;
        }
        if i != 0 && (c == b' ' || c == b'\t') {
            spaced = true;
            continue;
        }
        return None;
    }
    None
}

/// A folded value on one line, each line break and the whitespace after
/// it one space.
fn unfold(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value.split('\n');
    if let Some(first) = rest.next() {
        out.push_str(first);
    }
    for line in rest {
        out.push(' ');
        out.push_str(line.trim_start_matches(git_space));
    }
    out
}

/// Git's whitespace: space, tab, line feed, carriage return.
fn git_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

fn blank(line: &str) -> bool {
    line.chars().all(git_space)
}

fn comment(line: &str) -> bool {
    line.starts_with('#')
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUTHOR: &str = "A Bot <bot@example.com>";

    #[test]
    fn one_line_per_distinct_trailer_across_the_commits() {
        let commits = [
            format!(
                "train: land it\n\nWhy it lands.\n\nCo-Authored-By: {AUTHOR}\nClaude-Session: https://s/1"
            ),
            format!(
                "train: answer the review\n\nCo-authored-by: {AUTHOR}\nClaude-Session: https://s/2"
            ),
            format!("train: and again\n\nClaude-Session: https://s/1\nCO-AUTHORED-BY: {AUTHOR}"),
        ];
        let message = Message::new(
            "train: land a squash",
            352,
            commits.iter().map(String::as_str),
        );
        assert_eq!(message.title, "train: land a squash (#352)");
        assert_eq!(
            message.body,
            format!(
                "Co-Authored-By: {AUTHOR}\nClaude-Session: https://s/1\nClaude-Session: https://s/2"
            )
        );
        assert_eq!(
            message.whole(),
            format!("{}\n\n{}", message.title, message.body)
        );
    }

    fn history(messages: &[Option<&str>], complete: bool) -> crate::api::History {
        let commits = messages
            .iter()
            .enumerate()
            .map(|(i, m)| {
                serde_json::from_value(serde_json::json!({
                    "sha": format!("c{i}"),
                    "parents": [],
                    "commit": {"message": m},
                }))
                .unwrap()
            })
            .collect();
        crate::api::History { commits, complete }
    }

    #[test]
    fn a_history_cut_short_or_a_commit_without_a_message_says_nothing() {
        let whole = history(&[Some("a: one"), Some("a: two\n\nX-Y: z")], true);
        assert_eq!(messages(&whole).unwrap(), ["a: one", "a: two\n\nX-Y: z"]);
        let cut = history(&[Some("a: one")], false);
        assert_eq!(
            messages(&cut).unwrap_err(),
            "GitHub listed only 1 of its commits, so its landing can't say all their trailers"
        );
        let bare = history(&[Some("a: one"), None], true);
        assert_eq!(
            messages(&bare).unwrap_err(),
            "GitHub sent its commit c1 without a message"
        );
    }

    #[test]
    fn only_a_final_paragraph_of_trailers_after_the_subject_counts() {
        let commits = [
            // A subject alone looks like a trailer, and isn't one.
            "train: land it",
            // Trailer-like lines in the middle of a body aren't trailers.
            "train: two\n\nSee: this\n\nand that, which ends it.",
            // A final paragraph with prose in it isn't trailers.
            "train: three\n\nFixes: #1\nsome prose",
            // A key with a space isn't a key.
            "train: four\n\nNot a key: value",
            // Too few trailers among prose, even with a Git-generated one.
            "train: five\n\none\ntwo\nthree\nfour\nSigned-off-by: S <s@x>",
            // A line that starts with whitespace continues no trailer.
            "train: six\n\n  leading: x\nKey: v",
            // Trailing comments and blank lines leave only the subject.
            "Key: v\n\n# a comment\n\n",
        ];
        let message = Message::new(" a title ", 7, commits);
        assert_eq!(message.title, "a title (#7)");
        assert_eq!(message.body, "");
        assert_eq!(message.whole(), "a title (#7)");
    }

    /// Each case's expected trailers are what `git interpret-trailers
    /// --parse --no-divider` prints for its message.
    fn read(message: &str) -> Vec<String> {
        trailers(message)
            .into_iter()
            .map(|(k, v)| format!("{k}: {v}").trim_end().to_owned())
            .collect()
    }

    #[test]
    fn trailers_spelled_any_way_git_reads_them() {
        assert_eq!(
            read(
                "a: b\n\nCo-authored-by:\tA <a@x>\nClaude-Session:https://s/1\nReviewed-by : R\nAcked-by\t:K\n"
            ),
            [
                "Co-authored-by: A <a@x>",
                "Claude-Session: https://s/1",
                "Reviewed-by: R",
                "Acked-by: K"
            ]
        );
        assert_eq!(read("a: b\n\nEmpty:\nKey: v\n"), ["Empty:", "Key: v"]);
        // Git reads a URL's scheme as a key.
        assert_eq!(
            read("a: b\n\nhttps://example.com/x\n"),
            ["https: //example.com/x"]
        );
    }

    #[test]
    fn a_folded_trailer_is_one_line() {
        assert_eq!(
            read(
                "a: b\n\nCo-authored-by: A Very Long\n  Name <a@x>\nClaude-Session: https://s/2\n"
            ),
            [
                "Co-authored-by: A Very Long Name <a@x>",
                "Claude-Session: https://s/2"
            ]
        );
        // CRLF lines read as their LF spelling would (Git keeps the CR).
        assert_eq!(
            read("a: b\r\n\r\nCo-authored-by: A\r\n\tB <a@x>\r\n"),
            ["Co-authored-by: A B <a@x>"]
        );
    }

    #[test]
    fn a_block_with_prose_keeps_its_trailers() {
        assert_eq!(
            read(
                "a: b\n\nWhy it lands, in prose.\nSigned-off-by: S <s@x>\nCo-authored-by: A <a@x>\n"
            ),
            ["Signed-off-by: S <s@x>", "Co-authored-by: A <a@x>"]
        );
        assert_eq!(
            read("a: b\n\nsome prose\n(cherry picked from commit abc)\nSigned-off-by: S <s@x>\n"),
            ["Signed-off-by: S <s@x>"]
        );
        assert_eq!(
            read(
                "a: b\n\nCo-authored-by: A <a@x>\n# a comment\nClaude-Session: https://s/3\n\n# trailing\n"
            ),
            ["Co-authored-by: A <a@x>", "Claude-Session: https://s/3"]
        );
        assert_eq!(read("a: b\nstill subject\n\nKey: v\n"), ["Key: v"]);
    }

    #[test]
    fn trailers_spelled_differently_are_one_line() {
        let commits = [
            format!("train: one\n\nCo-authored-by:{AUTHOR}\nClaude-Session: https://s/\n 1"),
            format!("train: two\n\nWhy.\nSigned-off-by: {AUTHOR}\nco-authored-by :  {AUTHOR}"),
            format!("train: three\n\nclaude-session:\thttps://s/ 1\nSigned-off-by:\t{AUTHOR}"),
        ];
        let message = Message::new("train: land it", 9, commits.iter().map(String::as_str));
        assert_eq!(
            message.body,
            format!(
                "Co-authored-by: {AUTHOR}\nClaude-Session: https://s/ 1\nSigned-off-by: {AUTHOR}"
            )
        );
    }
}
