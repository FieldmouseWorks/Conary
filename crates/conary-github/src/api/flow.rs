//! The extra read-only GitHub projection the flow needs. REST supplies the
//! complete reviews/checks/commits; GraphQL supplies thread resolution and
//! mergeability, which REST does not expose with the same semantics.
use super::*;
use crate::flow::{
    BaseObservation, Ci, Read, ReviewThread, Revision, Snapshot, Suite, ThreadComment, Workflow,
};
use serde::Deserialize;
use std::collections::BTreeMap;

const QUERY: &str = r#"query($owner:String!,$name:String!,$number:Int!) {
 repository(owner:$owner,name:$name) { defaultBranchRef { name target { ... on Commit { oid statusCheckRollup { state } } } } pullRequest(number:$number) {
  headRefOid baseRefName title body isDraft mergeStateStatus updatedAt lastEditedAt
  labels(first:100) { pageInfo { hasNextPage } }
  reviewThreads(first:100) { pageInfo { hasNextPage } nodes { id isResolved path line originalLine comments(first:100) { pageInfo { hasNextPage } nodes { author { login } body pullRequestReview { databaseId } } } } }
  reviews { totalCount } comments { totalCount }
  timelineItems(last:50,itemTypes:[UNLABELED_EVENT,RENAMED_TITLE_EVENT]) { pageInfo { hasPreviousPage } nodes { ... on UnlabeledEvent { createdAt label { name } } ... on RenamedTitleEvent { createdAt previousTitle } } }
  retargets: timelineItems(last:1,itemTypes:[BASE_REF_CHANGED_EVENT]) { nodes { ... on BaseRefChangedEvent { createdAt } } }
  renames: timelineItems(last:1,itemTypes:[RENAMED_TITLE_EVENT]) { nodes { ... on RenamedTitleEvent { createdAt } } }
  commits(last:1) { nodes { commit { oid checkSuites(first:100) {
   pageInfo { hasNextPage } nodes { databaseId createdAt status conclusion app { slug }
    workflowRun { file { path } workflow { id name } }
   }
  } } } }
 } }
}"#;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Timeline {
    page_info: Previous,
    nodes: Vec<Event>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Previous {
    has_previous_page: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Event {
    created_at: String,
    label: Option<Label>,
    previous_title: Option<String>,
}
#[derive(Deserialize)]
struct DefaultBranch {
    name: String,
    target: Option<BaseTip>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BaseTip {
    oid: String,
    status_check_rollup: Option<Rollup>,
}
#[derive(Deserialize)]
struct Rollup {
    state: String,
}
#[derive(Deserialize)]
struct Envelope {
    data: Option<Data>,
    errors: Option<Vec<serde_json::Value>>,
}
#[derive(Deserialize)]
struct Data {
    repository: Option<Repository>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repository {
    default_branch_ref: Option<DefaultBranch>,
    pull_request: Option<Pull>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pull {
    head_ref_oid: String,
    base_ref_name: String,
    title: String,
    body: Option<String>,
    is_draft: bool,
    merge_state_status: String,
    updated_at: String,
    last_edited_at: Option<String>,
    labels: Paged,
    review_threads: Threads,
    reviews: Count,
    comments: Count,
    commits: Commits,
    timeline_items: Timeline,
    retargets: Retargets,
    renames: Retargets,
}
#[derive(Deserialize)]
/// The last timeline event of one kind, when it happened.
struct Retargets {
    nodes: Vec<Retarget>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Retarget {
    created_at: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Paged {
    page_info: More,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct More {
    has_next_page: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Threads {
    page_info: More,
    nodes: Vec<Thread>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Thread {
    id: String,
    is_resolved: bool,
    path: Option<String>,
    line: Option<u64>,
    original_line: Option<u64>,
    comments: ThreadComments,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadComments {
    page_info: More,
    nodes: Vec<ThreadNode>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadNode {
    author: Option<Login>,
    body: String,
    pull_request_review: Option<ReviewId>,
}
#[derive(Deserialize)]
struct Login {
    login: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewId {
    database_id: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Count {
    total_count: usize,
}
#[derive(Deserialize)]
struct Commits {
    nodes: Vec<CommitNode>,
}
#[derive(Deserialize)]
struct CommitNode {
    commit: Head,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Head {
    oid: String,
    check_suites: Suites,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Suites {
    page_info: More,
    nodes: Vec<SuiteNode>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SuiteNode {
    database_id: u64,
    created_at: String,
    status: String,
    conclusion: Option<String>,
    app: Option<App>,
    workflow_run: Option<Run>,
}
#[derive(Deserialize)]
struct App {
    slug: String,
}
#[derive(Deserialize)]
struct Run {
    file: Option<File>,
    workflow: Flow,
}
#[derive(Deserialize)]
struct File {
    path: String,
}
#[derive(Deserialize)]
struct Flow {
    id: String,
    name: String,
}
#[derive(Deserialize)]
struct Annotation {
    message: String,
}

impl Api {
    /// GitHub's GraphQL endpoint beside this REST root, Enterprise's too.
    pub(super) fn graphql(&self) -> String {
        if let Some(root) = self.base.strip_suffix("/api/v3") {
            format!("{root}/api/graphql")
        } else {
            format!("{}/graphql", self.base)
        }
    }
}

impl Read for Api {
    fn pushes(&self, repo: &str, branch: &str) -> Result<BTreeMap<String, String>, ApiError> {
        #[derive(Deserialize)]
        struct Push {
            after: String,
            actor: Option<User>,
        }
        let encoded: String = branch
            .bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect();
        let records: Vec<Push> = self.pages(&format!(
            "/repos/{repo}/activity?activity_type=push&ref=refs%2Fheads%2F{encoded}"
        ))?;
        let mut result = BTreeMap::new();
        for p in records {
            if let Some(actor) = p.actor {
                result.entry(p.after).or_insert(actor.login);
            }
        }
        Ok(result)
    }

    fn snapshot(&self, repo: &str, number: u64) -> Result<Snapshot, ApiError> {
        let (owner, name) = repo
            .split_once('/')
            .ok_or_else(|| ApiError::new("repository is not owner/name"))?;
        let url = self.graphql();
        let mut response=self.agent.post(&url)
            .header("Authorization",&format!("Bearer {}",self.tokens.of(repo)?))
            .send_json(serde_json::json!({"query":QUERY,"variables":{"owner":owner,"name":name,"number":number}}))
            .map_err(|e|unreachable(&url,e))?;
        if !response.status().is_success() {
            return Err(failed(&url, &mut response));
        }
        let body = read(&url, &mut response)?;
        let envelope: Envelope = parse(&url, &body)?;
        if let Some(errors) = envelope.errors.filter(|e| !e.is_empty()) {
            return Err(if errors.iter().any(|e| e["type"] == "RATE_LIMITED") {
                ApiError::unavailable("GitHub GraphQL rate limit")
            } else {
                ApiError::new("GitHub GraphQL returned errors instead of a complete snapshot")
            });
        }
        let repository = envelope
            .data
            .and_then(|d| d.repository)
            .ok_or_else(|| ApiError::new("GitHub did not return the repository snapshot"))?;
        let default_branch = repository.default_branch_ref.and_then(|b| {
            b.target.map(|t| BaseObservation {
                name: b.name,
                head: t.oid,
                ci: match t.status_check_rollup.as_ref().map(|r| r.state.as_str()) {
                    Some("FAILURE") => Ci::Red,
                    Some("SUCCESS") => Ci::Green,
                    _ => Ci::Pending,
                },
            })
        });
        let p = repository
            .pull_request
            .ok_or_else(|| ApiError::new("GitHub did not return the PR snapshot"))?;
        let mut suites = BTreeMap::new();
        let mut checks_complete = true;
        // When GitHub got the head, as near as it says: the earliest check
        // suite on it, each App that checks the repo getting one as the
        // push comes in. A commit's own dates say when it was made.
        let mut pushed: Option<u64> = None;
        for node in p.commits.nodes {
            if node.commit.oid != p.head_ref_oid {
                return Err(ApiError::new("PR moved within its GraphQL snapshot"));
            }
            checks_complete &= !node.commit.check_suites.page_info.has_next_page;
            for s in node.commit.check_suites.nodes {
                let at = crate::flow::reviews::timestamp(&s.created_at)?;
                pushed = Some(pushed.map_or(at, |p| p.min(at)));
                suites.insert(
                    s.database_id,
                    Suite {
                        status: s.status.to_lowercase(),
                        conclusion: s.conclusion.map(|s| s.to_lowercase()),
                        app: s.app.map_or_else(String::new, |a| a.slug),
                        workflow: s.workflow_run.map(|r| Workflow {
                            id: r.workflow.id,
                            name: r.workflow.name,
                            path: r.file.map(|f| f.path),
                        }),
                    },
                );
            }
        }
        let mut resolved_reviews = std::collections::BTreeSet::new();
        let mut unresolved_reviews = std::collections::BTreeSet::new();
        for t in &p.review_threads.nodes {
            if let Some(id) = t
                .comments
                .nodes
                .first()
                .and_then(|c| c.pull_request_review.as_ref())
                .map(|r| r.database_id)
            {
                if t.is_resolved {
                    resolved_reviews.insert(id);
                } else {
                    unresolved_reviews.insert(id);
                }
            }
        }
        resolved_reviews.retain(|id| !unresolved_reviews.contains(id));
        let mut rebase_removed = Vec::new();
        let mut oldest = None;
        for event in p.timeline_items.nodes {
            let at = crate::flow::reviews::timestamp(&event.created_at)?;
            oldest.get_or_insert(at);
            if event.label.is_some_and(|l| l.name == "rebase")
                || event
                    .previous_title
                    .is_some_and(|t| t.starts_with("rebase!"))
            {
                rebase_removed.push(at);
            }
        }
        Ok(Snapshot {
            default_branch,
            rebase_removed,
            timeline_cut_at: if p.timeline_items.page_info.has_previous_page {
                oldest
            } else {
                None
            },
            revision: Revision::for_parts(
                p.head_ref_oid,
                p.base_ref_name,
                &p.title,
                p.body.as_deref().unwrap_or(""),
            ),
            draft: p.is_draft,
            complete: !p.labels.page_info.has_next_page
                && !p.review_threads.page_info.has_next_page
                && !p
                    .review_threads
                    .nodes
                    .iter()
                    .any(|t| t.comments.page_info.has_next_page)
                && p.reviews.total_count <= 100
                && p.comments.total_count <= 100,
            merge_state: p.merge_state_status,
            open_threads: p
                .review_threads
                .nodes
                .iter()
                .filter(|t| !t.is_resolved)
                .count() as u32,
            threads: p
                .review_threads
                .nodes
                .into_iter()
                .map(|t| {
                    let thread = ReviewThread {
                        resolved: t.is_resolved,
                        path: t.path.unwrap_or_default(),
                        // An outdated thread has no line on the head: the
                        // line it was opened on is where it points.
                        line: t.line.or(t.original_line),
                        comments: t
                            .comments
                            .nodes
                            .into_iter()
                            .map(|c| ThreadComment {
                                author: c.author.map(|a| a.login),
                                body: c.body,
                            })
                            .collect(),
                    };
                    (t.id, thread)
                })
                .collect(),
            resolved_reviews,
            since: crate::flow::reviews::Since {
                // `lastEditedAt` dates only the body's edits; a rename is
                // its own timeline event, and the title is as much the
                // claim a review read.
                edited: p
                    .last_edited_at
                    .as_deref()
                    .into_iter()
                    .chain(p.renames.nodes.last().map(|r| r.created_at.as_str()))
                    .map(crate::flow::reviews::timestamp)
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .max(),
                retargeted: p
                    .retargets
                    .nodes
                    .last()
                    .map(|r| crate::flow::reviews::timestamp(&r.created_at))
                    .transpose()?,
                pushed,
            },
            updated_at: crate::flow::reviews::timestamp(&p.updated_at)?,
            suites,
            checks_complete,
        })
    }
    fn annotations(&self, repo: &str, run: u64) -> Result<Vec<String>, ApiError> {
        let all: Vec<Annotation> =
            self.pages(&format!("/repos/{repo}/check-runs/{run}/annotations"))?;
        Ok(all.into_iter().map(|a| a.message).collect())
    }
}
