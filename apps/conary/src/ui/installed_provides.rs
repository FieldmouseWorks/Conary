// apps/conary/src/ui/installed_provides.rs
//! Installed-provides records for the recorded capability observation.
//!
//! Rendering reports exactly the typed records a command adapter gathered, in
//! their recorded order and multiplicity. It performs no parsing, validation,
//! inference, sorting, or deduplication, and never invents a default version,
//! relation, architecture, or provenance. Every dynamic field is escaped, so a
//! control character cannot forge a field or record boundary.

use super::transaction_summary::{source_format_label, visible};
use super::{field_line, heading_line};
use conary_core::db::models::ProvideEntry;
use conary_core::repository::dependency_model::{
    CapabilityProvenance, ProvideArchitectureQualifier, ProvideVersionRelation,
    RepositoryCapabilityKind, SourcePackageFormat,
};

/// One nonempty section: a leading blank line, the shared heading with the
/// recorded count, then every record separated by a blank line. Empty input
/// contributes no lines at all.
pub(super) fn section(provides: &[ProvideEntry]) -> Vec<String> {
    if provides.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![
        String::new(),
        heading_line(&format!("Provides ({}):", provides.len())),
    ];
    for (index, provide) in provides.iter().enumerate() {
        if index > 0 {
            lines.push(String::new());
        }
        lines.extend(record_lines(provide));
    }
    lines
}

fn record_lines(provide: &ProvideEntry) -> Vec<String> {
    let mut lines = vec![
        field_line("Capability", &visible(&provide.capability)),
        field_line("Kind", capability_kind_label(provide.kind)),
    ];
    lines.push(field_line(
        "Version",
        &provide
            .version
            .as_deref()
            .map(visible)
            .unwrap_or_else(|| "-".to_owned()),
    ));
    lines.push(field_line(
        "Version relation",
        version_relation_label(provide.version_relation),
    ));
    lines.push(field_line(
        "Version scheme",
        provide.version_scheme.as_str(),
    ));
    lines.push(field_line(
        "Architecture qualifier",
        architecture_qualifier_label(&provide.architecture_qualifier),
    ));
    if let ProvideArchitectureQualifier::Exact(architecture) = &provide.architecture_qualifier {
        lines.push(field_line("Architecture", &visible(architecture)));
    }
    lines.extend(provenance_lines(&provide.provenance));
    lines
}

/// Every provenance variant renders its exact role label; source-derived
/// provenance also reports the format that recorded it, and a source-declared
/// capability keeps its recorded record index verbatim.
fn provenance_lines(provenance: &CapabilityProvenance) -> Vec<String> {
    match provenance {
        CapabilityProvenance::ExactIdentity => vec![field_line("Provenance", "exact-identity")],
        CapabilityProvenance::AuthorDeclared => vec![field_line("Provenance", "author-declared")],
        CapabilityProvenance::SourceDeclared {
            format,
            record_index,
        } => vec![
            field_line("Provenance", "source-declared"),
            source_line(*format),
            field_line("Source record index", &visible(&record_index.to_string())),
        ],
        CapabilityProvenance::SourceDerivedFile { format } => vec![
            field_line("Provenance", "source-derived-file"),
            source_line(*format),
        ],
        CapabilityProvenance::SourcePromisedPath { format } => vec![
            field_line("Provenance", "source-promised-path"),
            source_line(*format),
        ],
    }
}

fn source_line(format: SourcePackageFormat) -> String {
    field_line(
        "Source format",
        &visible(&source_format_label(Some(format))),
    )
}

fn capability_kind_label(kind: RepositoryCapabilityKind) -> &'static str {
    match kind {
        RepositoryCapabilityKind::PackageName => "package",
        RepositoryCapabilityKind::Virtual => "virtual",
        RepositoryCapabilityKind::Soname => "soname",
        RepositoryCapabilityKind::File => "file",
        RepositoryCapabilityKind::Path => "path",
        RepositoryCapabilityKind::Binary => "binary",
        RepositoryCapabilityKind::PkgConfig => "pkgconfig",
        RepositoryCapabilityKind::PkgConfig32 => "pkgconfig32",
        RepositoryCapabilityKind::Comar => "comar",
        RepositoryCapabilityKind::Generic => "generic",
    }
}

fn version_relation_label(relation: Option<ProvideVersionRelation>) -> &'static str {
    match relation {
        Some(ProvideVersionRelation::LessThan) => "<",
        Some(ProvideVersionRelation::LessOrEqual) => "<=",
        Some(ProvideVersionRelation::Equal) => "=",
        Some(ProvideVersionRelation::GreaterOrEqual) => ">=",
        Some(ProvideVersionRelation::GreaterThan) => ">",
        None => "-",
    }
}

fn architecture_qualifier_label(qualifier: &ProvideArchitectureQualifier) -> &'static str {
    match qualifier {
        ProvideArchitectureQualifier::Implicit => "implicit",
        ProvideArchitectureQualifier::Any => "any",
        ProvideArchitectureQualifier::Exact(_) => "exact",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conary_core::repository::dependency_model::{
        CapabilityProvenance, ProvideArchitectureQualifier, ProvideVersionRelation,
        RepositoryCapabilityKind, SourcePackageFormat,
    };
    use conary_core::repository::versioning::VersionScheme;

    fn plain() {
        console::set_colors_enabled(false);
    }

    fn entry(capability: &str, kind: RepositoryCapabilityKind) -> ProvideEntry {
        ProvideEntry {
            id: None,
            trove_id: 1,
            capability: capability.to_owned(),
            version: None,
            version_relation: None,
            kind,
            version_scheme: VersionScheme::Rpm,
            architecture_qualifier: ProvideArchitectureQualifier::Implicit,
            provenance: CapabilityProvenance::AuthorDeclared,
        }
    }

    fn fields(lines: &[String], label: &str) -> Vec<String> {
        let prefix = format!("  {label}: ");
        lines
            .iter()
            .filter(|line| line.starts_with(&prefix))
            .map(|line| line[prefix.len()..].to_owned())
            .collect()
    }

    #[test]
    fn empty_input_renders_nothing() {
        plain();
        assert!(section(&[]).is_empty());
    }

    #[test]
    fn records_keep_order_and_duplicates_with_blank_separators() {
        plain();
        let provides = [
            entry("zlib", RepositoryCapabilityKind::PackageName),
            entry("libz.so.1", RepositoryCapabilityKind::Soname),
            entry("zlib", RepositoryCapabilityKind::PackageName),
        ];
        let lines = section(&provides);
        assert_eq!(lines[0], "");
        assert_eq!(lines[1], "Provides (3):");
        assert_eq!(fields(&lines, "Capability"), ["zlib", "libz.so.1", "zlib"]);
        assert_eq!(fields(&lines, "Kind"), ["package", "soname", "package"]);
        assert_eq!(lines.iter().filter(|line| line.is_empty()).count(), 3);
        assert!(!lines.last().unwrap().is_empty());
    }

    #[test]
    fn one_record_matches_the_full_literal_frame() {
        plain();
        let lines = section(&[entry("virtual-abi", RepositoryCapabilityKind::Virtual)]);
        assert_eq!(
            lines,
            [
                "",
                "Provides (1):",
                "  Capability: virtual-abi",
                "  Kind: virtual",
                "  Version: -",
                "  Version relation: -",
                "  Version scheme: rpm",
                "  Architecture qualifier: implicit",
                "  Provenance: author-declared",
            ]
        );
    }

    #[test]
    fn absent_version_and_relation_stay_absent() {
        plain();
        let mut provide = entry("existence-only", RepositoryCapabilityKind::PackageName);
        provide.version_scheme = VersionScheme::Debian;
        let lines = section(&[provide]);
        assert_eq!(fields(&lines, "Version"), ["-"]);
        assert_eq!(fields(&lines, "Version relation"), ["-"]);
        assert_eq!(fields(&lines, "Version scheme"), ["debian"]);
    }

    #[test]
    fn control_characters_are_escaped_in_dynamic_fields() {
        plain();
        let mut provide = entry("lib\tx\n7", RepositoryCapabilityKind::Generic);
        provide.version = Some("1\u{1}2".to_owned());
        provide.version_relation = Some(ProvideVersionRelation::Equal);
        provide.architecture_qualifier = ProvideArchitectureQualifier::Exact("x86\t64".to_owned());
        let lines = section(&[provide]);
        assert_eq!(fields(&lines, "Capability"), ["lib\\tx\\n7"]);
        assert_eq!(fields(&lines, "Version"), ["1\\u{1}2"]);
        assert_eq!(fields(&lines, "Architecture"), ["x86\\t64"]);
    }

    #[test]
    fn architecture_qualifier_distinguishes_implicit_any_and_exact() {
        plain();
        let mut exact = entry("native-pkg", RepositoryCapabilityKind::PackageName);
        exact.architecture_qualifier = ProvideArchitectureQualifier::Exact("native".to_owned());
        let mut any = entry("wildcard", RepositoryCapabilityKind::Virtual);
        any.architecture_qualifier = ProvideArchitectureQualifier::Any;
        let lines = section(&[
            entry("implicit-pkg", RepositoryCapabilityKind::PackageName),
            any,
            exact,
        ]);
        assert_eq!(
            fields(&lines, "Architecture qualifier"),
            ["implicit", "any", "exact"]
        );
        assert_eq!(fields(&lines, "Architecture"), ["native"]);
    }

    #[test]
    fn source_provenance_carries_format_and_record_index() {
        plain();
        let mut declared = entry("declared", RepositoryCapabilityKind::Virtual);
        declared.version_scheme = VersionScheme::Debian;
        declared.provenance = CapabilityProvenance::SourceDeclared {
            format: SourcePackageFormat::Debian,
            record_index: 7,
        };
        let mut derived = entry("/usr/lib/x.so", RepositoryCapabilityKind::File);
        derived.version_scheme = VersionScheme::Arch;
        derived.provenance = CapabilityProvenance::SourceDerivedFile {
            format: SourcePackageFormat::Alpm,
        };
        let mut promised = entry("/usr/share/empty", RepositoryCapabilityKind::File);
        promised.version_scheme = VersionScheme::Eopkg;
        promised.provenance = CapabilityProvenance::SourcePromisedPath {
            format: SourcePackageFormat::Eopkg,
        };
        let mut authored = entry("authored", RepositoryCapabilityKind::Generic);
        authored.provenance = CapabilityProvenance::AuthorDeclared;
        let lines = section(&[declared, derived, promised, authored]);
        assert_eq!(
            fields(&lines, "Provenance"),
            [
                "source-declared",
                "source-derived-file",
                "source-promised-path",
                "author-declared",
            ]
        );
        assert_eq!(fields(&lines, "Source format"), ["deb", "arch", "eopkg"]);
        assert_eq!(fields(&lines, "Source record index"), ["7"]);
    }

    #[test]
    fn source_record_index_is_not_reindexed_by_position() {
        plain();
        let mut later = entry("second", RepositoryCapabilityKind::Virtual);
        later.provenance = CapabilityProvenance::SourceDeclared {
            format: SourcePackageFormat::Rpm,
            record_index: 41,
        };
        let lines = section(&[entry("first", RepositoryCapabilityKind::Virtual), later]);
        assert_eq!(fields(&lines, "Source record index"), ["41"]);
        assert_eq!(fields(&lines, "Source format"), ["rpm"]);
        assert_eq!(
            fields(&lines, "Provenance"),
            ["author-declared", "source-declared"]
        );
    }

    #[test]
    fn typed_relations_render_distinct_operators() {
        plain();
        let mut provides = Vec::new();
        for relation in [
            ProvideVersionRelation::LessThan,
            ProvideVersionRelation::LessOrEqual,
            ProvideVersionRelation::Equal,
            ProvideVersionRelation::GreaterOrEqual,
            ProvideVersionRelation::GreaterThan,
        ] {
            let mut provide = entry("bound", RepositoryCapabilityKind::PackageName);
            provide.version = Some("2".to_owned());
            provide.version_relation = Some(relation);
            provides.push(provide);
        }
        let lines = section(&provides);
        assert_eq!(
            fields(&lines, "Version relation"),
            ["<", "<=", "=", ">=", ">"]
        );
        assert_eq!(fields(&lines, "Version"), ["2", "2", "2", "2", "2"]);
    }
}
