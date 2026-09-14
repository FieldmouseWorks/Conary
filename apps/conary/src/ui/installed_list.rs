// apps/conary/src/ui/installed_list.rs
//! Ordinary installed-record list frame for one database request.
//!
//! Rendering reports exactly the records the command adapter queried, in query
//! order, and keeps the selected database and requested name visible. Totals
//! read typed trove variants only; no version, release, or record text
//! establishes a record type, source format, recovery state, or compatibility.

use super::transaction_summary::visible;
use super::{Status, field, heading, message, row};
use conary_core::db::models::{Trove, TroveType};

/// Render the ordinary installed list: one compact record per queried row,
/// then the exact record and typed-kind totals.
pub(crate) fn list(troves: &[Trove], selected_name: Option<&str>, database: &str) {
    heading("Installed records:");
    field("Database", &visible(database));
    if let Some(name) = selected_name {
        field("Name", &visible(name));
    }

    if troves.is_empty() {
        message(match selected_name {
            Some(_) => "No matching installed records.",
            None => "No installed records.",
        });
    }
    for trove in troves {
        let name = visible(&trove.name);
        let version = visible(&trove.version);
        let kind = format!("Type: {}", trove.trove_type.as_str());
        let release = format!(
            "CCS release: {}",
            exact_or_unspecified(trove.package_release.as_deref())
        );
        let architecture = format!(
            "Architecture: {}",
            exact_or_unspecified(trove.architecture.as_deref())
        );
        row(
            Status::Info,
            &[
                name.as_str(),
                version.as_str(),
                kind.as_str(),
                release.as_str(),
                architecture.as_str(),
            ],
        );
    }

    field("Records", &troves.len().to_string());
    field(
        "Packages",
        &typed_count(troves, &TroveType::Package).to_string(),
    );
    field(
        "Components",
        &typed_count(troves, &TroveType::Component).to_string(),
    );
    field(
        "Collections",
        &typed_count(troves, &TroveType::Collection).to_string(),
    );
}

/// Escape recorded values and label absent metadata without inventing a value.
fn exact_or_unspecified(value: Option<&str>) -> String {
    value
        .map(visible)
        .unwrap_or_else(|| "Unspecified".to_owned())
}

fn typed_count(troves: &[Trove], trove_type: &TroveType) -> usize {
    troves
        .iter()
        .filter(|trove| &trove.trove_type == trove_type)
        .count()
}
