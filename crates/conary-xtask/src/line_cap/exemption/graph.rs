// crates/conary-xtask/src/line_cap/exemption/graph.rs

//! Resolve source children in the directory established by each load site.
//! A file can be loaded both conventionally and through an exact path. Those
//! contexts remain separate until their gates are aggregated for the file cap.

use super::*;
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum LoadKind {
    Flat,
    Adjacent,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct LoadContext {
    file: String,
    kind: LoadKind,
}

impl LoadContext {
    fn directory(&self) -> PathBuf {
        let path = Path::new(&self.file);
        match self.kind {
            LoadKind::Flat => path
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .join(path.file_stem().expect("Rust source has a stem")),
            LoadKind::Adjacent => path.parent().unwrap_or_else(|| Path::new("")).to_path_buf(),
        }
    }
}

pub(crate) struct SourceGraph {
    pub(crate) declarations: BTreeMap<String, Vec<ModuleDeclaration>>,
    pub(crate) gates: BTreeMap<String, ExemptionGate>,
    pub(crate) unresolved_sources: BTreeMap<String, String>,
}

pub(crate) fn collect_source_graph(
    sources: &BTreeMap<String, syn::File>,
    targets: &TargetRoots,
    root: &Path,
) -> Result<SourceGraph, String> {
    build_source_graph(sources, targets, Some(root))
}

/// Logical fixtures supply resolved source identities without filesystem I/O.
/// Production callers always verify load identity against the actual scan root.
#[cfg(test)]
pub(crate) fn collect_fixture_graph(
    sources: &BTreeMap<String, syn::File>,
    targets: &TargetRoots,
) -> Result<SourceGraph, String> {
    build_source_graph(sources, targets, None)
}

fn build_source_graph(
    sources: &BTreeMap<String, syn::File>,
    targets: &TargetRoots,
    root: Option<&Path>,
) -> Result<SourceGraph, String> {
    let mut unresolved_sources = targets
        .iter()
        .filter(|(file, kind)| **kind == CargoTarget::Other && !sources.contains_key(*file))
        .map(|(file, _)| (file.clone(), "unscanned Cargo production target".to_owned()))
        .collect::<BTreeMap<_, _>>();
    let mut roots = BTreeMap::new();
    for file in sources.keys() {
        let target = targets.get(file).copied();
        if let Some(target) = target {
            roots.insert(
                LoadContext {
                    file: file.clone(),
                    kind: LoadKind::Adjacent,
                },
                target,
            );
        }
    }
    let mut queue: VecDeque<_> = roots.keys().cloned().collect();
    let mut seen = BTreeSet::new();
    let mut intrinsic = BTreeMap::new();
    let mut context_declarations: BTreeMap<LoadContext, Vec<ModuleDeclaration<LoadContext>>> =
        BTreeMap::new();
    let mut declarations: BTreeMap<String, Vec<ModuleDeclaration>> = BTreeMap::new();

    loop {
        while let Some(context) = queue.pop_front() {
            let Some(syntax) = sources.get(&context.file) else {
                continue;
            };
            if !seen.insert(context.clone()) {
                continue;
            }
            let gate = if cfg::configuration_is_test_only(&syntax.attrs) {
                ExemptionGate::TestGated
            } else if roots.get(&context) == Some(&CargoTarget::Other) {
                ExemptionGate::Ungated
            } else {
                ExemptionGate::Unknown
            };
            intrinsic.insert(context.clone(), gate);
            let mut sites = BTreeMap::new();
            collect_module_declarations(
                syntax,
                Path::new(&context.file),
                &mut sites,
                &context.directory(),
            );
            match collect_includes_with_authority(syntax, Path::new(&context.file), &mut sites) {
                Ok(false) => {}
                Ok(true) => {
                    unresolved_sources.insert(
                        context.file.clone(),
                        "production macro invocation requires compiler name resolution and expansion".to_owned(),
                    );
                }
                Err(error) => {
                    unresolved_sources.insert(context.file.clone(), error);
                }
            }
            for (file, entries) in sites {
                for site in entries {
                    if sources.contains_key(&file)
                        && root.is_some_and(|root| {
                            !load_identity_matches(root, &site.load_path, &file)
                        })
                    {
                        if !site.test_gated {
                            unresolved_sources
                                .entry(context.file.clone())
                                .or_insert_with(|| {
                                    format!(
                                        "unresolved filesystem load identity: {}",
                                        site.load_path.display()
                                    )
                                });
                        }
                        // Even a test-only spelling cannot certify the wrong
                        // scanned file. Leave that file to its actual load sites.
                        continue;
                    }
                    if !site.test_gated
                        && !sources.contains_key(&file)
                        && !alternate_module_path(Path::new(&file), site.kind)
                            .is_some_and(|alternate| sources.contains_key(&path_text(&alternate)))
                    {
                        unresolved_sources
                            .entry(context.file.clone())
                            .or_insert_with(|| format!("unscanned Rust load target: {file}"));
                    }
                    let target = LoadContext {
                        file: file.clone(),
                        kind: match site.kind {
                            DeclarationKind::Flat => LoadKind::Flat,
                            DeclarationKind::Exact | DeclarationKind::ModuleRoot => {
                                LoadKind::Adjacent
                            }
                        },
                    };
                    context_declarations
                        .entry(target.clone())
                        .or_default()
                        .push(ModuleDeclaration {
                            declaring_file: context.clone(),
                            load_path: site.load_path.clone(),
                            name: site.name.clone(),
                            kind: site.kind,
                            depth: site.depth,
                            test_gated: site.test_gated,
                            is_module: site.is_module,
                        });
                    declarations.entry(file.clone()).or_default().push(site);
                    queue.push_back(target);
                }
            }
        }
        // Unreferenced source is still measured and can contain explicit test
        // gates. Its conventional fallback has unknown authority; a filename
        // alone cannot make this orphan a production or test Cargo target.
        let Some(file) = sources
            .keys()
            .find(|file| !seen.iter().any(|context| &context.file == *file))
        else {
            break;
        };
        queue.push_back(LoadContext {
            file: file.clone(),
            kind: if Path::new(file).file_name() == Some(OsStr::new("mod.rs")) {
                LoadKind::Adjacent
            } else {
                LoadKind::Flat
            },
        });
    }

    let resolved = resolve_gates(&intrinsic, &context_declarations, &roots);
    let mut gates = BTreeMap::new();
    for context in seen {
        let gate = resolved
            .get(&context)
            .copied()
            .unwrap_or(ExemptionGate::Unknown);
        gates
            .entry(context.file)
            .and_modify(|current| {
                *current = match (*current, gate) {
                    (ExemptionGate::Ungated, _) | (_, ExemptionGate::Ungated) => {
                        ExemptionGate::Ungated
                    }
                    (ExemptionGate::Unknown, _) | (_, ExemptionGate::Unknown) => {
                        ExemptionGate::Unknown
                    }
                    _ => ExemptionGate::TestGated,
                };
            })
            .or_insert(gate);
    }
    if unresolved_sources
        .keys()
        .any(|file| gates.get(file) != Some(&ExemptionGate::TestGated))
    {
        // An opaque expansion can introduce another production load site. It
        // invalidates contextual test-only proof, but cannot remove a cfg guard
        // carried by the loaded source itself. Preserve known production sites.
        for (file, gate) in &mut gates {
            if *gate == ExemptionGate::TestGated
                && !cfg::configuration_is_test_only(&sources[file].attrs)
            {
                *gate = ExemptionGate::Unknown;
            }
        }
    }
    // A macro in a source reached only through test contexts cannot seed a
    // production load. If another production expansion made that context
    // uncertain above, its diagnostic remains in the incomplete graph.
    unresolved_sources.retain(|file, _| gates.get(file) != Some(&ExemptionGate::TestGated));
    Ok(SourceGraph {
        declarations,
        gates,
        unresolved_sources,
    })
}

fn load_identity_matches(root: &Path, original: &Path, scanned: &str) -> bool {
    match (
        root.join(original).canonicalize(),
        root.join(scanned).canonicalize(),
    ) {
        (Ok(original), Ok(scanned)) => original == scanned,
        _ => false,
    }
}
