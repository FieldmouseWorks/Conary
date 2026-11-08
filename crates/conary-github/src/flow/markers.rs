//! Sealed judge and dependency-sweep verdicts. A quoted marker, even posted
//! by the shared author account, is not the job's attested answer.
use super::{Judged, Judgment, Major, Revision, Settings, fingerprint};
use crate::api::{Comment, PullRequest};
use ring::hmac;
use std::fmt;
use subtle::ConstantTimeEq;

#[derive(Clone, PartialEq, Eq)]
pub struct SealKey(String);
impl fmt::Debug for SealKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SealKey([redacted])")
    }
}
impl SealKey {
    pub fn from_hex(text: &str) -> Result<Self, &'static str> {
        if text.len() != 64
            || !text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("seal key must be 64 lowercase hexadecimal characters");
        }
        Ok(Self(text.into()))
    }
    /// Same MAC format as burrow's seal.py. The key is the hexadecimal
    /// text, not its decoded bytes; only the first sixteen tag bytes travel.
    pub fn seal(
        &self,
        repo: &str,
        pr: u64,
        name: &str,
        verdict: &str,
        head: &str,
        on: &str,
    ) -> String {
        let k = hmac::Key::new(hmac::HMAC_SHA256, self.0.as_bytes());
        let text = format!("{repo}#{pr} {name} {verdict} {head} {on}");
        hmac::sign(&k, text.as_bytes()).as_ref()[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

pub(super) struct Markers {
    pub judgment: Option<Judged>,
    pub judge_asked: bool,
    pub major: Option<Major>,
}
pub(super) fn read(
    repo: &str,
    pull: &PullRequest,
    comments: &[Comment],
    settings: &Settings,
) -> Markers {
    let mut result = Markers {
        judgment: None,
        judge_asked: false,
        major: None,
    };
    let pat=regex::Regex::new(r"<!-- (autoland-judgment|major-verdict): ([a-z]+) ([0-9a-f]{40})(?: on (\S+))? ([0-9a-f]{32}) -->").expect("constant regex");
    let judged_on = format!(
        "{} {}",
        pull.base.name,
        fingerprint(pull.body.as_deref().unwrap_or(""))
    );
    for c in comments.iter().filter(|c| {
        c.user
            .as_ref()
            .is_some_and(|u| settings.verdict_authors.contains(&u.login))
    }) {
        for m in pat.captures_iter(c.body.as_deref().unwrap_or("")) {
            let (name, verdict, head, tag) = (&m[1], &m[2], &m[3], &m[5]);
            if head != pull.head.sha {
                continue;
            }
            if name == "major-verdict" {
                if m.get(4).map(|s| s.as_str()) != Some(pull.base.name.as_str()) {
                    continue;
                }
                if settings.major_key.as_ref().is_some_and(|k| {
                    verified(
                        k.seal(
                            &repo.to_lowercase(),
                            pull.number,
                            name,
                            verdict,
                            head,
                            &pull.base.name,
                        ),
                        tag,
                    )
                }) {
                    result.major = match verdict {
                        "ok" => Some(Major::Cleared),
                        "held" => Some(Major::Held),
                        _ => None,
                    };
                }
            } else if let Some(key) = &settings.judge_key {
                if verified(
                    key.seal(repo, pull.number, name, verdict, head, &pull.base.name),
                    tag,
                ) {
                    result.judge_asked = true;
                }
                if verified(
                    key.seal(repo, pull.number, name, verdict, head, &judged_on),
                    tag,
                ) {
                    result.judge_asked = true;
                    result.judgment = match verdict {
                        "land" => Some(Judgment::Land),
                        "owner" => Some(Judgment::Owner),
                        "failed" => Some(Judgment::Failing),
                        "misdescribed" => Some(Judgment::Misdescribed),
                        _ => None,
                    }
                    .map(|verdict| Judged {
                        revision: Revision::from(pull),
                        verdict,
                    });
                }
            }
        }
    }
    result
}

fn verified(expected: String, tag: &str) -> bool {
    bool::from(expected.as_bytes().ct_eq(tag.as_bytes()))
}
