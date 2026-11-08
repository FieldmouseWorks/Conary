//! Provenance is read from every compared commit. Review coverage is never
//! inferred from a branch name, nor from the author of its final commit.
use super::Settings;
use crate::api::History;

pub(super) struct Provenance {
    pub known: bool,
    pub judge: bool,
    pub edited: Option<bool>,
}

/// Git's trailer block is the final paragraph, all of whose lines are
/// trailers or their indented continuations. Prose quoting an Engine line
/// earlier in the message is not a trailer.
fn trailers(message: &str) -> Vec<(&str, &str)> {
    let last = message.trim_end().rsplit("\n\n").next().unwrap_or("");
    let mut result = Vec::new();
    for line in last.lines() {
        if line.starts_with([' ', '\t']) && !result.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            return Vec::new();
        };
        if key.is_empty() || !key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
            return Vec::new();
        }
        result.push((key, value.trim()));
    }
    result
}

pub(super) fn provenance(got: Option<&History>, settings: &Settings, renovate: bool) -> Provenance {
    let Some(got) = got else {
        return Provenance {
            known: false,
            judge: false,
            edited: None,
        };
    };
    let mut judge = !got.complete;
    let mut edited = false;
    for c in &got.commits {
        let Some(detail) = &c.commit else {
            return Provenance {
                known: false,
                judge: false,
                edited: None,
            };
        };
        let Some(message) = detail.message.as_deref() else {
            return Provenance {
                known: false,
                judge: false,
                edited: None,
            };
        };
        let trailers = trailers(message);
        judge |= trailers.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("engine")
                && settings
                    .judge_engines
                    .iter()
                    .any(|e| v.eq_ignore_ascii_case(e))
        });
        edited |= !detail
            .author
            .as_ref()
            .and_then(|a| a.email.as_ref())
            .is_some_and(|e| bot(settings, e));
        edited |= !detail
            .committer
            .as_ref()
            .and_then(|a| a.email.as_ref())
            .is_some_and(|e| e == "noreply@github.com" || bot(settings, e));
        for (_, value) in trailers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("co-authored-by"))
        {
            let email = value
                .rsplit_once('<')
                .and_then(|(_, e)| e.strip_suffix('>'));
            edited |= !email.is_some_and(|e| bot(settings, e));
        }
    }
    Provenance {
        known: true,
        judge,
        edited: if !got.complete || renovate && settings.renovate_email.is_none() {
            None
        } else {
            Some(edited)
        },
    }
}

fn bot(settings: &Settings, email: &str) -> bool {
    settings.bot_emails.contains(email) || settings.renovate_email.as_deref() == Some(email)
}
