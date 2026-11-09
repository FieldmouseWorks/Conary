//! The train's ledger observations are local inputs, separate from GitHub's
//! word on whether an isolated PR merges. No train is stepped here.
use super::decide::Decision;
use super::*;
#[derive(Debug, Clone, Default)]
pub struct TrainState {
    pub seat: Option<Seat>,
    pub held: Option<(String, String)>,
}
#[derive(Debug, Clone)]
pub struct Seat {
    pub head: String,
    pub ahead: u32,
    pub candidate: Option<Candidate>,
}
#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: String,
    pub contains: bool,
    pub landing: bool,
    pub peers: Vec<u64>,
}
pub(super) fn apply(f: &Facts, decision: Decision) -> Decision {
    let (stage, step, description) = decision;
    let by_train = f.settings.train && f.revision.base == f.default_branch;
    let ready = matches!(
        step,
        Step::Merge | Step::Settle | Step::JoinTrain | Step::CloseAlreadyLanded
    );
    if f.queued_merge && f.open && (by_train || !ready) {
        return (
            stage,
            Step::DisableAutoMerge,
            "Take GitHub's queued merge off before proceeding".into(),
        );
    }
    if let Some(seat) = &f.train.seat {
        if !by_train || step != Step::JoinTrain {
            return (
                stage,
                Step::LeaveTrain,
                format!("{description}; leave the train while unsettled"),
            );
        }
        if seat.head == f.revision.head {
            let text = if let Some(candidate) = &seat.candidate {
                let id: String = candidate.id.chars().take(8).collect();
                if candidate.contains {
                    let peers = candidate
                        .peers
                        .iter()
                        .map(|n| format!("#{n}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!(
                        "In the train, {} in candidate {id}{}",
                        if candidate.landing {
                            "landing"
                        } else {
                            "gating"
                        },
                        if peers.is_empty() {
                            String::new()
                        } else {
                            format!(" with {peers}")
                        }
                    )
                } else {
                    format!(
                        "In the train, behind candidate {id}, {} ahead of it",
                        seat.ahead
                    )
                }
            } else if seat.ahead > 0 {
                format!("In the train, {} ahead of it", seat.ahead)
            } else {
                "In the train, next in line".into()
            };
            return (Stage::Train, Step::Wait { until: None }, text);
        }
    }
    if step == Step::JoinTrain
        && let Some((head, why)) = &f.train.held
        && head == &f.revision.head
    {
        return (
            Stage::Train,
            step,
            format!("Held out of the train: {why}; ask to join again"),
        );
    }
    (stage, step, description)
}
