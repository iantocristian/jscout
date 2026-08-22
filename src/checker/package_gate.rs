use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use serde_json::Value;

use super::protocol::FileOwnership;

const ABSENT_INPUT_HASH: &str = "absent:v1";
const INVENTORY_SQL: &str = "SELECT id, path, role FROM files
     WHERE origin IN ('repository', 'workspace')
     ORDER BY path";
const SOURCE_EXTENSIONS: &[&str] = &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];
const OUTPUT_DIRECTORIES: &[&str] = &["dist", "out", "build", "lib"];
const OUTPUT_FLAVORS: &[&str] = &["esm", "cjs", "es", "es6", "mjs", "umd", "lib", "types"];

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManifestObservation {
    path: PathBuf,
    display: String,
    source_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GatePlan {
    pub(super) admitted_orphans: BTreeSet<String>,
    pub(super) fingerprint: String,
    observations: Vec<ManifestObservation>,
}

impl GatePlan {
    /// Recheck every manifest boundary observed while the policy was planned.
    /// Missing probes are as important as existing manifests: a newly created
    /// closer `package.json` changes both the package majority and inferred
    /// scope membership.
    pub(super) fn validate_fresh(&self) -> Result<()> {
        for observation in &self.observations {
            if observation.source_hash == ABSENT_INPUT_HASH {
                match fs::symlink_metadata(&observation.path) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!(
                                "could not inspect package-policy input {}",
                                observation.display
                            )
                        });
                    }
                    Ok(_) => bail!(
                        "package boundary appeared after checker planning: {}",
                        observation.display
                    ),
                }
                continue;
            }
            let bytes = fs::read(&observation.path).with_context(|| {
                format!(
                    "could not read package-policy input {}",
                    observation.display
                )
            })?;
            let actual = blake3::hash(&bytes).to_hex().to_string();
            if actual != observation.source_hash {
                bail!(
                    "package manifest changed after checker planning: {}",
                    observation.display
                );
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct IndexedSource {
    id: i64,
    path: String,
    role: String,
    package: String,
}

impl IndexedSource {
    fn default_role(&self) -> bool {
        matches!(self.role.as_str(), "production" | "unknown")
    }
}

#[derive(Debug, Clone)]
struct PackageRecord {
    identity: String,
    directory: PathBuf,
    manifest: Option<Value>,
}

#[derive(Debug, Clone, Copy, Default)]
struct PackageCounts {
    total_default: usize,
    unowned_default: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConfiguredOwners {
    selected: BTreeSet<String>,
    excluded: BTreeSet<String>,
    tooling_fallback: bool,
}

impl PackageCounts {
    fn js_first(self) -> bool {
        self.unowned_default.saturating_mul(2) > self.total_default
    }
}

#[derive(Debug, Clone)]
struct ManifestTarget {
    value: String,
    source_mirror: bool,
}

/// Repository/workspace paths used by the full, configuration-only ownership
/// pass. Keep this query shared with [`evaluate`] so sidecar ownership and the
/// policy engine cannot silently describe different inventories.
pub(super) fn inventory_paths(conn: &Connection) -> Result<Vec<String>> {
    let mut statement = conn.prepare(INVENTORY_SQL)?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Whether TypeScript configuration owns a file. An owner suppressed by the
/// tooling preference is still configured ownership; the gate must inspect
/// both selected and excluded project IDs before treating a file as orphaned.
pub(super) fn has_configured_owner(ownership: &FileOwnership) -> bool {
    configured_owners(ownership).is_some()
}

pub(super) fn same_configured_ownership(left: &FileOwnership, right: &FileOwnership) -> bool {
    configured_owners(left) == configured_owners(right)
}

fn configured_owners(ownership: &FileOwnership) -> Option<ConfiguredOwners> {
    let selected = ownership
        .project_ids
        .iter()
        .filter(|project| !project.starts_with("inferred:"))
        .cloned()
        .collect::<BTreeSet<_>>();
    let excluded = ownership
        .excluded_project_ids
        .iter()
        .filter(|project| !project.starts_with("inferred:"))
        .cloned()
        .collect::<BTreeSet<_>>();
    (!selected.is_empty() || !excluded.is_empty()).then_some(ConfiguredOwners {
        selected,
        excluded,
        tooling_fallback: ownership.tooling_fallback,
    })
}

/// Evaluate default inferred-project admission from one complete ownership
/// inventory. This performs no TypeScript Program construction. The returned
/// orphan set is the full root population for the second `plan_members` call,
/// not merely files that currently contain eligible member calls.
pub(super) fn evaluate(
    root: &Path,
    conn: &Connection,
    ownership: &[FileOwnership],
    include_all: bool,
) -> Result<GatePlan> {
    let root = fs::canonicalize(root)
        .with_context(|| format!("repository root does not exist: {}", root.display()))?;
    let mut sources = load_inventory(conn)?;
    let configured_owners = configured_inventory(&sources, ownership)?;
    let mut package_cache = HashMap::<PathBuf, String>::new();
    let mut packages = BTreeMap::<String, PackageRecord>::new();
    let mut observations = BTreeMap::<String, ManifestObservation>::new();
    for source in &mut sources {
        source.package = nearest_package(
            &root,
            &source.path,
            &mut package_cache,
            &mut packages,
            &mut observations,
        )?;
    }

    let mut counts = BTreeMap::<String, PackageCounts>::new();
    for source in &sources {
        if !source.default_role() {
            continue;
        }
        let count = counts.entry(source.package.clone()).or_default();
        count.total_default += 1;
        if !configured_owners.contains_key(&source.path) {
            count.unowned_default += 1;
        }
    }

    let source_by_path = sources
        .iter()
        .map(|source| (source.path.as_str(), source))
        .collect::<BTreeMap<_, _>>();
    let seed_ids = manifest_seed_ids(&root, &packages, &source_by_path);
    let reachable = runtime_reachable(conn, &seed_ids)?;
    let admitted_orphans = sources
        .iter()
        .filter(|source| !configured_owners.contains_key(&source.path))
        .filter(|source| {
            include_all
                || (source.default_role()
                    && (counts
                        .get(&source.package)
                        .copied()
                        .unwrap_or_default()
                        .js_first()
                        || reachable.contains(&source.id)))
        })
        .map(|source| source.path.clone())
        .collect::<BTreeSet<_>>();
    let observations = observations.into_values().collect::<Vec<_>>();
    let fingerprint = policy_fingerprint(
        include_all,
        &sources,
        &configured_owners,
        &packages,
        &counts,
        &seed_ids,
        &admitted_orphans,
        &observations,
    );
    Ok(GatePlan {
        admitted_orphans,
        fingerprint,
        observations,
    })
}

fn load_inventory(conn: &Connection) -> Result<Vec<IndexedSource>> {
    let mut statement = conn.prepare(INVENTORY_SQL)?;
    let rows = statement.query_map([], |row| {
        Ok(IndexedSource {
            id: row.get(0)?,
            path: row.get(1)?,
            role: row.get(2)?,
            package: String::new(),
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn configured_inventory(
    sources: &[IndexedSource],
    ownership: &[FileOwnership],
) -> Result<BTreeMap<String, ConfiguredOwners>> {
    let inventory = sources
        .iter()
        .map(|source| source.path.as_str())
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    let mut configured = BTreeMap::new();
    for entry in ownership {
        if !inventory.contains(entry.file.as_str()) {
            bail!(
                "checker returned ownership for a file outside the first-party inventory: {}",
                entry.file
            );
        }
        if !seen.insert(entry.file.as_str()) {
            bail!("checker returned duplicate ownership for {}", entry.file);
        }
        if entry.project_ids.is_empty() && entry.excluded_project_ids.is_empty() {
            bail!("checker returned no project ownership for {}", entry.file);
        }
        if let Some(owners) = configured_owners(entry) {
            configured.insert(entry.file.clone(), owners);
        }
    }
    if seen != inventory {
        let missing = inventory
            .difference(&seen)
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        bail!("checker omitted first-party ownership for: {missing}");
    }
    Ok(configured)
}

fn nearest_package(
    root: &Path,
    display: &str,
    cache: &mut HashMap<PathBuf, String>,
    packages: &mut BTreeMap<String, PackageRecord>,
    observations: &mut BTreeMap<String, ManifestObservation>,
) -> Result<String> {
    let lexical = root.join(display);
    let canonical = fs::canonicalize(&lexical)
        .with_context(|| format!("indexed source does not exist: {display}"))?;
    if !canonical.starts_with(root) {
        bail!("indexed source resolves outside the repository: {display}");
    }
    let mut directory = canonical
        .parent()
        .context("indexed source has no parent directory")?
        .to_path_buf();
    let mut visited = Vec::new();
    loop {
        if let Some(identity) = cache.get(&directory).cloned() {
            for candidate in visited {
                cache.insert(candidate, identity.clone());
            }
            return Ok(identity);
        }
        visited.push(directory.clone());
        let manifest_path = directory.join("package.json");
        let manifest_display = repository_display(root, &manifest_path)?;
        match fs::symlink_metadata(&manifest_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                record_observation(
                    observations,
                    ManifestObservation {
                        path: manifest_path,
                        display: manifest_display,
                        source_hash: ABSENT_INPUT_HASH.into(),
                    },
                )?;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("could not inspect package boundary {manifest_display}")
                });
            }
            Ok(_) => {
                let metadata = fs::metadata(&manifest_path).with_context(|| {
                    format!("could not follow package boundary {manifest_display}")
                })?;
                if !metadata.is_file() {
                    bail!("package boundary is not a file: {manifest_display}");
                }
                let bytes = fs::read(&manifest_path).with_context(|| {
                    format!("could not read package boundary {manifest_display}")
                })?;
                let source_hash = blake3::hash(&bytes).to_hex().to_string();
                record_observation(
                    observations,
                    ManifestObservation {
                        path: manifest_path,
                        display: manifest_display,
                        source_hash,
                    },
                )?;
                let identity = package_identity(root, &directory)?;
                packages
                    .entry(identity.clone())
                    .or_insert_with(|| PackageRecord {
                        identity: identity.clone(),
                        directory: directory.clone(),
                        // A malformed manifest still establishes a package
                        // boundary but contributes no runtime entry roots.
                        manifest: serde_json::from_slice(&bytes).ok(),
                    });
                for candidate in visited {
                    cache.insert(candidate, identity.clone());
                }
                return Ok(identity);
            }
        }
        if directory == root {
            let identity = ".".to_string();
            packages
                .entry(identity.clone())
                .or_insert_with(|| PackageRecord {
                    identity: identity.clone(),
                    directory: root.to_path_buf(),
                    manifest: None,
                });
            for candidate in visited {
                cache.insert(candidate, identity.clone());
            }
            return Ok(identity);
        }
        directory = directory
            .parent()
            .filter(|parent| parent.starts_with(root))
            .context("package boundary search escaped the repository")?
            .to_path_buf();
    }
}

fn record_observation(
    observations: &mut BTreeMap<String, ManifestObservation>,
    observation: ManifestObservation,
) -> Result<()> {
    if let Some(previous) = observations.insert(observation.display.clone(), observation.clone())
        && previous.source_hash != observation.source_hash
    {
        bail!(
            "package boundary changed while planning: {}",
            observation.display
        );
    }
    Ok(())
}

fn repository_display(root: &Path, path: &Path) -> Result<String> {
    Ok(path
        .strip_prefix(root)
        .with_context(|| format!("package input escapes repository: {}", path.display()))?
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/"))
}

fn package_identity(root: &Path, directory: &Path) -> Result<String> {
    let relative = repository_display(root, directory)?;
    Ok(if relative.is_empty() {
        ".".into()
    } else {
        relative
    })
}

fn manifest_seed_ids(
    root: &Path,
    packages: &BTreeMap<String, PackageRecord>,
    sources: &BTreeMap<&str, &IndexedSource>,
) -> BTreeSet<i64> {
    let mut seeds = BTreeSet::new();
    for package in packages.values() {
        let Some(manifest) = &package.manifest else {
            continue;
        };
        for target in manifest_targets(manifest) {
            seeds.extend(resolve_manifest_target(root, package, &target, sources));
        }
    }
    seeds
}

fn manifest_targets(manifest: &Value) -> Vec<ManifestTarget> {
    let mut targets = Vec::new();
    if let Some(main) = manifest.get("main").and_then(Value::as_str) {
        targets.push(ManifestTarget {
            value: main.into(),
            source_mirror: true,
        });
    }
    if let Some(exports) = manifest.get("exports") {
        let values = match exports {
            Value::Object(map) if map.keys().any(|key| key.starts_with('.')) => {
                map.values().collect::<Vec<_>>()
            }
            value => vec![value],
        };
        for value in values {
            let mut active = Vec::new();
            collect_runtime_export_targets(value, &mut active);
            targets.extend(active.into_iter().map(|value| ManifestTarget {
                value,
                source_mirror: true,
            }));
        }
    }
    match manifest.get("bin") {
        Some(Value::String(value)) => targets.push(ManifestTarget {
            value: value.clone(),
            source_mirror: true,
        }),
        Some(Value::Object(values)) => {
            targets.extend(
                values
                    .values()
                    .filter_map(Value::as_str)
                    .map(|value| ManifestTarget {
                        value: value.into(),
                        source_mirror: true,
                    }),
            );
        }
        _ => {}
    }
    if let Some(Value::Object(scripts)) = manifest.get("scripts") {
        for command in scripts.values().filter_map(Value::as_str) {
            targets.extend(script_path_tokens(command).map(|value| ManifestTarget {
                value,
                source_mirror: false,
            }));
        }
    }
    targets.sort_by(|left, right| {
        left.value
            .cmp(&right.value)
            .then_with(|| left.source_mirror.cmp(&right.source_mirror))
    });
    targets.dedup_by(|left, right| {
        left.value == right.value && left.source_mirror == right.source_mirror
    });
    targets
}

/// Package entry reachability is broader than resolving one concrete import:
/// ESM, CommonJS, browser, and custom-condition consumers are all possible
/// roots. Traverse every non-type condition instead of committing to the first
/// active branch as the module resolver does for one request.
fn collect_runtime_export_targets(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(value) => out.push(value.clone()),
        Value::Array(values) => {
            for value in values {
                collect_runtime_export_targets(value, out);
            }
        }
        Value::Object(map) => {
            for (condition, value) in map {
                if condition != "types" && !condition.starts_with("types@") {
                    collect_runtime_export_targets(value, out);
                }
            }
        }
        _ => {}
    }
}

fn script_path_tokens(command: &str) -> impl Iterator<Item = String> + '_ {
    shell_tokens(command).into_iter().filter_map(|token| {
        let token = token
            .split_once('=')
            .map_or(token.as_str(), |(_, value)| value)
            .trim_matches(['(', ')', ',', ';']);
        if token.is_empty()
            || token.starts_with('-')
            || token.contains("//")
            || token.contains(['$', '{', '}'])
        {
            return None;
        }
        let path = Path::new(token);
        let source_extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| SOURCE_EXTENSIONS.contains(&extension));
        (source_extension && (token.contains('/') || path.file_name().is_some()))
            .then(|| token.replace('\\', "/"))
    })
}

fn shell_tokens(command: &str) -> Vec<String> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Quote {
        None,
        Single,
        Double,
    }
    let mut quote = Quote::None;
    let mut escaped = false;
    let mut current = String::new();
    let mut tokens = Vec::new();
    for character in command.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        match (quote, character) {
            (Quote::None | Quote::Double, '\\') => escaped = true,
            (Quote::None, '\'') => quote = Quote::Single,
            (Quote::Single, '\'') => quote = Quote::None,
            (Quote::None, '"') => quote = Quote::Double,
            (Quote::Double, '"') => quote = Quote::None,
            (Quote::None, character) if character.is_whitespace() || ";|&".contains(character) => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            (_, character) => current.push(character),
        }
    }
    if escaped {
        current.push('\\');
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn resolve_manifest_target(
    root: &Path,
    package: &PackageRecord,
    target: &ManifestTarget,
    sources: &BTreeMap<&str, &IndexedSource>,
) -> BTreeSet<i64> {
    let Some(relative) = normalize_target(root, &package.directory, &target.value) else {
        return BTreeSet::new();
    };
    let mut groups = vec![source_variants(&relative)];
    if target.source_mirror {
        groups.extend(source_mirror_variants(&relative, &package.identity));
    }
    for variants in groups {
        let mut matches = BTreeSet::new();
        for variant in variants {
            if variant.contains(['*', '?']) {
                for (path, source) in sources {
                    if source.default_role() && path_matches(&variant, path) {
                        matches.insert(source.id);
                    }
                }
            } else if let Some(source) = sources.get(variant.as_str())
                && source.default_role()
            {
                matches.insert(source.id);
            }
        }
        if !matches.is_empty() {
            return matches;
        }
    }
    BTreeSet::new()
}

fn normalize_target(root: &Path, package: &Path, target: &str) -> Option<String> {
    let normalized = target.replace('\\', "/");
    if normalized.is_empty()
        || normalized.contains('\0')
        || normalized.contains("//")
        || normalized.contains(['$', '{', '}'])
        || Path::new(&normalized).is_absolute()
    {
        return None;
    }
    let mut components = package
        .strip_prefix(root)
        .ok()?
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str().map(str::to_string),
            _ => None,
        })
        .collect::<Vec<_>>();
    for component in Path::new(&normalized).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                components.pop()?;
            }
            Component::Normal(value) => components.push(value.to_str()?.to_string()),
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(components.join("/"))
}

fn source_variants(path: &str) -> Vec<String> {
    if path.is_empty() {
        return SOURCE_EXTENSIONS
            .iter()
            .map(|extension| format!("index.{extension}"))
            .collect();
    }
    let lower = path.to_ascii_lowercase();
    for (extension, replacements) in [
        (".js", &[".ts", ".tsx", ".js", ".jsx"][..]),
        (".jsx", &[".tsx", ".jsx"][..]),
        (".mjs", &[".mts", ".ts", ".tsx", ".mjs"][..]),
        (".cjs", &[".cts", ".ts", ".tsx", ".cjs"][..]),
    ] {
        if lower.ends_with(extension) {
            let stem = &path[..path.len() - extension.len()];
            return replacements
                .iter()
                .map(|replacement| format!("{stem}{replacement}"))
                .collect();
        }
    }
    if Path::new(path).extension().is_some() {
        return vec![path.to_string()];
    }
    let mut variants = SOURCE_EXTENSIONS
        .iter()
        .map(|extension| format!("{path}.{extension}"))
        .collect::<Vec<_>>();
    variants.extend(
        SOURCE_EXTENSIONS
            .iter()
            .map(|extension| format!("{path}/index.{extension}")),
    );
    variants
}

fn source_mirror_variants(path: &str, package: &str) -> Vec<Vec<String>> {
    let package_components = if package == "." {
        Vec::new()
    } else {
        package.split('/').collect::<Vec<_>>()
    };
    let components = path.split('/').collect::<Vec<_>>();
    let offset = package_components.len();
    let Some(output) = components
        .iter()
        .enumerate()
        .skip(offset)
        .find_map(|(index, component)| OUTPUT_DIRECTORIES.contains(component).then_some(index))
    else {
        return Vec::new();
    };
    let mut tail = &components[output + 1..];
    if tail
        .first()
        .is_some_and(|component| OUTPUT_FLAVORS.contains(component))
    {
        tail = &tail[1..];
    }
    if tail.is_empty() {
        return Vec::new();
    }
    let mut direct = package_components.clone();
    direct.extend(tail);
    let mut source = package_components;
    source.push("src");
    source.extend(tail);
    vec![
        source_variants(&source.join("/")),
        source_variants(&direct.join("/")),
    ]
}

fn path_matches(pattern: &str, value: &str) -> bool {
    if !pattern.contains(['*', '?']) {
        return pattern == value;
    }
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let mut previous = vec![false; value.len() + 1];
    previous[0] = true;
    for character in pattern {
        let mut current = vec![false; value.len() + 1];
        if *character == b'*' {
            current[0] = previous[0];
        }
        for index in 1..=value.len() {
            current[index] = match *character {
                b'*' => previous[index] || current[index - 1],
                b'?' => previous[index - 1],
                literal => previous[index - 1] && literal == value[index - 1],
            };
        }
        previous = current;
    }
    previous[value.len()]
}

fn runtime_reachable(conn: &Connection, seeds: &BTreeSet<i64>) -> Result<BTreeSet<i64>> {
    let mut adjacency = BTreeMap::<i64, Vec<i64>>::new();
    let mut statement = conn.prepare(
        "SELECT from_file, to_file FROM module_edges
         WHERE type_only=0 AND to_file IS NOT NULL
         ORDER BY from_file, to_file",
    )?;
    for row in statement.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))? {
        let (from, to) = row?;
        adjacency.entry(from).or_default().push(to);
    }
    let mut reachable = seeds.clone();
    let mut pending = seeds.iter().copied().collect::<VecDeque<_>>();
    while let Some(file) = pending.pop_front() {
        for target in adjacency.get(&file).into_iter().flatten() {
            if reachable.insert(*target) {
                pending.push_back(*target);
            }
        }
    }
    Ok(reachable)
}

#[expect(
    clippy::too_many_arguments,
    reason = "the policy identity deliberately covers every independently changing input plane"
)]
fn policy_fingerprint(
    include_all: bool,
    sources: &[IndexedSource],
    configured_owners: &BTreeMap<String, ConfiguredOwners>,
    packages: &BTreeMap<String, PackageRecord>,
    counts: &BTreeMap<String, PackageCounts>,
    seed_ids: &BTreeSet<i64>,
    admitted: &BTreeSet<String>,
    observations: &[ManifestObservation],
) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_value(&mut hasher, "jscout-checker-package-gate-v1");
    hash_value(&mut hasher, if include_all { "all" } else { "default" });
    for source in sources {
        hash_value(&mut hasher, &source.path);
        hash_value(&mut hasher, &source.role);
        hash_value(&mut hasher, &source.package);
        if let Some(owners) = configured_owners.get(&source.path) {
            hash_value(&mut hasher, &format!("selected:{}", owners.selected.len()));
            for owner in &owners.selected {
                hash_value(&mut hasher, owner);
            }
            hash_value(&mut hasher, &format!("excluded:{}", owners.excluded.len()));
            for owner in &owners.excluded {
                hash_value(&mut hasher, owner);
            }
            hash_value(
                &mut hasher,
                if owners.tooling_fallback {
                    "tooling-fallback"
                } else {
                    "ordinary-owner"
                },
            );
        } else {
            hash_value(&mut hasher, "unowned");
        }
    }
    for package in packages.values() {
        let count = counts.get(&package.identity).copied().unwrap_or_default();
        hash_value(&mut hasher, &package.identity);
        hash_value(&mut hasher, &count.total_default.to_string());
        hash_value(&mut hasher, &count.unowned_default.to_string());
        hash_value(
            &mut hasher,
            if count.js_first() {
                "js-first"
            } else {
                "ts-first"
            },
        );
    }
    let paths_by_id = sources
        .iter()
        .map(|source| (source.id, source.path.as_str()))
        .collect::<BTreeMap<_, _>>();
    for seed in seed_ids {
        if let Some(path) = paths_by_id.get(seed) {
            hash_value(&mut hasher, path);
        }
    }
    for path in admitted {
        hash_value(&mut hasher, path);
    }
    for observation in observations {
        hash_value(&mut hasher, &observation.display);
        hash_value(&mut hasher, &observation.source_hash);
    }
    hasher.finalize().to_hex().to_string()
}

fn hash_value(hasher: &mut blake3::Hasher, value: &str) {
    hasher.update(value.as_bytes());
    hasher.update(b"\0");
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;
    use rusqlite::{Connection, params};

    use super::*;

    struct Fixture {
        root: tempfile::TempDir,
        conn: Connection,
        next_id: i64,
    }

    impl Fixture {
        fn new(manifest: &str) -> Result<Self> {
            let root = tempfile::tempdir()?;
            fs::write(root.path().join("package.json"), manifest)?;
            let conn = Connection::open_in_memory()?;
            conn.execute_batch(
                "CREATE TABLE files(
                   id INTEGER PRIMARY KEY,
                   path TEXT UNIQUE NOT NULL,
                   role TEXT NOT NULL,
                   origin TEXT NOT NULL
                 );
                 CREATE TABLE module_edges(
                   from_file INTEGER NOT NULL,
                   to_file INTEGER,
                   type_only INTEGER NOT NULL
                 );",
            )?;
            Ok(Self {
                root,
                conn,
                next_id: 1,
            })
        }

        fn file(&mut self, path: &str, role: &str) -> Result<i64> {
            let absolute = self.root.path().join(path);
            if let Some(parent) = absolute.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&absolute, "export const value = 1;\n")?;
            let id = self.next_id;
            self.next_id += 1;
            self.conn.execute(
                "INSERT INTO files(id,path,role,origin) VALUES(?1,?2,?3,'repository')",
                params![id, path, role],
            )?;
            Ok(id)
        }

        fn edge(&self, from: i64, to: i64, type_only: bool) -> Result<()> {
            self.conn.execute(
                "INSERT INTO module_edges(from_file,to_file,type_only) VALUES(?1,?2,?3)",
                params![from, to, i64::from(type_only)],
            )?;
            Ok(())
        }

        fn ownership(&self, configured: &[&str]) -> Result<Vec<FileOwnership>> {
            let configured = configured.iter().copied().collect::<BTreeSet<_>>();
            Ok(inventory_paths(&self.conn)?
                .into_iter()
                .map(|file| FileOwnership {
                    project_ids: if configured.contains(file.as_str()) {
                        vec!["tsconfig.json".into()]
                    } else {
                        vec!["inferred:.#node-esm".into()]
                    },
                    file,
                    excluded_project_ids: Vec::new(),
                    tooling_fallback: false,
                })
                .collect())
        }
    }

    #[test]
    fn strict_majority_admits_js_first_default_files_but_not_tests() -> Result<()> {
        let mut fixture = Fixture::new("{}")?;
        fixture.file("owned.ts", "production")?;
        fixture.file("orphan-a.js", "production")?;
        fixture.file("orphan-b.js", "unknown")?;
        fixture.file("test/orphan.test.js", "test")?;
        let ownership = fixture.ownership(&["owned.ts"])?;

        let plan = evaluate(fixture.root.path(), &fixture.conn, &ownership, false)?;
        assert_eq!(
            plan.admitted_orphans,
            BTreeSet::from(["orphan-a.js".into(), "orphan-b.js".into()])
        );
        plan.validate_fresh()?;
        Ok(())
    }

    #[test]
    fn package_majorities_are_computed_at_each_nested_boundary() -> Result<()> {
        let mut fixture = Fixture::new("{}")?;
        fixture.file("root-owned.ts", "production")?;
        fixture.file("root-a.js", "production")?;
        fixture.file("root-b.js", "unknown")?;
        fixture.file("nested/owned-a.ts", "production")?;
        fixture.file("nested/owned-b.ts", "production")?;
        fixture.file("nested/orphan.js", "production")?;
        fs::write(fixture.root.path().join("nested/package.json"), "{}")?;
        let ownership =
            fixture.ownership(&["root-owned.ts", "nested/owned-a.ts", "nested/owned-b.ts"])?;

        let plan = evaluate(fixture.root.path(), &fixture.conn, &ownership, false)?;
        assert_eq!(
            plan.admitted_orphans,
            BTreeSet::from(["root-a.js".into(), "root-b.js".into()])
        );
        Ok(())
    }

    #[test]
    fn excluded_roles_do_not_change_majorities_or_enter_default_scopes() -> Result<()> {
        for role in ["test", "fixture", "generated", "documentation"] {
            let mut js_first = Fixture::new("{}")?;
            js_first.file("owned.ts", "production")?;
            js_first.file("orphan-a.js", "production")?;
            js_first.file("orphan-b.js", "unknown")?;
            for index in 0..4 {
                js_first.file(&format!("excluded-owned-{index}.js"), role)?;
            }
            let mut configured = vec!["owned.ts".to_string()];
            configured.extend((0..4).map(|index| format!("excluded-owned-{index}.js")));
            let configured = configured.iter().map(String::as_str).collect::<Vec<_>>();
            assert_eq!(
                evaluate(
                    js_first.root.path(),
                    &js_first.conn,
                    &js_first.ownership(&configured)?,
                    false,
                )?
                .admitted_orphans,
                BTreeSet::from(["orphan-a.js".into(), "orphan-b.js".into()]),
                "configured {role} files must not enlarge the denominator",
            );

            let manifest = r#"{"main":"./excluded-entry.js"}"#;
            let mut ts_first = Fixture::new(manifest)?;
            ts_first.file("owned-a.ts", "production")?;
            ts_first.file("owned-b.ts", "production")?;
            ts_first.file("orphan.js", "production")?;
            ts_first.file("excluded-entry.js", role)?;
            for index in 0..3 {
                ts_first.file(&format!("excluded-orphan-{index}.js"), role)?;
            }
            assert!(
                evaluate(
                    ts_first.root.path(),
                    &ts_first.conn,
                    &ts_first.ownership(&["owned-a.ts", "owned-b.ts"])?,
                    false,
                )?
                .admitted_orphans
                .is_empty(),
                "unowned {role} files must not enlarge the numerator or seed reachability",
            );
        }
        Ok(())
    }

    #[test]
    fn tie_is_ts_first_and_all_is_exhaustive() -> Result<()> {
        let mut fixture = Fixture::new("{}")?;
        fixture.file("owned.ts", "production")?;
        fixture.file("orphan.js", "production")?;
        fixture.file("test/orphan.test.js", "test")?;
        let ownership = fixture.ownership(&["owned.ts"])?;

        assert!(
            evaluate(fixture.root.path(), &fixture.conn, &ownership, false)?
                .admitted_orphans
                .is_empty()
        );
        assert_eq!(
            evaluate(fixture.root.path(), &fixture.conn, &ownership, true)?.admitted_orphans,
            BTreeSet::from(["orphan.js".into(), "test/orphan.test.js".into()])
        );
        Ok(())
    }

    #[test]
    fn all_admits_every_orphan_role_across_packages_but_not_excluded_config_owners() -> Result<()> {
        let mut fixture = Fixture::new("{}")?;
        fs::create_dir_all(fixture.root.path().join("nested"))?;
        fs::write(fixture.root.path().join("nested/package.json"), "{}")?;
        let mut expected = BTreeSet::new();
        for (index, role) in [
            "production",
            "unknown",
            "test",
            "fixture",
            "generated",
            "documentation",
        ]
        .into_iter()
        .enumerate()
        {
            for prefix in ["root", "nested"] {
                let path = if prefix == "root" {
                    format!("orphan-{index}.js")
                } else {
                    format!("nested/orphan-{index}.js")
                };
                fixture.file(&path, role)?;
                expected.insert(path);
            }
        }
        fixture.file("excluded-owner.ts", "production")?;
        let mut ownership = fixture.ownership(&[])?;
        let excluded = ownership
            .iter_mut()
            .find(|entry| entry.file == "excluded-owner.ts")
            .expect("excluded configured owner");
        excluded.excluded_project_ids = vec!["tsconfig.tooling.json".into()];

        let plan = evaluate(fixture.root.path(), &fixture.conn, &ownership, true)?;
        assert_eq!(plan.admitted_orphans, expected);
        Ok(())
    }

    #[test]
    fn runtime_bfs_admits_multihop_but_not_type_only_or_unreachable_files() -> Result<()> {
        let mut fixture = Fixture::new(r#"{"main":"./src/entry.ts"}"#)?;
        let entry = fixture.file("src/entry.ts", "production")?;
        for path in ["src/owned-a.ts", "src/owned-b.ts", "src/owned-c.ts"] {
            fixture.file(path, "production")?;
        }
        let reachable = fixture.file("src/reachable.ts", "production")?;
        let deep = fixture.file("src/deep.ts", "production")?;
        let type_only = fixture.file("src/type-only.ts", "production")?;
        fixture.file("src/unreachable.ts", "production")?;
        fixture.edge(entry, reachable, false)?;
        fixture.edge(reachable, deep, false)?;
        fixture.edge(deep, reachable, false)?;
        fixture.edge(entry, type_only, true)?;
        let ownership = fixture.ownership(&[
            "src/entry.ts",
            "src/owned-a.ts",
            "src/owned-b.ts",
            "src/owned-c.ts",
        ])?;

        let plan = evaluate(fixture.root.path(), &fixture.conn, &ownership, false)?;
        assert_eq!(
            plan.admitted_orphans,
            BTreeSet::from(["src/deep.ts".into(), "src/reachable.ts".into()])
        );
        Ok(())
    }

    #[test]
    fn runtime_reachability_crosses_package_boundaries() -> Result<()> {
        let mut fixture = Fixture::new(r#"{"main":"./entry.ts"}"#)?;
        let entry = fixture.file("entry.ts", "production")?;
        fixture.file("nested/owned.ts", "production")?;
        let nested_orphan = fixture.file("nested/orphan.ts", "production")?;
        fs::write(fixture.root.path().join("nested/package.json"), "{}")?;
        fixture.edge(entry, nested_orphan, false)?;
        let ownership = fixture.ownership(&["entry.ts", "nested/owned.ts"])?;

        let plan = evaluate(fixture.root.path(), &fixture.conn, &ownership, false)?;
        assert_eq!(
            plan.admitted_orphans,
            BTreeSet::from(["nested/orphan.ts".into()])
        );
        Ok(())
    }

    #[test]
    fn main_exports_bin_and_script_targets_seed_ts_first_packages() -> Result<()> {
        let manifest = serde_json::json!({
            "main": "./dist/main.js",
            "exports": {
                "./feature": {
                    "import": "./feature.mjs",
                    "require": "./feature.cjs",
                    "browser": "./feature.browser.js",
                    "types": "./feature.d.ts"
                }
            },
            "bin": { "tool": "./bin/tool.js" },
            "scripts": { "task": "cross-env MODE=test node './scripts/task.mjs'" }
        });
        let mut fixture = Fixture::new(&manifest.to_string())?;
        for path in [
            "src/owned-a.ts",
            "src/owned-b.ts",
            "src/owned-c.ts",
            "src/owned-d.ts",
            "src/owned-e.ts",
            "src/owned-f.ts",
        ] {
            fixture.file(path, "production")?;
        }
        fixture.file("src/main.ts", "production")?;
        fixture.file("feature.mjs", "production")?;
        fixture.file("feature.cjs", "production")?;
        fixture.file("feature.browser.js", "production")?;
        fixture.file("bin/tool.js", "production")?;
        fixture.file("scripts/task.mjs", "production")?;
        let ownership = fixture.ownership(&[
            "src/owned-a.ts",
            "src/owned-b.ts",
            "src/owned-c.ts",
            "src/owned-d.ts",
            "src/owned-e.ts",
            "src/owned-f.ts",
        ])?;

        let plan = evaluate(fixture.root.path(), &fixture.conn, &ownership, false)?;
        assert_eq!(
            plan.admitted_orphans,
            BTreeSet::from([
                "bin/tool.js".into(),
                "feature.browser.js".into(),
                "feature.cjs".into(),
                "feature.mjs".into(),
                "scripts/task.mjs".into(),
                "src/main.ts".into(),
            ])
        );
        Ok(())
    }

    #[test]
    fn package_root_main_targets_seed_root_and_nested_indexes() -> Result<()> {
        let mut fixture = Fixture::new(r#"{"main":"."}"#)?;
        fixture.file("owned-a.ts", "production")?;
        fixture.file("owned-b.ts", "production")?;
        fixture.file("index.ts", "production")?;
        fixture.file("nested/owned-a.ts", "production")?;
        fixture.file("nested/owned-b.ts", "production")?;
        fixture.file("nested/index.ts", "production")?;
        fs::write(
            fixture.root.path().join("nested/package.json"),
            r#"{"main":"./"}"#,
        )?;
        let ownership = fixture.ownership(&[
            "owned-a.ts",
            "owned-b.ts",
            "nested/owned-a.ts",
            "nested/owned-b.ts",
        ])?;

        assert_eq!(
            evaluate(fixture.root.path(), &fixture.conn, &ownership, false)?.admitted_orphans,
            BTreeSet::from(["index.ts".into(), "nested/index.ts".into()])
        );
        Ok(())
    }

    #[test]
    fn excluded_config_owner_counts_as_owned_and_ownership_must_cover_inventory() -> Result<()> {
        let mut fixture = Fixture::new("{}")?;
        fixture.file("owned.ts", "production")?;
        fixture.file("orphan.js", "production")?;
        let mut ownership = fixture.ownership(&[])?;
        let owned = ownership
            .iter_mut()
            .find(|entry| entry.file == "owned.ts")
            .expect("owned file");
        owned.excluded_project_ids = vec!["tsconfig.tooling.json".into()];
        assert!(has_configured_owner(owned));
        assert!(
            evaluate(fixture.root.path(), &fixture.conn, &ownership, false)?
                .admitted_orphans
                .is_empty()
        );

        ownership.pop();
        assert!(
            evaluate(fixture.root.path(), &fixture.conn, &ownership, false)
                .expect_err("missing ownership")
                .to_string()
                .contains("omitted first-party ownership")
        );
        Ok(())
    }

    #[test]
    fn fingerprint_records_every_configured_owner_not_only_owned_status() -> Result<()> {
        let mut fixture = Fixture::new("{}")?;
        fixture.file("owned.ts", "production")?;
        fixture.file("orphan.js", "production")?;
        let mut ownership = fixture.ownership(&["owned.ts"])?;
        let first = evaluate(fixture.root.path(), &fixture.conn, &ownership, false)?;

        ownership
            .iter_mut()
            .find(|entry| entry.file == "owned.ts")
            .expect("owned file")
            .excluded_project_ids
            .push("tsconfig.secondary.json".into());
        let second = evaluate(fixture.root.path(), &fixture.conn, &ownership, false)?;

        assert_eq!(first.admitted_orphans, second.admitted_orphans);
        assert_ne!(first.fingerprint, second.fingerprint);
        let owned = ownership
            .iter_mut()
            .find(|entry| entry.file == "owned.ts")
            .expect("owned file");
        owned.excluded_project_ids.clear();
        owned.project_ids.push("tsconfig.secondary.json".into());
        let promoted = evaluate(fixture.root.path(), &fixture.conn, &ownership, false)?;
        assert_eq!(second.admitted_orphans, promoted.admitted_orphans);
        assert_ne!(second.fingerprint, promoted.fingerprint);
        Ok(())
    }

    #[test]
    fn nested_and_new_package_boundaries_are_freshness_inputs() -> Result<()> {
        let mut fixture = Fixture::new("{}")?;
        fixture.file("src/main.js", "production")?;
        let ownership = fixture.ownership(&[])?;
        let plan = evaluate(fixture.root.path(), &fixture.conn, &ownership, false)?;
        fs::write(fixture.root.path().join("src/package.json"), "{")?;
        assert!(
            plan.validate_fresh()
                .expect_err("new boundary")
                .to_string()
                .contains("appeared")
        );

        let repaired = evaluate(fixture.root.path(), &fixture.conn, &ownership, false)?;
        assert!(repaired.admitted_orphans.contains("src/main.js"));
        fs::write(
            fixture.root.path().join("src/package.json"),
            r#"{"main":"main.js"}"#,
        )?;
        assert!(
            repaired
                .validate_fresh()
                .expect_err("changed malformed manifest")
                .to_string()
                .contains("changed")
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn manifest_symlink_and_absence_races_fail_closed() -> Result<()> {
        use std::os::unix::fs::symlink;

        let mut retargeted = Fixture::new("{}")?;
        fs::create_dir_all(retargeted.root.path().join("manifests"))?;
        fs::write(retargeted.root.path().join("manifests/a.json"), "{}")?;
        fs::write(
            retargeted.root.path().join("manifests/b.json"),
            r#"{"main":"main.js"}"#,
        )?;
        fs::remove_file(retargeted.root.path().join("package.json"))?;
        symlink(
            retargeted.root.path().join("manifests/a.json"),
            retargeted.root.path().join("package.json"),
        )?;
        retargeted.file("main.js", "production")?;
        let ownership = retargeted.ownership(&[])?;
        let plan = evaluate(retargeted.root.path(), &retargeted.conn, &ownership, false)?;
        fs::remove_file(retargeted.root.path().join("package.json"))?;
        symlink(
            retargeted.root.path().join("manifests/b.json"),
            retargeted.root.path().join("package.json"),
        )?;
        assert!(
            plan.validate_fresh()
                .expect_err("manifest retarget")
                .to_string()
                .contains("changed")
        );

        let mut absent = Fixture::new("{}")?;
        absent.file("src/main.js", "production")?;
        let ownership = absent.ownership(&[])?;
        let plan = evaluate(absent.root.path(), &absent.conn, &ownership, false)?;
        symlink(
            absent.root.path().join("missing-package.json"),
            absent.root.path().join("src/package.json"),
        )?;
        assert!(
            plan.validate_fresh()
                .expect_err("dangling symlink replaced an absent probe")
                .to_string()
                .contains("appeared")
        );
        assert!(
            evaluate(absent.root.path(), &absent.conn, &ownership, false)
                .expect_err("dangling manifest boundary")
                .to_string()
                .contains("could not follow package boundary")
        );

        let mut root_absent = Fixture::new("{}")?;
        fs::remove_file(root_absent.root.path().join("package.json"))?;
        root_absent.file("main.js", "production")?;
        let ownership = root_absent.ownership(&[])?;
        let plan = evaluate(
            root_absent.root.path(),
            &root_absent.conn,
            &ownership,
            false,
        )?;
        fs::write(root_absent.root.path().join("package.json"), "{}")?;
        assert!(
            plan.validate_fresh()
                .expect_err("root boundary appeared")
                .to_string()
                .contains("appeared")
        );
        Ok(())
    }

    #[test]
    fn fingerprint_is_order_independent_and_changes_with_manifest_policy() -> Result<()> {
        let mut fixture = Fixture::new(r#"{"scripts":{"start":"node a.js"}}"#)?;
        fixture.file("owned.ts", "production")?;
        fixture.file("a.js", "production")?;
        let ownership = fixture.ownership(&["owned.ts"])?;
        let first = evaluate(fixture.root.path(), &fixture.conn, &ownership, false)?;
        let mut reversed = ownership.clone();
        reversed.reverse();
        let reordered = evaluate(fixture.root.path(), &fixture.conn, &reversed, false)?;
        assert_eq!(first.fingerprint, reordered.fingerprint);

        fs::write(
            fixture.root.path().join("package.json"),
            r#"{"scripts":{"start":"node missing.js"}}"#,
        )?;
        let changed = evaluate(fixture.root.path(), &fixture.conn, &ownership, false)?;
        assert_ne!(first.fingerprint, changed.fingerprint);
        Ok(())
    }
}
