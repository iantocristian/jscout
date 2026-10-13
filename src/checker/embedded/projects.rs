use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tsr_core::{
    CompilerOptions, JsxEmit, ModuleKind, ModuleResolutionKind, ScriptTarget, Tristate,
};
use tsr_jsstring::JsString;
use tsr_tsoptions::{ConfigValue, ParsedCommandLine};
use tsr_vfs::FileSystem;

use super::host::{InputFs, inputs_fingerprint};
use super::{Request, error, path_text, relative};
use crate::checker::enrich::INFERRED_ROOT_CAP;
use crate::checker::process::CheckerError;
use crate::checker::protocol::{ConfigurationProblem, FileOwnership, ProjectSummary};

#[derive(Clone)]
pub struct Project {
    pub summary: ProjectSummary,
    pub config: ParsedCommandLine,
    pub host: Arc<InputFs>,
    pub roots: BTreeSet<PathBuf>,
}

pub struct Discovery {
    pub projects: BTreeMap<String, Project>,
    pub owners: BTreeMap<PathBuf, Vec<String>>,
    pub problems: Vec<ConfigurationProblem>,
}

fn discover_configs(root: &Path, request: &Request) -> Result<Vec<PathBuf>, CheckerError> {
    let mut configs = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        request.check()?;
        // Match the sidecar's best-effort discovery: inaccessible or removed
        // subtrees must not prevent checking the readable projects. Parsing a
        // discovered config still reports its own read/configuration failures.
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            request.check()?;
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if kind.is_dir() {
                if ![".git", ".jscout", ".worktrees", "node_modules"].contains(&name.as_ref()) {
                    pending.push(entry.path());
                }
            } else if kind.is_file()
                && (name == "tsconfig.json"
                    || name.starts_with("tsconfig.")
                        && name.ends_with(".json")
                        && name.len() > "tsconfig..json".len())
            {
                configs.push(entry.path());
            }
        }
    }
    configs.sort();
    Ok(configs)
}

fn diagnostic_message(diagnostic: &tsr_ast::Diagnostic) -> String {
    let mut text = if diagnostic.message_text.is_empty() {
        diagnostic.message.map_or_else(
            || String::from_utf8_lossy(diagnostic.message_key.as_bytes()).into_owned(),
            |message| message.text.into(),
        )
    } else {
        String::from_utf8_lossy(diagnostic.message_text.as_bytes()).into_owned()
    };
    for (index, argument) in diagnostic.message_args.iter().enumerate() {
        text = text.replace(
            &format!("{{{index}}}"),
            &String::from_utf8_lossy(argument.as_bytes()),
        );
    }
    format!("TS{}: {text}", diagnostic.code)
}

pub fn discover(root: &Path, request: &Arc<Request>) -> Result<Discovery, CheckerError> {
    let shared: Arc<dyn FileSystem> = Arc::new(tsr_vfs::cached::CachedFs::new(Arc::new(
        tsr_bundled::BundledFs::new(tsr_vfs::os::shared_fs()),
    )));
    let mut projects = BTreeMap::new();
    let mut inherited = BTreeSet::new();
    let mut implicit = BTreeSet::new();
    let mut problems = Vec::new();
    for path in discover_configs(root, request)? {
        request.check()?;
        let id = relative(root, &path)?;
        let host = Arc::new(InputFs::new(shared.clone(), request.clone()));
        let config_host = tsr_compiler::CompilerConfigHost::new_live(
            host.clone(),
            JsString::from_bytes(path_text(root)?.as_bytes()),
        );
        let result = tsr_tsoptions::get_parsed_command_line_of_config_file(
            path_text(&path)?.as_bytes(),
            &CompilerOptions::default(),
            &ConfigValue::Null,
            &config_host,
        );
        request.check()?;
        let parsed = match result {
            Ok(result) => {
                for diagnostic in &result.read_errors {
                    problems.push(ConfigurationProblem {
                        project_id: id.clone(),
                        code: "config".into(),
                        message: diagnostic_message(diagnostic),
                    });
                }
                result.command_line
            }
            Err(failure) => {
                problems.push(ConfigurationProblem {
                    project_id: id.clone(),
                    code: "config".into(),
                    message: failure.to_string(),
                });
                None
            }
        };
        let Some(mut config) = parsed else { continue };
        // `raw` includes inherited files/include. Ownership policy must use
        // the properties written in this config, before inheritance.
        let own = config
            .config_file
            .as_ref()
            .map_or(ConfigValue::Null, |source| {
                tsr_tsoptions::convert_to_object(source).0
            });
        if ![b"files".as_slice(), b"include", b"references"]
            .iter()
            .any(|key| own.get(key).is_some())
        {
            implicit.insert(path.clone());
        }
        for dependency in &config.config_dependencies {
            let source = dependency
                .file
                .view()
                .source_file(dependency.root)
                .map_err(|e| error("config", e.to_string()))?;
            let name = std::str::from_utf8(source.file_name())
                .map_err(|_| error("config", "non-UTF-8 inherited configuration"))?;
            if Path::new(name) != path {
                inherited.insert(PathBuf::from(name));
            }
        }
        let diagnostics = config.config_file_parsing_diagnostics();
        if !diagnostics.is_empty() {
            for diagnostic in &diagnostics {
                problems.push(ConfigurationProblem {
                    project_id: id.clone(),
                    code: "config".into(),
                    message: diagnostic_message(diagnostic),
                });
            }
            continue;
        }
        let mut roots = BTreeSet::new();
        for name in &config.root_file_names {
            let name = std::str::from_utf8(name.as_bytes())
                .map_err(|_| error("config", "non-UTF-8 configured source"))?;
            let canonical = host
                .realpath(name.as_bytes())
                .map_err(|e| error("config", e.to_string()))?;
            roots.insert(PathBuf::from(
                std::str::from_utf8(canonical.as_bytes())
                    .map_err(|_| error("config", "non-UTF-8 source path"))?,
            ));
        }
        config.root_file_names = roots
            .iter()
            .map(|p| path_text(p).map(|p| JsString::from_bytes(p.as_bytes())))
            .collect::<Result<_, _>>()?;
        let (purpose, purpose_reasons) = purpose(root, &path, &config, &own, &host)?;
        let summary = summary(id.clone(), &config, &host, &roots, purpose, purpose_reasons)?;
        projects.insert(
            id,
            Project {
                summary,
                config,
                host,
                roots,
            },
        );
    }
    projects
        .retain(|id, _| !inherited.contains(&root.join(id)) || !implicit.contains(&root.join(id)));
    problems.retain(|problem| {
        !inherited.contains(&root.join(&problem.project_id))
            || !implicit.contains(&root.join(&problem.project_id))
    });
    let mut owners: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for (id, project) in &projects {
        for file in &project.roots {
            owners.entry(file.clone()).or_default().push(id.clone());
        }
    }
    Ok(Discovery {
        projects,
        owners,
        problems,
    })
}

fn summary(
    project_id: String,
    config: &ParsedCommandLine,
    host: &InputFs,
    roots: &BTreeSet<PathBuf>,
    purpose: String,
    purpose_reasons: Vec<String>,
) -> Result<ProjectSummary, CheckerError> {
    let mut membership = blake3::Hasher::new();
    for file in roots {
        membership.update(path_text(file)?.as_bytes());
        membership.update(b"\0");
    }
    let options = tsr_tsoptions::options_json::marshal_compiler_options(&config.options)
        .map_err(|e| error("config", e.to_string()))?;
    let mut fingerprint = blake3::Hasher::new();
    fingerprint.update(inputs_fingerprint(&host.inputs()?).as_bytes());
    fingerprint.update(&options);
    Ok(ProjectSummary {
        project_id,
        file_count: roots.len(),
        purpose,
        purpose_reasons,
        membership_fingerprint: membership.finalize().to_hex().to_string(),
        config_fingerprint: fingerprint.finalize().to_hex().to_string(),
    })
}

fn lint_token(value: &str, separators: &[char]) -> bool {
    value
        .split(separators)
        .any(|part| part.eq_ignore_ascii_case("lint") || part.eq_ignore_ascii_case("eslint"))
}

fn command_references(command: &str, config: &str) -> bool {
    let command = command.replace('\\', "/");
    [config.to_string(), format!("./{config}")]
        .iter()
        .any(|needle| {
            command.match_indices(needle).any(|(start, _)| {
                let before = command[..start].chars().next_back();
                let after = command[start + needle.len()..].chars().next();
                before.is_none_or(|ch| ch.is_whitespace() || "\"'=".contains(ch))
                    && after.is_none_or(|ch| ch.is_whitespace() || "\"';&|)".contains(ch))
            })
        })
}

fn purpose(
    root: &Path,
    path: &Path,
    config: &ParsedCommandLine,
    own: &ConfigValue,
    host: &InputFs,
) -> Result<(String, Vec<String>), CheckerError> {
    let mut reasons = Vec::new();
    if lint_token(
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default(),
        &['.', '_', '-'],
    ) {
        reasons.push("tooling-filename".into());
    }
    if let Some(value) = own.get(b"extends") {
        let values = value
            .as_array()
            .unwrap_or_else(|| std::slice::from_ref(value));
        if values
            .iter()
            .filter_map(ConfigValue::as_string)
            .any(|value| {
                lint_token(
                    &String::from_utf8_lossy(value.as_bytes()),
                    &['.', '_', '/', '@', '-'],
                )
            })
        {
            reasons.push("tooling-extends".into());
        }
    }
    if config.options.no_emit.is_true() {
        let package = nearest_package(root, path, host)?;
        let relative = relative(&package.directory, path)?;
        let mut scripts = package
            .value
            .get("scripts")
            .and_then(serde_json::Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(name, value)| {
                lint_token(name, &[':', '.', '_', '-'])
                    && value
                        .as_str()
                        .is_some_and(|command| command_references(command, &relative))
            })
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        scripts.sort();
        if !scripts.is_empty() {
            reasons.push(format!("tooling-script:{}", scripts.join(",")));
        }
    }
    Ok((
        if reasons.is_empty() {
            "general"
        } else {
            "tooling"
        }
        .into(),
        reasons,
    ))
}

struct Package {
    directory: PathBuf,
    value: serde_json::Value,
}

fn nearest_package(root: &Path, file: &Path, host: &InputFs) -> Result<Package, CheckerError> {
    let mut directory = file.parent().unwrap_or(root);
    while directory.starts_with(root) {
        let manifest = directory.join("package.json");
        if let Some(file) = host
            .read_file(path_text(&manifest)?.as_bytes())
            .map_err(|e| error("config", e.to_string()))?
        {
            return Ok(Package {
                directory: directory.to_path_buf(),
                value: serde_json::from_slice(&file.raw).unwrap_or_default(),
            });
        }
        if directory == root {
            break;
        }
        directory = directory.parent().unwrap_or(root);
    }
    Ok(Package {
        directory: root.to_path_buf(),
        value: serde_json::Value::Null,
    })
}

fn family(path: &Path, package: &Package) -> &'static str {
    match path
        .extension()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "jsx" | "tsx" => "bundler-jsx",
        "mjs" | "mts" => "node-esm",
        "cjs" | "cts" => "node-cjs",
        _ if package
            .value
            .get("type")
            .and_then(serde_json::Value::as_str)
            == Some("module") =>
        {
            "node-esm"
        }
        _ => "node-cjs",
    }
}

fn inferred(
    root: &Path,
    id: String,
    files: Vec<PathBuf>,
    request: &Arc<Request>,
) -> Result<Project, CheckerError> {
    if files.is_empty() || files.len() > INFERRED_ROOT_CAP {
        return Err(error("protocol", "invalid inferred project root count"));
    }
    let host = Arc::new(InputFs::os(request.clone()));
    let (package_id, compiler_family) = id
        .strip_prefix("inferred:")
        .and_then(|id| id.rsplit_once('#'))
        .ok_or_else(|| error("project_not_found", "invalid inferred project ID"))?;
    let compiler_family = compiler_family.split('/').next().unwrap_or_default();
    if !["node-esm", "node-cjs", "bundler-jsx"].contains(&compiler_family) {
        return Err(error("project_not_found", "invalid compiler family"));
    }
    let roots = files.into_iter().collect::<BTreeSet<_>>();
    for file in &roots {
        let package = nearest_package(root, file, &host)?;
        if relative(root, &package.directory)? != package_id
            || family(file, &package) != compiler_family
        {
            return Err(error(
                "project_mismatch",
                format!("{} does not belong to {id}", file.display()),
            ));
        }
    }
    let options = CompilerOptions {
        allow_js: Tristate::TRUE,
        check_js: Tristate::FALSE,
        jsx: JsxEmit::PRESERVE,
        target: ScriptTarget::ESNEXT,
        module: if compiler_family == "bundler-jsx" {
            ModuleKind::ESNEXT
        } else {
            ModuleKind::NODE_NEXT
        },
        module_resolution: if compiler_family == "bundler-jsx" {
            ModuleResolutionKind::BUNDLER
        } else {
            ModuleResolutionKind::NODE_NEXT
        },
        ..Default::default()
    };
    let config = ParsedCommandLine::new(
        options,
        roots
            .iter()
            .map(|file| path_text(file).map(|p| JsString::from_bytes(p.as_bytes())))
            .collect::<Result<_, _>>()?,
    );
    let summary = summary(
        id.clone(),
        &config,
        &host,
        &roots,
        "inferred".into(),
        vec![
            "no-configured-owner".into(),
            format!("compiler-family:{compiler_family}"),
        ],
    )?;
    Ok(Project {
        summary,
        config,
        host,
        roots,
    })
}

pub fn inferred_from_request(
    root: &Path,
    id: String,
    files: Vec<String>,
    request: &Arc<Request>,
) -> Result<Project, CheckerError> {
    let mut unique = BTreeSet::new();
    for file in files {
        if !unique.insert(super::query_file(root, &file)?) {
            return Err(error("protocol", "duplicate inferred root"));
        }
    }
    inferred(root, id, unique.into_iter().collect(), request)
}

fn split_evenly(files: Vec<PathBuf>) -> Vec<Vec<PathBuf>> {
    let parts = files.len().div_ceil(INFERRED_ROOT_CAP);
    if parts == 0 {
        return Vec::new();
    }
    let minimum = files.len() / parts;
    let extras = files.len() % parts;
    let mut values = files.into_iter();
    (0..parts)
        .map(|index| {
            values
                .by_ref()
                .take(minimum + usize::from(index < extras))
                .collect()
        })
        .collect()
}

fn directory_units(directory: &Path, files: Vec<PathBuf>) -> Vec<Vec<PathBuf>> {
    let mut direct = Vec::new();
    let mut children: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for file in files {
        let relative = file.strip_prefix(directory).expect("group member");
        if relative.components().count() == 1 {
            direct.push(file);
        } else {
            children
                .entry(directory.join(relative.components().next().expect("child").as_os_str()))
                .or_default()
                .push(file);
        }
    }
    let mut units = split_evenly(direct);
    for (child, files) in children {
        if files.len() <= INFERRED_ROOT_CAP {
            units.push(files);
        } else {
            units.extend(directory_units(&child, files));
        }
    }
    units
}

fn part_name(top: &str, index: usize) -> String {
    let mut name = String::new();
    if top.is_empty() {
        name.push_str("@root");
    } else {
        use std::fmt::Write;
        for byte in top.bytes() {
            if byte.is_ascii_alphanumeric() || b"-_.!*'()".contains(&byte) {
                name.push(char::from(byte));
            } else {
                write!(name, "%{byte:02X}").expect("string write");
            }
        }
    }
    format!("{name}~{}", index + 1)
}

pub fn plan(
    root: &Path,
    discovery: &mut Discovery,
    files: &[String],
    request: &Arc<Request>,
) -> Result<Vec<FileOwnership>, CheckerError> {
    discovery
        .projects
        .retain(|_, project| project.summary.purpose != "inferred");
    let package_host = InputFs::os(request.clone());
    let mut ownership = Vec::new();
    let mut groups: BTreeMap<(PathBuf, &'static str), Vec<PathBuf>> = BTreeMap::new();
    let mut canonical = BTreeMap::new();
    for file in files {
        request.check()?;
        let path = super::query_file(root, file)?;
        let owners = discovery.owners.get(&path).cloned().unwrap_or_default();
        let primary = owners
            .iter()
            .filter(|id| discovery.projects[*id].summary.purpose != "tooling")
            .cloned()
            .collect::<Vec<_>>();
        let (project_ids, excluded_project_ids, tooling_fallback) = if owners.is_empty() {
            let package = nearest_package(root, &path, &package_host)?;
            let compiler_family = family(&path, &package);
            groups
                .entry((package.directory, compiler_family))
                .or_default()
                .push(path.clone());
            (Vec::new(), Vec::new(), false)
        } else if primary.is_empty() {
            (owners, Vec::new(), true)
        } else {
            let excluded = owners
                .into_iter()
                .filter(|id| !primary.contains(id))
                .collect();
            (primary, excluded, false)
        };
        canonical.insert(file.clone(), path);
        ownership.push(FileOwnership {
            file: file.clone(),
            project_ids,
            excluded_project_ids,
            tooling_fallback,
        });
    }
    let mut inferred_owners = BTreeMap::new();
    for ((directory, compiler_family), mut files) in groups {
        files.sort();
        files.dedup();
        let prefix = format!("inferred:{}#{compiler_family}", relative(root, &directory)?);
        let mut scopes = Vec::new();
        if files.len() <= INFERRED_ROOT_CAP {
            scopes.push((prefix, files));
        } else {
            let mut by_top: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
            for file in files {
                let path = file.strip_prefix(&directory).expect("package member");
                let top = if path.components().count() == 1 {
                    String::new()
                } else {
                    path.components()
                        .next()
                        .expect("top")
                        .as_os_str()
                        .to_str()
                        .ok_or_else(|| error("unsupported_path", "non-UTF-8 directory"))?
                        .into()
                };
                by_top.entry(top).or_default().push(file);
            }
            for (top, files) in by_top {
                let mut bins = vec![Vec::new()];
                for unit in directory_units(&directory.join(&top), files) {
                    if !bins.last().expect("bin").is_empty()
                        && bins.last().expect("bin").len() + unit.len() > INFERRED_ROOT_CAP
                    {
                        bins.push(Vec::new());
                    }
                    bins.last_mut().expect("bin").extend(unit);
                }
                for (index, files) in bins.into_iter().enumerate() {
                    scopes.push((format!("{prefix}/{}", part_name(&top, index)), files));
                }
            }
        }
        for (id, files) in scopes {
            for file in &files {
                inferred_owners.insert(file.clone(), id.clone());
            }
            discovery
                .projects
                .insert(id.clone(), inferred(root, id, files, request)?);
        }
    }
    for entry in &mut ownership {
        if entry.project_ids.is_empty() {
            entry
                .project_ids
                .push(inferred_owners[&canonical[&entry.file]].clone());
        }
    }
    Ok(ownership)
}
