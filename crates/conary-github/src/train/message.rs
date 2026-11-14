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
            if seen.insert((key.to_ascii_lowercase(), value)) {
                lines.push(format!("{key}: {value}"));
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
pub fn messages(history: &crate::api::History) -> Result<Vec<&str>, String> {
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
                .and_then(|d| d.message.as_deref())
                .ok_or_else(|| format!("GitHub sent its commit {} without a message", c.sha))
        })
        .collect()
}

/// A commit message's trailers, as (key, value): its final paragraph, when
/// that isn't its subject and every line of it is `Key: value`.
fn trailers(message: &str) -> Vec<(&str, &str)> {
    let mut paragraphs = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in message.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                paragraphs.push(std::mem::take(&mut current));
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        paragraphs.push(current);
    }
    if paragraphs.len() < 2 {
        return Vec::new();
    }
    let last = paragraphs.pop().unwrap_or_default();
    last.iter()
        .map(|line| trailer(line))
        .collect::<Option<Vec<_>>>()
        .unwrap_or_default()
}

/// `Key: value`, where the key is letters, digits and dashes.
fn trailer(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once(": ")?;
    let value = value.trim();
    let token = !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    (token && !value.is_empty()).then_some((key, value))
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
            // Nor a URL, whose colon has no space after it.
            "train: four\n\nhttps://example.com/x",
            // A key with a space isn't a key.
            "train: five\n\nNot a key: value",
        ];
        let message = Message::new(" a title ", 7, commits);
        assert_eq!(message.title, "a title (#7)");
        assert_eq!(message.body, "");
        assert_eq!(message.whole(), "a title (#7)");
    }
}
