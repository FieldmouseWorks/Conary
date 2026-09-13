// apps/conary/src/ui/ccs_build.rs
//! CCS build-result presentation, driven by the typed build result and
//! conversion-loss report.
//!
//! Rendering reports what the builder recorded. It never inspects the
//! filesystem, revalidates or reclassifies payload sources, and never claims
//! an archive size or on-disk delta from payload counts.

use super::transaction_summary::visible;
use super::{Status, field_line, heading_line, row_line};
use conary_core::ccs::builder::{BuildResult, ChunkStats, ComponentData};
use conary_core::ccs::native_export::LossReport;
use std::collections::HashMap;

/// Two-space-indented fixed-width table with an unpadded final column.
fn table_lines(headings: &[&str], rows: &[Vec<String>]) -> Vec<String> {
    let widths: Vec<usize> = (0..headings.len())
        .map(|column| {
            rows.iter()
                .map(|row| console::measure_text_width(&row[column]))
                .chain(std::iter::once(console::measure_text_width(
                    headings[column],
                )))
                .max()
                .unwrap_or(0)
        })
        .collect();
    let render = |cells: &[&str]| {
        let padded: Vec<String> = cells
            .iter()
            .enumerate()
            .map(|(column, cell)| {
                if column + 1 == cells.len() {
                    (*cell).to_owned()
                } else {
                    format!(
                        "{cell}{}",
                        " ".repeat(widths[column] - console::measure_text_width(cell))
                    )
                }
            })
            .collect();
        format!("  {}", padded.join("  "))
    };
    let mut lines = vec![render(headings)];
    lines.extend(rows.iter().map(|row| {
        let cells: Vec<&str> = row.iter().map(String::as_str).collect();
        render(&cells)
    }));
    lines
}

/// Exact manifest identity fields. The version is authoritative text and is
/// never re-prefixed; the CCS release stays its own field.
fn identity_lines(
    name: &str,
    version: &str,
    release: &str,
    architecture: Option<&str>,
) -> Vec<String> {
    vec![
        field_line("Package", &visible(name)),
        field_line("Version", &visible(version)),
        field_line("CCS release", &visible(release)),
        field_line(
            "Architecture",
            &architecture.map(visible).unwrap_or_else(|| "-".into()),
        ),
    ]
}

fn payload_source_count(result: &BuildResult) -> usize {
    result
        .payloads
        .iter()
        .filter(|payload| payload.node.kind.is_regular())
        .count()
}

fn record_lines(result: &BuildResult) -> Vec<String> {
    vec![
        field_line("File records", &result.files.len().to_string()),
        field_line("Payload size", &format!("{} bytes", result.total_size)),
        field_line(
            "Payload sources",
            &format!("{} regular files", payload_source_count(result)),
        ),
    ]
}

fn chunking_lines(stats: &ChunkStats) -> Vec<String> {
    let mut lines = vec![
        heading_line("Chunking:"),
        field_line("Chunked files", &stats.chunked_files.to_string()),
        field_line("Whole files", &stats.whole_files.to_string()),
        field_line("Total chunks", &stats.total_chunks.to_string()),
        field_line("Unique chunks", &stats.unique_chunks.to_string()),
    ];
    if stats.dedup_savings > 0 {
        lines.push(field_line(
            "Intra-package deduplication",
            &format!("{} bytes saved", stats.dedup_savings),
        ));
    }
    lines
}

fn component_table_lines(components: &HashMap<String, ComponentData>) -> Vec<String> {
    let mut entries: Vec<(&String, &ComponentData)> = components.iter().collect();
    entries.sort_by(|left, right| left.0.cmp(right.0));
    let rows: Vec<Vec<String>> = entries
        .iter()
        .map(|(name, component)| {
            vec![
                visible(name),
                component.files.len().to_string(),
                format!("{} bytes", component.size),
            ]
        })
        .collect();
    table_lines(&["Component", "File records", "Payload size"], &rows)
}

fn component_section_lines(components: &HashMap<String, ComponentData>) -> Vec<String> {
    let mut lines = vec![heading_line("Components:")];
    if components.is_empty() {
        lines.push("  No components.".to_owned());
        return lines;
    }
    lines.extend(component_table_lines(components));
    lines
}

fn summary_lines(result: &BuildResult) -> Vec<String> {
    let package = &result.manifest.package;
    let mut lines = vec![String::new(), heading_line("Package build summary:")];
    lines.extend(identity_lines(
        &package.name,
        &package.version,
        &package.release,
        package
            .platform
            .as_ref()
            .and_then(|platform| platform.arch.as_deref()),
    ));
    lines.extend(record_lines(result));
    if let Some(stats) = &result.chunk_stats {
        lines.push(String::new());
        lines.extend(chunking_lines(stats));
    }
    lines.push(String::new());
    lines.extend(component_section_lines(&result.components));
    lines
}

fn loss_lines(report: &LossReport, format_name: &str) -> Vec<String> {
    if report.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![heading_line(&format!(
        "Conversion notes for {}:",
        visible(format_name)
    ))];
    lines.extend(
        report
            .unsupported_features
            .iter()
            .map(|note| row_line(Status::Warn, &["Unsupported", &visible(note)])),
    );
    lines.extend(
        report
            .hook_notes
            .iter()
            .map(|note| row_line(Status::Info, &["Hook", &visible(note)])),
    );
    lines.extend(
        report
            .dependency_notes
            .iter()
            .map(|note| row_line(Status::Info, &["Dependency", &visible(note)])),
    );
    lines
}

/// Report the recorded build result for an authored CCS package.
pub(crate) fn print_build_summary(result: &BuildResult) {
    super::message(&summary_lines(result).join("\n"));
}

/// Report conversion losses for one native export format, when any exist.
pub(crate) fn print_loss_report(report: &LossReport, format_name: &str) {
    let lines = loss_lines(report, format_name);
    if !lines.is_empty() {
        super::message(&lines.join("\n"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() {
        console::set_colors_enabled(false);
    }

    fn component(name: &str, size: u64) -> ComponentData {
        ComponentData {
            name: name.to_owned(),
            files: Vec::new(),
            hash: String::new(),
            size,
        }
    }

    #[test]
    fn identity_keeps_version_and_release_separate() {
        plain();
        let lines = identity_lines("nginx", "1.24.0", "7", Some("x86_64"));
        assert_eq!(
            lines,
            vec![
                "  Package: nginx",
                "  Version: 1.24.0",
                "  CCS release: 7",
                "  Architecture: x86_64",
            ]
        );
        assert!(!lines.iter().any(|line| line.contains("v1.24.0")));
    }

    #[test]
    fn identity_reports_absent_architecture_as_dash() {
        plain();
        let lines = identity_lines("nginx", "1.24.0", "7", None);
        assert_eq!(lines[3], "  Architecture: -");
    }

    #[test]
    fn components_render_sorted_under_shared_table_columns() {
        plain();
        let components = HashMap::from([
            ("zlib".to_owned(), component("zlib", 128)),
            ("core".to_owned(), component("core", 4096)),
            ("docs".to_owned(), component("docs", 16)),
        ]);
        let lines = component_section_lines(&components);
        assert_eq!(lines[0], "Components:");
        assert_eq!(lines[1], "  Component  File records  Payload size");
        let names: Vec<&str> = lines[2..]
            .iter()
            .map(|line| line.trim_start().split("  ").next().unwrap())
            .collect();
        assert_eq!(names, vec!["core", "docs", "zlib"]);
        assert!(lines[2].ends_with("4096 bytes"));
    }

    #[test]
    fn absent_components_render_a_stable_placeholder() {
        plain();
        let components: HashMap<String, ComponentData> = HashMap::new();
        assert_eq!(
            component_section_lines(&components),
            vec!["Components:", "  No components."]
        );
    }

    #[test]
    fn dynamic_text_is_escaped_rather_than_interpolated() {
        plain();
        let lines = identity_lines("evil\npackage", "1.0\u{1b}[31m", "0", None);
        assert_eq!(lines[0], "  Package: evil\\npackage");
        assert_eq!(lines[1], "  Version: 1.0\\u{1b}[31m");

        let components = HashMap::from([("weird\nname".to_owned(), component("weird\nname", 1))]);
        let table = component_table_lines(&components);
        assert_eq!(table.len(), 2);
        assert!(table[1].starts_with("  weird\\nname"));
    }

    #[test]
    fn chunking_reports_deduplication_only_when_savings_exist() {
        plain();
        let stats = ChunkStats {
            chunked_files: 3,
            whole_files: 4,
            total_chunks: 30,
            unique_chunks: 27,
            dedup_savings: 0,
        };
        let lines = chunking_lines(&stats);
        assert_eq!(lines[0], "Chunking:");
        assert_eq!(lines[1], "  Chunked files: 3");
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("Intra-package deduplication"))
        );

        let saved = ChunkStats {
            dedup_savings: 512,
            ..stats
        };
        let lines = chunking_lines(&saved);
        assert_eq!(
            lines.last().unwrap(),
            "  Intra-package deduplication: 512 bytes saved"
        );
    }

    #[test]
    fn loss_rows_keep_typed_categories_and_escaped_notes() {
        plain();
        assert!(loss_lines(&LossReport::default(), "DEB").is_empty());

        let mut report = LossReport::default();
        report.add_unsupported("scriptlets\nare dropped");
        report.add_hook_note("post-install hook");
        report.add_dependency_note("libc6");
        assert_eq!(
            loss_lines(&report, "DEB"),
            vec![
                "Conversion notes for DEB:",
                "[warn]     Unsupported  scriptlets\\nare dropped",
                "[info]     Hook  post-install hook",
                "[info]     Dependency  libc6",
            ]
        );
    }
}
