//! The extra read-only GitHub projection the flow needs. REST supplies the
//! complete reviews/checks/commits; GraphQL supplies thread resolution and
//! mergeability, which REST does not expose with the same semantics.
use super::*;
use crate::flow::{BaseObservation, Ci, Read, Revision, Snapshot, Suite, Workflow};
use serde::Deserialize;
use std::collections::BTreeMap;

const QUERY: &str = r#"query($owner:String!,$name:String!,$number:Int!) {
 repository(owner:$owner,name:$name) { defaultBranchRef { name target { ... on Commit { oid statusCheckRollup { state } } } } pullRequest(number:$number) {
  headRefOid baseRefName title body isDraft mergeStateStatus updatedAt
  labels(first:100) { pageInfo { hasNextPage } }
  reviewThreads(first:100) { pageInfo { hasNextPage } nodes { id isResolved comments(first:1) { nodes { pullRequestReview { databaseId } } } } }
  reviews { totalCount } comments { totalCount }
  timelineItems(last:50,itemTypes:[UNLABELED_EVENT,RENAMED_TITLE_EVENT]) { pageInfo { hasPreviousPage } nodes { ... on UnlabeledEvent { createdAt label { name } } ... on RenamedTitleEvent { createdAt previousTitle } } }
  commits(last:1) { nodes { commit { oid checkSuites(first:100) {
   pageInfo { hasNextPage } nodes { databaseId status conclusion app { slug }
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
    labels: Paged,
    review_threads: Threads,
    reviews: Count,
    comments: Count,
    commits: Commits,
    timeline_items: Timeline,
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
    comments: Option<ThreadComments>,
}
#[derive(Deserialize)]
struct ThreadComments {
    nodes: Vec<ThreadComment>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadComment {
    pull_request_review: Option<ReviewId>,
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

    fn carries(&self, repo: &str, base: &str, head: &str, old: &str) -> Result<bool, ApiError> {
        Api::carries(self, repo, base, head, old)
    }
    fn snapshot(&self, repo: &str, number: u64) -> Result<Snapshot, ApiError> {
        let (owner, name) = repo
            .split_once('/')
            .ok_or_else(|| ApiError::new("repository is not owner/name"))?;
        let url = if let Some(root) = self.base.strip_suffix("/api/v3") {
            format!("{root}/api/graphql")
        } else {
            format!("{}/graphql", self.base)
        };
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
        for node in p.commits.nodes {
            if node.commit.oid != p.head_ref_oid {
                return Err(ApiError::new("PR moved within its GraphQL snapshot"));
            }
            checks_complete &= !node.commit.check_suites.page_info.has_next_page;
            for s in node.commit.check_suites.nodes {
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
                .as_ref()
                .and_then(|c| c.nodes.first())
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
                .iter()
                .map(|t| (t.id.clone(), t.is_resolved))
                .collect(),
            resolved_reviews,
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
