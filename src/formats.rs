use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub const JAVASCRIPT: &str = "javascript";
pub const TYPESCRIPT: &str = "typescript";
pub const MARKDOWN: &str = "markdown";
pub const MDX: &str = "mdx";
pub const RUST: &str = "rust";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corpus {
    Code,
    Docs,
}

impl Corpus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Docs => "docs",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Understanding {
    PlainText,
    NamedSections,
    Ast,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepositoryPolicy {
    Code,
    Documentation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DependencyPolicy {
    Excluded,
    EcmaScript,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectoryPolicy {
    Standard,
    CargoTarget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Extractor {
    EcmaScript,
    Documentation,
    RustText,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RankedProjection {
    CodeLexicalAndVector,
    CodeLexical,
    DocumentationLexicalAndVector,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExactDefinitionPolicy {
    Disabled,
    NamedChunks,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExactOccurrencePolicy {
    Disabled,
    EcmaScript,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckerPolicy {
    Disabled,
    TypeScript,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchAffinity {
    None,
    Checker,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StructuralPolicy {
    None,
    EcmaScript,
    DocumentationMetadata,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolverPolicy {
    None,
    EcmaScript,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconnaissancePolicy {
    Disabled,
    Repository,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotContractPolicy {
    /// Covered by the pre-registry `extraction_version` snapshot input.
    LegacyCode,
    /// Covered by the pre-registry documentation chunk-format snapshot input.
    LegacyDocumentation,
    /// Hash this format's producer and persisted contract only while rows of
    /// the format are present, preserving the phase-0 legacy snapshot.
    PerFormatWhenPresent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormatSpec {
    pub id: &'static str,
    pub extensions: &'static [&'static str],
    pub corpus: Corpus,
    pub understanding: Understanding,
    pub repository: RepositoryPolicy,
    pub dependency: DependencyPolicy,
    pub directory: DirectoryPolicy,
    pub extractor: Extractor,
    pub extractor_version: &'static str,
    pub ranked: RankedProjection,
    pub exact_definition: ExactDefinitionPolicy,
    pub exact_occurrence: ExactOccurrencePolicy,
    pub checker: CheckerPolicy,
    pub watch_affinity: WatchAffinity,
    pub structural: StructuralPolicy,
    pub resolver: ResolverPolicy,
    pub reconnaissance: ReconnaissancePolicy,
    pub snapshot_contract: SnapshotContractPolicy,
}

impl FormatSpec {
    pub const fn repository_code(self) -> bool {
        matches!(self.repository, RepositoryPolicy::Code)
    }

    pub const fn documentation(self) -> bool {
        matches!(self.repository, RepositoryPolicy::Documentation)
    }

    pub const fn dependency_code(self) -> bool {
        matches!(self.dependency, DependencyPolicy::EcmaScript)
    }

    pub const fn vector_eligible(self) -> bool {
        matches!(
            self.ranked,
            RankedProjection::CodeLexicalAndVector
                | RankedProjection::DocumentationLexicalAndVector
        )
    }

    pub const fn lexical_eligible(self) -> bool {
        matches!(
            self.ranked,
            RankedProjection::CodeLexicalAndVector
                | RankedProjection::CodeLexical
                | RankedProjection::DocumentationLexicalAndVector
        )
    }

    pub const fn documentation_metadata_eligible(self) -> bool {
        matches!(self.structural, StructuralPolicy::DocumentationMetadata)
    }

    pub const fn exact_definition_eligible(self) -> bool {
        matches!(self.exact_definition, ExactDefinitionPolicy::NamedChunks)
    }

    pub const fn exact_occurrence_eligible(self) -> bool {
        matches!(self.exact_occurrence, ExactOccurrencePolicy::EcmaScript)
    }

    pub const fn checker_eligible(self) -> bool {
        matches!(self.checker, CheckerPolicy::TypeScript)
    }

    pub const fn checker_watch_affinity(self) -> bool {
        matches!(self.watch_affinity, WatchAffinity::Checker)
    }

    pub const fn resolver_eligible(self) -> bool {
        matches!(self.resolver, ResolverPolicy::EcmaScript)
    }

    pub const fn structural_eligible(self) -> bool {
        matches!(self.structural, StructuralPolicy::EcmaScript)
    }

    pub const fn reconnaissance_eligible(self) -> bool {
        matches!(self.reconnaissance, ReconnaissancePolicy::Repository)
    }
}

pub const ALL: &[FormatSpec] = &[
    FormatSpec {
        id: JAVASCRIPT,
        extensions: &["js", "jsx", "mjs", "cjs"],
        corpus: Corpus::Code,
        understanding: Understanding::Ast,
        repository: RepositoryPolicy::Code,
        dependency: DependencyPolicy::EcmaScript,
        directory: DirectoryPolicy::Standard,
        extractor: Extractor::EcmaScript,
        extractor_version: crate::entity::EXTRACTION_VERSION,
        ranked: RankedProjection::CodeLexicalAndVector,
        exact_definition: ExactDefinitionPolicy::NamedChunks,
        exact_occurrence: ExactOccurrencePolicy::EcmaScript,
        checker: CheckerPolicy::TypeScript,
        watch_affinity: WatchAffinity::Checker,
        structural: StructuralPolicy::EcmaScript,
        resolver: ResolverPolicy::EcmaScript,
        reconnaissance: ReconnaissancePolicy::Repository,
        snapshot_contract: SnapshotContractPolicy::LegacyCode,
    },
    FormatSpec {
        id: TYPESCRIPT,
        extensions: &["ts", "tsx", "mts", "cts"],
        corpus: Corpus::Code,
        understanding: Understanding::Ast,
        repository: RepositoryPolicy::Code,
        dependency: DependencyPolicy::EcmaScript,
        directory: DirectoryPolicy::Standard,
        extractor: Extractor::EcmaScript,
        extractor_version: crate::entity::EXTRACTION_VERSION,
        ranked: RankedProjection::CodeLexicalAndVector,
        exact_definition: ExactDefinitionPolicy::NamedChunks,
        exact_occurrence: ExactOccurrencePolicy::EcmaScript,
        checker: CheckerPolicy::TypeScript,
        watch_affinity: WatchAffinity::Checker,
        structural: StructuralPolicy::EcmaScript,
        resolver: ResolverPolicy::EcmaScript,
        reconnaissance: ReconnaissancePolicy::Repository,
        snapshot_contract: SnapshotContractPolicy::LegacyCode,
    },
    FormatSpec {
        id: MARKDOWN,
        extensions: &["md"],
        corpus: Corpus::Docs,
        understanding: Understanding::NamedSections,
        repository: RepositoryPolicy::Documentation,
        dependency: DependencyPolicy::Excluded,
        directory: DirectoryPolicy::Standard,
        extractor: Extractor::Documentation,
        extractor_version: crate::docs::CHUNK_FORMAT_VERSION,
        ranked: RankedProjection::DocumentationLexicalAndVector,
        exact_definition: ExactDefinitionPolicy::Disabled,
        exact_occurrence: ExactOccurrencePolicy::Disabled,
        checker: CheckerPolicy::Disabled,
        watch_affinity: WatchAffinity::None,
        structural: StructuralPolicy::DocumentationMetadata,
        resolver: ResolverPolicy::None,
        reconnaissance: ReconnaissancePolicy::Disabled,
        snapshot_contract: SnapshotContractPolicy::LegacyDocumentation,
    },
    FormatSpec {
        id: MDX,
        extensions: &["mdx"],
        corpus: Corpus::Docs,
        understanding: Understanding::NamedSections,
        repository: RepositoryPolicy::Documentation,
        dependency: DependencyPolicy::Excluded,
        directory: DirectoryPolicy::Standard,
        extractor: Extractor::Documentation,
        extractor_version: crate::docs::CHUNK_FORMAT_VERSION,
        ranked: RankedProjection::DocumentationLexicalAndVector,
        exact_definition: ExactDefinitionPolicy::Disabled,
        exact_occurrence: ExactOccurrencePolicy::Disabled,
        checker: CheckerPolicy::Disabled,
        watch_affinity: WatchAffinity::None,
        structural: StructuralPolicy::DocumentationMetadata,
        resolver: ResolverPolicy::None,
        reconnaissance: ReconnaissancePolicy::Disabled,
        snapshot_contract: SnapshotContractPolicy::LegacyDocumentation,
    },
    FormatSpec {
        id: RUST,
        extensions: &["rs"],
        corpus: Corpus::Code,
        understanding: Understanding::PlainText,
        repository: RepositoryPolicy::Code,
        dependency: DependencyPolicy::Excluded,
        directory: DirectoryPolicy::CargoTarget,
        extractor: Extractor::RustText,
        extractor_version: "rust-text-ra-ap-syntax-0.0.349-cargo-edition-v3",
        ranked: RankedProjection::CodeLexical,
        exact_definition: ExactDefinitionPolicy::Disabled,
        exact_occurrence: ExactOccurrencePolicy::Disabled,
        checker: CheckerPolicy::Disabled,
        watch_affinity: WatchAffinity::None,
        structural: StructuralPolicy::None,
        resolver: ResolverPolicy::None,
        reconnaissance: ReconnaissancePolicy::Disabled,
        snapshot_contract: SnapshotContractPolicy::PerFormatWhenPresent,
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    Dependency,
    CodeLexical,
    DocumentationLexical,
    CodeVector,
    DocumentationVector,
    DocumentationMetadata,
    ExactDefinition,
    ExactOccurrence,
    Checker,
    Structural,
    Resolver,
    Reconnaissance,
}

pub fn by_id(id: &str) -> Option<&'static FormatSpec> {
    ALL.iter().find(|format| format.id == id)
}

pub fn for_path(path: &Path) -> Option<&'static FormatSpec> {
    let extension = path.extension()?.to_str()?;
    ALL.iter()
        .find(|format| format.extensions.contains(&extension))
}

pub fn documentation_for_path(path: &Path) -> Option<&'static FormatSpec> {
    for_path(path).filter(|format| format.documentation())
}

pub fn dependency_code_for_path(path: &Path) -> Option<&'static FormatSpec> {
    for_path(path).filter(|format| format.dependency_code())
}

pub fn repository_code_for_path(path: &Path) -> Option<&'static FormatSpec> {
    for_path(path).filter(|format| format.repository_code())
}

/// Apply a format's directory policy using membership captured by the
/// authoritative repository traversal. `cargo_roots` contains relative
/// package directories whose `Cargo.toml` was a visible regular file in that
/// same walk.
pub fn repository_directory_admitted(
    format: &FormatSpec,
    relative: &Path,
    cargo_roots: &BTreeSet<PathBuf>,
) -> bool {
    format.directory != DirectoryPolicy::CargoTarget || !beneath_cargo_target(relative, cargo_roots)
}

pub fn eligible_ids(capability: Capability) -> Vec<&'static str> {
    ALL.iter()
        .filter(|format| match capability {
            Capability::Dependency => format.dependency_code(),
            Capability::CodeLexical => format.corpus == Corpus::Code && format.lexical_eligible(),
            Capability::DocumentationLexical => {
                format.corpus == Corpus::Docs && format.lexical_eligible()
            }
            Capability::CodeVector => format.corpus == Corpus::Code && format.vector_eligible(),
            Capability::DocumentationVector => {
                format.corpus == Corpus::Docs && format.vector_eligible()
            }
            Capability::DocumentationMetadata => format.documentation_metadata_eligible(),
            Capability::ExactDefinition => format.exact_definition_eligible(),
            Capability::ExactOccurrence => format.exact_occurrence_eligible(),
            Capability::Checker => format.checker_eligible(),
            Capability::Structural => format.structural_eligible(),
            Capability::Resolver => format.resolver_eligible(),
            Capability::Reconnaissance => format.reconnaissance_eligible(),
        })
        .map(|format| format.id)
        .collect()
}

pub fn eligible_ids_json(capability: Capability) -> String {
    serde_json::to_string(&eligible_ids(capability)).expect("static format ids serialize")
}

pub fn eligible_ids_in_scope_json(capability: Capability, requested: &[String]) -> String {
    let eligible = eligible_ids(capability)
        .into_iter()
        .filter(|format| requested.is_empty() || requested.iter().any(|value| value == format))
        .collect::<Vec<_>>();
    serde_json::to_string(&eligible).expect("static scoped format ids serialize")
}

pub fn contract_meta_key(format: &FormatSpec) -> String {
    format!("format_contract_version:{}", format.id)
}

/// Ordered ECMAScript suffix policy shared by workspace/package target
/// probing and the module resolver. The registry remains the extension source;
/// this helper owns only the existing TypeScript-first resolution preference.
pub fn ecmascript_resolution_extensions() -> Vec<&'static str> {
    [TYPESCRIPT, JAVASCRIPT]
        .into_iter()
        .flat_map(|id| {
            by_id(id)
                .expect("built-in ECMAScript format is registered")
                .extensions
                .iter()
                .copied()
        })
        .collect()
}

/// Package entry-field suffix substitution in preference order. This is the
/// ECMAScript extractor/resolver behavior attached to the registered formats,
/// kept here so workspace discovery does not own another extension switch.
pub fn ecmascript_entry_extension_candidates(extension: &str) -> Option<&'static [&'static str]> {
    match extension {
        "ts" => Some(&["ts"]),
        "tsx" => Some(&["tsx"]),
        "mts" => Some(&["mts"]),
        "cts" => Some(&["cts"]),
        "jsx" => Some(&["jsx"]),
        "js" => Some(&["ts", "tsx", "js", "jsx"]),
        "mjs" => Some(&["mts", "mjs"]),
        "cjs" => Some(&["cts", "cjs"]),
        _ => None,
    }
}

/// Source-mirror substitutions used by the checker package policy. These are
/// intentionally broader than package entry candidates for ESM/CJS outputs.
pub fn ecmascript_source_mirror_extension_candidates(
    extension: &str,
) -> Option<&'static [&'static str]> {
    match extension {
        "js" => Some(&["ts", "tsx", "js", "jsx"]),
        "jsx" => Some(&["tsx", "jsx"]),
        "mjs" => Some(&["mts", "ts", "tsx", "mjs"]),
        "cjs" => Some(&["cts", "ts", "tsx", "cjs"]),
        _ => None,
    }
}

pub fn ecmascript_resolver_extension_aliases() -> Vec<(String, Vec<String>)> {
    ["js", "mjs", "cjs"]
        .into_iter()
        .map(|extension| {
            let aliases = ecmascript_entry_extension_candidates(extension)
                .expect("built-in resolver extension has aliases")
                .iter()
                .map(|alias| format!(".{alias}"))
                .collect();
            (format!(".{extension}"), aliases)
        })
        .collect()
}

fn beneath_cargo_target(relative: &Path, cargo_roots: &BTreeSet<PathBuf>) -> bool {
    let mut parent = PathBuf::new();
    for component in relative.components() {
        let name = component.as_os_str();
        if name == "target" && cargo_roots.contains(&parent) {
            return true;
        }
        parent.push(name);
    }
    false
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn format_registry_contract_is_complete_and_origin_specific() {
        let ids = ALL.iter().map(|format| format.id).collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), ALL.len(), "format ids must be unique");
        let extensions = ALL
            .iter()
            .flat_map(|format| format.extensions.iter().copied())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            extensions.len(),
            ALL.iter()
                .map(|format| format.extensions.len())
                .sum::<usize>(),
            "format extensions must be unique"
        );
        let expected = [
            FormatSpec {
                id: JAVASCRIPT,
                extensions: &["js", "jsx", "mjs", "cjs"],
                corpus: Corpus::Code,
                understanding: Understanding::Ast,
                repository: RepositoryPolicy::Code,
                dependency: DependencyPolicy::EcmaScript,
                directory: DirectoryPolicy::Standard,
                extractor: Extractor::EcmaScript,
                extractor_version: crate::entity::EXTRACTION_VERSION,
                ranked: RankedProjection::CodeLexicalAndVector,
                exact_definition: ExactDefinitionPolicy::NamedChunks,
                exact_occurrence: ExactOccurrencePolicy::EcmaScript,
                checker: CheckerPolicy::TypeScript,
                watch_affinity: WatchAffinity::Checker,
                structural: StructuralPolicy::EcmaScript,
                resolver: ResolverPolicy::EcmaScript,
                reconnaissance: ReconnaissancePolicy::Repository,
                snapshot_contract: SnapshotContractPolicy::LegacyCode,
            },
            FormatSpec {
                id: TYPESCRIPT,
                extensions: &["ts", "tsx", "mts", "cts"],
                corpus: Corpus::Code,
                understanding: Understanding::Ast,
                repository: RepositoryPolicy::Code,
                dependency: DependencyPolicy::EcmaScript,
                directory: DirectoryPolicy::Standard,
                extractor: Extractor::EcmaScript,
                extractor_version: crate::entity::EXTRACTION_VERSION,
                ranked: RankedProjection::CodeLexicalAndVector,
                exact_definition: ExactDefinitionPolicy::NamedChunks,
                exact_occurrence: ExactOccurrencePolicy::EcmaScript,
                checker: CheckerPolicy::TypeScript,
                watch_affinity: WatchAffinity::Checker,
                structural: StructuralPolicy::EcmaScript,
                resolver: ResolverPolicy::EcmaScript,
                reconnaissance: ReconnaissancePolicy::Repository,
                snapshot_contract: SnapshotContractPolicy::LegacyCode,
            },
            FormatSpec {
                id: MARKDOWN,
                extensions: &["md"],
                corpus: Corpus::Docs,
                understanding: Understanding::NamedSections,
                repository: RepositoryPolicy::Documentation,
                dependency: DependencyPolicy::Excluded,
                directory: DirectoryPolicy::Standard,
                extractor: Extractor::Documentation,
                extractor_version: crate::docs::CHUNK_FORMAT_VERSION,
                ranked: RankedProjection::DocumentationLexicalAndVector,
                exact_definition: ExactDefinitionPolicy::Disabled,
                exact_occurrence: ExactOccurrencePolicy::Disabled,
                checker: CheckerPolicy::Disabled,
                watch_affinity: WatchAffinity::None,
                structural: StructuralPolicy::DocumentationMetadata,
                resolver: ResolverPolicy::None,
                reconnaissance: ReconnaissancePolicy::Disabled,
                snapshot_contract: SnapshotContractPolicy::LegacyDocumentation,
            },
            FormatSpec {
                id: MDX,
                extensions: &["mdx"],
                corpus: Corpus::Docs,
                understanding: Understanding::NamedSections,
                repository: RepositoryPolicy::Documentation,
                dependency: DependencyPolicy::Excluded,
                directory: DirectoryPolicy::Standard,
                extractor: Extractor::Documentation,
                extractor_version: crate::docs::CHUNK_FORMAT_VERSION,
                ranked: RankedProjection::DocumentationLexicalAndVector,
                exact_definition: ExactDefinitionPolicy::Disabled,
                exact_occurrence: ExactOccurrencePolicy::Disabled,
                checker: CheckerPolicy::Disabled,
                watch_affinity: WatchAffinity::None,
                structural: StructuralPolicy::DocumentationMetadata,
                resolver: ResolverPolicy::None,
                reconnaissance: ReconnaissancePolicy::Disabled,
                snapshot_contract: SnapshotContractPolicy::LegacyDocumentation,
            },
            FormatSpec {
                id: RUST,
                extensions: &["rs"],
                corpus: Corpus::Code,
                understanding: Understanding::PlainText,
                repository: RepositoryPolicy::Code,
                dependency: DependencyPolicy::Excluded,
                directory: DirectoryPolicy::CargoTarget,
                extractor: Extractor::RustText,
                extractor_version: "rust-text-ra-ap-syntax-0.0.349-cargo-edition-v3",
                ranked: RankedProjection::CodeLexical,
                exact_definition: ExactDefinitionPolicy::Disabled,
                exact_occurrence: ExactOccurrencePolicy::Disabled,
                checker: CheckerPolicy::Disabled,
                watch_affinity: WatchAffinity::None,
                structural: StructuralPolicy::None,
                resolver: ResolverPolicy::None,
                reconnaissance: ReconnaissancePolicy::Disabled,
                snapshot_contract: SnapshotContractPolicy::PerFormatWhenPresent,
            },
        ];
        assert_eq!(ALL, expected.as_slice());

        for id in [JAVASCRIPT, TYPESCRIPT] {
            let format = by_id(id).unwrap();
            assert_eq!(format.corpus, Corpus::Code);
            assert!(format.repository_code());
            assert!(format.dependency_code());
            assert!(format.vector_eligible());
            assert!(format.exact_definition_eligible());
            assert!(format.exact_occurrence_eligible());
            assert!(format.checker_eligible());
            assert!(format.checker_watch_affinity());
            assert!(format.resolver_eligible());
        }
        for id in [MARKDOWN, MDX] {
            let format = by_id(id).unwrap();
            assert_eq!(format.corpus, Corpus::Docs);
            assert!(format.documentation());
            assert!(!format.dependency_code());
            assert!(!format.exact_definition_eligible());
            assert!(!format.checker_eligible());
            assert!(!format.checker_watch_affinity());
        }
        let rust = by_id(RUST).unwrap();
        assert_eq!(rust.corpus, Corpus::Code);
        assert!(rust.repository_code());
        assert!(!rust.dependency_code());
        assert!(!rust.vector_eligible());
        assert!(!rust.exact_definition_eligible());
        assert!(!rust.exact_occurrence_eligible());
        assert!(!rust.checker_eligible());
        assert!(!rust.checker_watch_affinity());
        assert!(!rust.resolver_eligible());
    }

    #[test]
    fn repository_and_dependency_admission_are_independent() -> Result<()> {
        let rust = repository_code_for_path(Path::new("src/lib.rs")).unwrap();
        let cargo_roots = [PathBuf::new()].into_iter().collect::<BTreeSet<_>>();

        assert_eq!(
            repository_code_for_path(Path::new("src/lib.rs")).map(|format| format.id),
            Some(RUST)
        );
        assert!(!repository_directory_admitted(
            rust,
            Path::new("target/debug/out.rs"),
            &cargo_roots,
        ));
        assert!(repository_directory_admitted(
            rust,
            Path::new("src/target/authored.rs"),
            &cargo_roots,
        ));
        assert!(repository_directory_admitted(
            by_id(TYPESCRIPT).unwrap(),
            Path::new("target/debug/authored.ts"),
            &cargo_roots,
        ));
        assert!(dependency_code_for_path(Path::new("native.rs")).is_none());
        assert_eq!(
            dependency_code_for_path(Path::new("index.ts")).map(|format| format.id),
            Some(TYPESCRIPT)
        );
        Ok(())
    }

    #[test]
    fn extensions_are_exact_lowercase() {
        assert_eq!(
            for_path(Path::new("src/lib.rs")).map(|format| format.id),
            Some(RUST)
        );
        assert!(for_path(Path::new("src/lib.RS")).is_none());
        assert!(for_path(Path::new("README.MD")).is_none());
    }
}
