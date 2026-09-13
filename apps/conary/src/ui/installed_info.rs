// apps/conary/src/ui/installed_info.rs
//! Installed-package detail frame for the recorded trove observation.
//!
//! Rendering reports exactly what the command adapter gathered. It performs no
//! database, filesystem, or network reads, never parses a version, and never
//! infers a source format, recovery state, or compatibility from version
//! grammar. Optional observations are omitted rather than defaulted, and the
//! CCS release stays its own field, separate from the version text.

use super::transaction_summary::visible;
use super::{field_line, heading_line, message};
use conary_core::db::models::{Component, InstalledRequirementAtom, ProvideEntry, Trove};

/// Exact recorded observations for one installed package.
pub(crate) struct InstalledPackageInfo<'a> {
    pub(crate) trove: &'a Trove,
    /// Prepared authority label, already resolved by the command adapter.
    pub(crate) authority: &'a str,
    /// Prepared repository name, when one was recorded.
    pub(crate) repository: Option<&'a str>,
    pub(crate) file_records: usize,
    pub(crate) payload_bytes: u64,
    pub(crate) dependencies: &'a [InstalledRequirementAtom],
    pub(crate) provides: &'a [ProvideEntry],
    pub(crate) components: &'a [Component],
}

/// Render the complete frame as one coordinated message.
pub(crate) fn details(info: &InstalledPackageInfo<'_>) {
    message(&detail_lines(info).join("\n"));
}

fn yes_no(recorded: bool) -> &'static str {
    if recorded { "yes" } else { "no" }
}

/// A nonempty relation section: heading with the recorded count, then every
/// entry in recorded order and multiplicity, escaped and indented two spaces.
fn relation_section(heading: &str, entries: &[String]) -> Vec<String> {
    if entries.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![
        String::new(),
        heading_line(&format!("{heading} ({}):", entries.len())),
    ];
    lines.extend(entries.iter().map(|entry| format!("  {}", visible(entry))));
    lines
}

fn component_section(components: &[Component]) -> Vec<String> {
    if components.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![
        String::new(),
        heading_line(&format!("Components ({}):", components.len())),
    ];
    for component in components {
        lines.push(field_line(
            "Component",
            &format!(":{}", visible(&component.name)),
        ));
        lines.push(field_line("Installed", yes_no(component.is_installed)));
    }
    lines
}

fn detail_lines(info: &InstalledPackageInfo<'_>) -> Vec<String> {
    let trove = info.trove;
    let release = trove
        .package_release
        .as_deref()
        .map(visible)
        .unwrap_or_else(|| "-".to_owned());
    let mut lines = vec![
        heading_line("Installed package:"),
        field_line("Name", &visible(&trove.name)),
        field_line("Version", &visible(&trove.version)),
        field_line("CCS release", &release),
        field_line("Type", &visible(trove.trove_type.as_str())),
        field_line("Authority", &visible(info.authority)),
        field_line("Install source", &visible(trove.install_source.as_str())),
    ];
    if let Some(profile) = &trove.source_profile {
        lines.push(field_line("Source profile", &visible(profile)));
    }
    lines.push(field_line(
        "Version scheme",
        &visible(trove.version_scheme.as_str()),
    ));
    if let Some(repository) = info.repository {
        lines.push(field_line("Repository", &visible(repository)));
    }
    if let Some(architecture) = &trove.architecture {
        lines.push(field_line("Architecture", &visible(architecture)));
    }
    if let Some(description) = &trove.description {
        lines.push(field_line("Description", &visible(description)));
    }
    if let Some(installed) = &trove.installed_at {
        lines.push(field_line("Installed", &visible(installed)));
    }
    if let Some(reason) = &trove.selection_reason {
        lines.push(field_line("Selection reason", &visible(reason)));
    }
    lines.push(field_line(
        "Install reason",
        &visible(trove.install_reason.as_str()),
    ));
    lines.push(field_line("Pinned", yes_no(trove.pinned)));
    lines.push(field_line("File records", &info.file_records.to_string()));
    lines.push(field_line(
        "Payload size",
        &format!("{} bytes", info.payload_bytes),
    ));

    let dependencies: Vec<String> = info
        .dependencies
        .iter()
        .map(InstalledRequirementAtom::to_typed_string)
        .collect();
    lines.extend(relation_section("Dependencies", &dependencies));
    let provides: Vec<String> = info
        .provides
        .iter()
        .map(ProvideEntry::to_typed_string)
        .collect();
    lines.extend(relation_section("Provides", &provides));
    lines.extend(component_section(info.components));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use conary_core::db::models::TroveType;
    use conary_core::repository::dependency_model::RepositoryCapabilityKind;
    use conary_core::repository::versioning::VersionScheme;

    fn plain() {
        console::set_colors_enabled(false);
    }

    fn trove(name: &str, version: &str) -> Trove {
        Trove::new(
            name.to_owned(),
            version.to_owned(),
            TroveType::Package,
            VersionScheme::Debian,
        )
    }

    fn info<'a>(
        trove: &'a Trove,
        dependencies: &'a [InstalledRequirementAtom],
        provides: &'a [ProvideEntry],
        components: &'a [Component],
    ) -> InstalledPackageInfo<'a> {
        InstalledPackageInfo {
            trove,
            authority: "debian@bookworm",
            repository: None,
            file_records: 0,
            payload_bytes: 0,
            dependencies,
            provides,
            components,
        }
    }

    #[test]
    fn identity_keeps_version_and_release_separate() {
        plain();
        let mut trove = trove("nginx", "1.24.0-1");
        trove.package_release = Some("7".to_owned());
        let lines = detail_lines(&info(&trove, &[], &[], &[]));
        assert_eq!(
            lines[..4],
            [
                "Installed package:",
                "  Name: nginx",
                "  Version: 1.24.0-1",
                "  CCS release: 7",
            ]
        );
    }

    #[test]
    fn missing_observations_are_omitted_and_release_stays_absent() {
        plain();
        let mut trove = trove("nginx", "1.24.0");
        trove.selection_reason = None;
        let lines = detail_lines(&info(&trove, &[], &[], &[]));
        assert!(lines.contains(&"  CCS release: -".to_owned()));
        for label in [
            "Source profile",
            "Repository",
            "Architecture",
            "Description",
            "Selection reason",
            "Dependencies",
            "Provides",
            "Components",
        ] {
            assert!(
                !lines.iter().any(|line| line.contains(label)),
                "unexpected {label} in {lines:?}"
            );
        }
        assert!(!lines.iter().any(|line| line.starts_with("  Installed:")));
    }

    #[test]
    fn dynamic_control_characters_are_escaped() {
        plain();
        let mut trove = trove("bad\u{7}name", "1.0");
        trove.description = Some("line\nbreak".to_owned());
        let lines = detail_lines(&info(&trove, &[], &[], &[]));
        assert_eq!(lines[1], "  Name: bad\\u{7}name");
        assert!(
            lines.contains(&"  Description: line\\nbreak".to_owned()),
            "{lines:?}"
        );
        assert!(!lines.iter().any(|line| line.contains('\n')));
    }

    #[test]
    fn typed_relations_and_components_retain_order_and_multiplicity() {
        plain();
        let trove = trove("nginx", "1.24.0");
        let dependencies = vec![
            InstalledRequirementAtom {
                id: None,
                trove_id: 1,
                depends_on_name: "libc6".to_owned(),
                depends_on_version: Some(">= 2.36".to_owned()),
                dependency_type: "runtime".to_owned(),
                version_constraint: Some(">= 2.36".to_owned()),
                kind: "package".to_owned(),
                group_id: None,
            },
            InstalledRequirementAtom {
                id: None,
                trove_id: 1,
                depends_on_name: "libc6".to_owned(),
                depends_on_version: Some(">= 2.36".to_owned()),
                dependency_type: "runtime".to_owned(),
                version_constraint: Some(">= 2.36".to_owned()),
                kind: "package".to_owned(),
                group_id: None,
            },
        ];
        let mut virtual_provide =
            ProvideEntry::new(1, "httpd".to_owned(), None, VersionScheme::Conary);
        virtual_provide.kind = RepositoryCapabilityKind::Virtual;
        let provides = vec![
            ProvideEntry::new(1, "nginx".to_owned(), None, VersionScheme::Conary),
            virtual_provide,
        ];
        let components = vec![
            Component::new(1, "lib".to_owned()),
            Component {
                is_installed: false,
                ..Component::new(1, "doc".to_owned())
            },
        ];
        let lines = detail_lines(&info(&trove, &dependencies, &provides, &components));
        assert!(lines.contains(&"Dependencies (2):".to_owned()));
        assert!(lines.contains(&"Provides (2):".to_owned()));
        assert!(lines.contains(&"Components (2):".to_owned()));
        let section = |heading: &str| {
            let start = lines.iter().position(|line| line == heading).unwrap();
            lines[start + 1..].to_vec()
        };
        assert_eq!(
            section("Dependencies (2):")[..2],
            ["  libc6>= 2.36", "  libc6>= 2.36"]
        );
        assert_eq!(
            section("Provides (2):")[..2],
            ["  nginx", "  virtual(httpd)"]
        );
        assert_eq!(
            section("Components (2):")[..4],
            [
                "  Component: :lib",
                "  Installed: yes",
                "  Component: :doc",
                "  Installed: no",
            ]
        );
    }
}
