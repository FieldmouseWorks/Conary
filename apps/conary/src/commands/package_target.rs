// apps/conary/src/commands/package_target.rs

//! Shared installed-package selector and rendering helpers.

use anyhow::Result;
use conary_core::db::models::{InstallSource, Trove, TroveType};
mod release;
pub use release::InstalledRelease;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstalledPackageSelector {
    pub(crate) name: String,
    pub(crate) version: Option<String>,
    pub(crate) architecture: Option<String>,
    pub(crate) release: Option<InstalledRelease>,
}

impl InstalledPackageSelector {
    pub(crate) fn new(name: String, version: Option<String>, architecture: Option<String>) -> Self {
        Self {
            name,
            version,
            architecture,
            release: None,
        }
    }

    pub(crate) fn with_release(mut self, release: Option<InstalledRelease>) -> Self {
        self.release = release;
        self
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedInstalledPackage {
    pub(crate) trove: Trove,
    pub(crate) trove_id: i64,
}

pub(crate) fn resolve_installed_package(
    conn: &rusqlite::Connection,
    selector: &InstalledPackageSelector,
) -> Result<ResolvedInstalledPackage> {
    let troves = Trove::find_by_name(conn, &selector.name)?
        .into_iter()
        .filter(|trove| trove.trove_type == TroveType::Package)
        .collect::<Vec<_>>();

    if troves.is_empty() {
        anyhow::bail!("Package '{}' is not installed", selector.name);
    }

    let matches = matching_installed_packages(&troves, selector);
    match matches.as_slice() {
        [] => anyhow::bail!(
            "Package '{}' with selector version={:?} release={} architecture={:?} is not installed. Installed variants: {}. Use --version, --release, and/or --arch to choose one.",
            selector.name,
            selector.version,
            format_selector_release(selector.release.as_ref()),
            selector.architecture,
            format_installed_variants(&troves)
        ),
        [trove] => {
            let trove_id = trove
                .id
                .ok_or_else(|| anyhow::anyhow!("Package '{}' has no database ID", selector.name))?;
            Ok(ResolvedInstalledPackage {
                trove: (*trove).clone(),
                trove_id,
            })
        }
        _ => {
            let variants = matches
                .iter()
                .map(|trove| format!("  - {}", format_installed_variant(trove)))
                .collect::<Vec<_>>()
                .join("\n");
            anyhow::bail!(
                "Multiple installed variants of '{}' match the selector:\n{}\nUse --version, --release, and/or --arch to choose one.",
                selector.name,
                variants
            )
        }
    }
}

fn matching_installed_packages<'a>(
    troves: &'a [Trove],
    selector: &InstalledPackageSelector,
) -> Vec<&'a Trove> {
    troves
        .iter()
        .filter(|trove| {
            selector
                .version
                .as_deref()
                .is_none_or(|version| trove.version == version)
                && release_matches(selector.release.as_ref(), trove.package_release.as_deref())
                && architecture_matches(
                    selector.architecture.as_deref(),
                    trove.architecture.as_deref(),
                )
        })
        .collect()
}

fn release_matches(selector: Option<&InstalledRelease>, actual: Option<&str>) -> bool {
    match selector {
        None => true,
        Some(InstalledRelease::Unspecified) => actual.is_none(),
        Some(InstalledRelease::Exact(release)) => actual == Some(release.as_str()),
    }
}

fn architecture_matches(selector: Option<&str>, actual: Option<&str>) -> bool {
    match selector {
        None => true,
        Some("none" | "unspecified") => actual.is_none(),
        Some(arch) => actual == Some(arch),
    }
}

fn format_installed_variant(trove: &Trove) -> String {
    format!(
        "version {} [{}] (release {}, {}, {})",
        trove.version,
        trove.architecture.as_deref().unwrap_or("none"),
        trove.package_release.as_deref().unwrap_or("none"),
        package_authority_label(trove.install_source.clone()),
        trove.version_scheme.as_str()
    )
}

fn format_selector_release(release: Option<&InstalledRelease>) -> &str {
    match release {
        None => "*",
        Some(InstalledRelease::Unspecified) => "none",
        Some(InstalledRelease::Exact(release)) => release.as_str(),
    }
}

fn format_installed_variants(troves: &[Trove]) -> String {
    troves
        .iter()
        .map(format_installed_variant)
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn package_authority_label(source: InstallSource) -> &'static str {
    if source.is_adopted() {
        "native-authority"
    } else {
        "conary-owned"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conary_core::db::models::{InstallSource, Trove, TroveType};

    fn db_with_variants() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conary_core::db::schema::ensure_current(&conn).unwrap();

        let mut x86 = Trove::new_with_source(
            "demo".to_string(),
            "1.0.0".to_string(),
            TroveType::Package,
            InstallSource::Repository,
            conary_core::repository::versioning::VersionScheme::Conary,
        );
        x86.architecture = Some("x86_64".to_string());
        x86.insert(&conn).unwrap();

        let mut arm = Trove::new_with_source(
            "demo".to_string(),
            "1.0.0".to_string(),
            TroveType::Package,
            InstallSource::Repository,
            conary_core::repository::versioning::VersionScheme::Conary,
        );
        arm.architecture = Some("aarch64".to_string());
        arm.insert(&conn).unwrap();

        conn
    }

    fn db_with_release_variants() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conary_core::db::schema::ensure_current(&conn).unwrap();

        for release in [Some("1"), Some("2"), None] {
            let mut trove = Trove::new_with_source(
                "demo".to_string(),
                "1.0.0".to_string(),
                TroveType::Package,
                InstallSource::Repository,
                conary_core::repository::versioning::VersionScheme::Conary,
            );
            trove.architecture = Some("x86_64".to_string());
            trove.package_release = release.map(str::to_string);
            trove.insert(&conn).unwrap();
        }

        conn
    }

    #[test]
    fn installed_release_parses_literal_none_as_unspecified() {
        assert_eq!(
            "none".parse::<InstalledRelease>().unwrap(),
            InstalledRelease::Unspecified
        );
    }

    #[test]
    fn installed_release_retains_exact_digits_without_normalizing() {
        assert_eq!(
            "007".parse::<InstalledRelease>().unwrap(),
            InstalledRelease::Exact("007".to_string())
        );
    }

    #[test]
    fn installed_release_rejects_invalid_values() {
        for raw in [
            "",
            "0",
            "00",
            "-1",
            "+1",
            "1.0",
            "abc",
            " 1",
            "18446744073709551616",
        ] {
            assert!(
                raw.parse::<InstalledRelease>().is_err(),
                "release {raw:?} should be rejected"
            );
        }
    }

    #[test]
    fn selector_new_leaves_release_unfiltered() {
        let selector = InstalledPackageSelector::new("demo".to_string(), None, None);

        assert!(selector.release.is_none());
    }

    #[test]
    fn selector_resolves_exact_release() {
        let conn = db_with_release_variants();
        let selector = InstalledPackageSelector::new(
            "demo".to_string(),
            Some("1.0.0".to_string()),
            Some("x86_64".to_string()),
        )
        .with_release(Some("2".parse().unwrap()));

        let resolved = resolve_installed_package(&conn, &selector).unwrap();

        assert_eq!(resolved.trove.package_release.as_deref(), Some("2"));
    }

    #[test]
    fn selector_resolves_unspecified_release() {
        let conn = db_with_release_variants();
        let selector = InstalledPackageSelector::new("demo".to_string(), None, None)
            .with_release(Some(InstalledRelease::Unspecified));

        let resolved = resolve_installed_package(&conn, &selector).unwrap();

        assert_eq!(resolved.trove.package_release, None);
    }

    #[test]
    fn selector_without_release_filter_refuses_release_ambiguity() {
        let conn = db_with_release_variants();
        let selector = InstalledPackageSelector::new(
            "demo".to_string(),
            Some("1.0.0".to_string()),
            Some("x86_64".to_string()),
        );

        let err = resolve_installed_package(&conn, &selector)
            .unwrap_err()
            .to_string();

        assert!(err.contains("Multiple installed variants of 'demo' match"));
        assert!(err.contains("release 1"));
        assert!(err.contains("release 2"));
        assert!(err.contains("release none"));
        assert!(err.contains("--version, --release, and/or --arch"));
    }

    #[test]
    fn selector_reports_no_match_for_unknown_release() {
        let conn = db_with_release_variants();
        let selector = InstalledPackageSelector::new("demo".to_string(), None, None)
            .with_release(Some(InstalledRelease::Exact("3".to_string())));

        let err = resolve_installed_package(&conn, &selector)
            .unwrap_err()
            .to_string();

        assert!(err.contains("Package 'demo' with selector"));
        assert!(err.contains("release=3"));
        assert!(err.contains("Installed variants:"));
        assert!(err.contains("release none"));
        assert!(err.contains("--release"));
    }

    #[test]
    fn selector_with_release_none_keeps_no_release_filter() {
        let conn = db_with_release_variants();
        let selector =
            InstalledPackageSelector::new("demo".to_string(), None, None).with_release(None);

        let err = resolve_installed_package(&conn, &selector)
            .unwrap_err()
            .to_string();

        assert!(err.contains("Multiple installed variants of 'demo' match"));
    }

    #[test]
    fn selector_refuses_ambiguous_package_without_variant_fields() {
        let conn = db_with_variants();
        let selector = InstalledPackageSelector::new("demo".to_string(), None, None);

        let err = resolve_installed_package(&conn, &selector)
            .unwrap_err()
            .to_string();

        assert!(err.contains("Multiple installed variants of 'demo' match"));
        assert!(err.contains("version 1.0.0 [x86_64]"));
        assert!(err.contains("version 1.0.0 [aarch64]"));
        assert!(err.contains("--version"));
        assert!(err.contains("--arch"));
    }

    #[test]
    fn selector_resolves_version_and_architecture() {
        let conn = db_with_variants();
        let selector = InstalledPackageSelector::new(
            "demo".to_string(),
            Some("1.0.0".to_string()),
            Some("aarch64".to_string()),
        );

        let resolved = resolve_installed_package(&conn, &selector).unwrap();

        assert_eq!(resolved.trove.name, "demo");
        assert_eq!(resolved.trove.version, "1.0.0");
        assert_eq!(resolved.trove.architecture.as_deref(), Some("aarch64"));
    }

    #[test]
    fn selector_reports_available_variants_when_filter_matches_none() {
        let conn = db_with_variants();
        let selector = InstalledPackageSelector::new(
            "demo".to_string(),
            Some("2.0.0".to_string()),
            Some("x86_64".to_string()),
        );

        let err = resolve_installed_package(&conn, &selector)
            .unwrap_err()
            .to_string();

        assert!(err.contains("Package 'demo' with selector"));
        assert!(err.contains("Installed variants:"));
        assert!(err.contains("1.0.0 [x86_64]"));
    }

    #[test]
    fn selector_ignores_non_package_troves() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conary_core::db::schema::ensure_current(&conn).unwrap();

        let mut component = Trove::new(
            "demo".to_string(),
            "1.0.0".to_string(),
            TroveType::Component,
            conary_core::repository::versioning::VersionScheme::Conary,
        );
        component.architecture = Some("aarch64".to_string());
        component.insert(&conn).unwrap();

        let mut package = Trove::new(
            "demo".to_string(),
            "1.0.0".to_string(),
            TroveType::Package,
            conary_core::repository::versioning::VersionScheme::Conary,
        );
        package.architecture = Some("x86_64".to_string());
        package.insert(&conn).unwrap();

        let selector = InstalledPackageSelector::new("demo".to_string(), None, None);
        let resolved = resolve_installed_package(&conn, &selector).unwrap();

        assert_eq!(resolved.trove.trove_type, TroveType::Package);
    }

    #[test]
    fn selector_can_target_unspecified_architecture() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conary_core::db::schema::ensure_current(&conn).unwrap();

        let mut unspecified = Trove::new(
            "demo".to_string(),
            "1.0.0".to_string(),
            TroveType::Package,
            conary_core::repository::versioning::VersionScheme::Conary,
        );
        unspecified.insert(&conn).unwrap();

        let mut x86 = Trove::new(
            "demo".to_string(),
            "1.0.0".to_string(),
            TroveType::Package,
            conary_core::repository::versioning::VersionScheme::Conary,
        );
        x86.architecture = Some("x86_64".to_string());
        x86.insert(&conn).unwrap();

        let selector = InstalledPackageSelector::new(
            "demo".to_string(),
            Some("1.0.0".to_string()),
            Some("none".to_string()),
        );
        let resolved = resolve_installed_package(&conn, &selector).unwrap();

        assert_eq!(resolved.trove.architecture, None);
    }
}
