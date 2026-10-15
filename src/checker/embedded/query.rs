use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tsr_ast::{NodeId, SyntaxKind as K};
use tsr_checker::{Operation, type_flags, type_format_flags};
use tsr_embed::{FileCache, Program, ProgramOptions, Session};
use tsr_jsstring::JsString;
use tsr_printer::emit_resolver::DeclarationEmitResolver;
use tsr_vfs::FileSystem;

use super::host::{InputFs, inputs_fingerprint};
use super::projects::Project;
use super::{Request, error, identity, path_text, query_file, relative};
use crate::checker::process::CheckerError;
use crate::checker::protocol::{
    CheckerInputFile, DeclarationSite, MemberProjectResult, MemberQuery, ProjectAnswer,
};

type Spans = [i64; 6];
type Calls = HashMap<Spans, (NodeId, NodeId)>;

pub struct BuiltProject {
    pub project: Project,
    pub session: Session,
    pub fingerprint: String,
    pub inputs: Vec<CheckerInputFile>,
    calls: HashMap<PathBuf, Calls>,
}

fn compiler_error(error: impl std::fmt::Display) -> CheckerError {
    super::error("checker_failure", error.to_string())
}

pub fn build(
    root: &Path,
    project: Project,
    request: &Arc<Request>,
) -> Result<BuiltProject, CheckerError> {
    request
        .diagnostics
        .project(&project.summary.project_id, "program-load");
    request.check()?;
    project.host.set_request(request.clone());
    let counters = tsr_arena::Counters::new();
    let program = Program::load_live(
        ProgramOptions {
            config: project.config.clone(),
            host: project.host.clone(),
            current_directory: JsString::from_bytes(path_text(root)?.as_bytes()),
            default_library_path: JsString::from_bytes(tsr_bundled::LIB_PATH),
            skip_module_resolution: false,
            single_threaded: tsr_core::Tristate::TRUE,
        },
        &mut FileCache::new(),
        &counters,
    )
    .map_err(compiler_error)?;
    request.check()?;
    let session = Session::from_program(Arc::new(program), &counters);
    request.diagnostics.operation("checker-initialize");
    session.checker().map_err(compiler_error)?;
    // Declaration paths are part of the answer. Observe their symlinks before
    // the first batch so later queries cannot expand the input identity.
    request.diagnostics.operation("declaration-source-paths");
    for file in session.program().files() {
        request.check()?;
        let source = file.bound().view().source_file().map_err(compiler_error)?;
        if !tsr_bundled::is_bundled(source.file_name()) {
            project
                .host
                .realpath(source.file_name())
                .map_err(compiler_error)?;
        }
    }
    request.check()?;
    request.diagnostics.operation("input-manifest");
    let inputs = project.host.inputs()?;
    let fingerprint = fingerprint(&project, &inputs)?;
    Ok(BuiltProject {
        project,
        session,
        inputs,
        fingerprint,
        calls: HashMap::new(),
    })
}

fn fingerprint(project: &Project, inputs: &[CheckerInputFile]) -> Result<String, CheckerError> {
    let mut hash = blake3::Hasher::new();
    for text in [
        identity().version,
        project.summary.project_id.clone(),
        project.summary.membership_fingerprint.clone(),
        project.summary.config_fingerprint.clone(),
        inputs_fingerprint(inputs),
    ] {
        hash.update(&(text.len() as u64).to_le_bytes());
        hash.update(text.as_bytes());
    }
    hash.update(
        &tsr_tsoptions::options_json::marshal_compiler_options(&project.config.options)
            .map_err(compiler_error)?,
    );
    Ok(hash.finalize().to_hex().to_string())
}

fn source_prefix(raw: &[u8], loaded: &[u8]) -> Result<i64, CheckerError> {
    if raw == loaded {
        Ok(0)
    } else if raw.strip_prefix(b"\xef\xbb\xbf") == Some(loaded) {
        Ok(3)
    } else {
        Err(error(
            "unsupported_encoding",
            "indexed byte spans require UTF-8 source",
        ))
    }
}

fn calls(
    file: &tsr_embed::ProgramFile,
    host: &InputFs,
    path: &Path,
    request: &Request,
) -> Result<Calls, CheckerError> {
    let view = file.bound().view().ast();
    let source = file.bound().view().source_file().map_err(compiler_error)?;
    let raw = host.raw(path)?;
    let prefix = source_prefix(&raw, source.text().as_bytes())?;
    let mut jsdoc = tsr_parser::ParserJsDocProvider::default();
    let mut navigator = tsr_astnav::Navigator::new(view, file.source(), &mut jsdoc);
    let mut pending = vec![file.source()];
    let mut result = HashMap::new();
    while let Some(id) = pending.pop() {
        request.check()?;
        let node = view.node(id).map_err(compiler_error)?;
        if node.kind().known() == Some(K::CallExpression)
            && let Some(member) = node.expression()
            && view.node(member).map_err(compiler_error)?.kind().known()
                == Some(K::PropertyAccessExpression)
        {
            let access = view.node(member).map_err(compiler_error)?;
            let receiver = access
                .expression()
                .ok_or_else(|| error("checker_failure", "member has no receiver"))?;
            let property = access
                .name()
                .ok_or_else(|| error("checker_failure", "member has no name"))?;
            let mut spans = [0; 6];
            for (index, node) in [id, receiver, property].into_iter().enumerate() {
                spans[index * 2] = navigator
                    .get_start_of_node(node, false)
                    .map_err(compiler_error)?
                    + prefix;
                spans[index * 2 + 1] =
                    i64::from(view.node(node).map_err(compiler_error)?.end()) + prefix;
            }
            result.insert(spans, (id, member));
        }
        for child in tsr_astnav::visit_each_child(view, id).map_err(compiler_error)? {
            match child {
                tsr_astnav::Visit::Node(node) => pending.push(node),
                tsr_astnav::Visit::List(list) => pending.extend(
                    view.node_slice(view.list(list).map_err(compiler_error)?.nodes())
                        .map_err(compiler_error)?
                        .iter()
                        .flatten(),
                ),
            }
        }
    }
    Ok(result)
}

fn spans(query: &MemberQuery, raw: &[u8]) -> Result<Spans, CheckerError> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| error("unsupported_encoding", "indexed source is not UTF-8"))?;
    let spans = [
        query.call_start,
        query.call_end,
        query.receiver_start,
        query.receiver_end,
        query.property_start,
        query.property_end,
    ];
    for offset in spans {
        let byte =
            usize::try_from(offset).map_err(|_| error("invalid_span", "negative source offset"))?;
        if byte > raw.len() || !text.is_char_boundary(byte) {
            return Err(error(
                "invalid_span",
                "source offset is not a UTF-8 boundary",
            ));
        }
    }
    if !(spans[0] <= spans[2]
        && spans[2] < spans[3]
        && spans[3] <= spans[4]
        && spans[4] < spans[5]
        && spans[5] <= spans[1])
    {
        return Err(error(
            "invalid_span",
            "receiver and property must be contained by the call",
        ));
    }
    Ok(spans)
}

fn declarations(
    op: &mut Operation<'_>,
    receiver: tsr_checker::TypeRef,
    member: NodeId,
    call: NodeId,
) -> Result<Vec<NodeId>, CheckerError> {
    let name = op
        .node(member)
        .map_err(compiler_error)?
        .name()
        .ok_or_else(|| error("checker_failure", "missing member name"))?;
    let name_node = op.node(name).map_err(compiler_error)?;
    let property = if let Some(identifier) = name_node.as_identifier() {
        identifier.text().to_vec()
    } else if let Some(identifier) = name_node.as_private_identifier() {
        identifier.text().to_vec()
    } else {
        return Err(error("checker_failure", "unsupported property name"));
    };
    drop(name_node);
    let types = if op.type_flags(receiver).map_err(compiler_error)? & type_flags::UNION != 0 {
        op.constituents(receiver).map_err(compiler_error)?
    } else {
        vec![receiver]
    };
    let mut symbols = HashSet::new();
    let mut result = Vec::new();
    for ty in types {
        let symbol = match op
            .get_property_of_type(ty, &property)
            .map_err(compiler_error)?
        {
            Some(symbol) => Some(symbol),
            None => op.get_symbol_at_location(name).map_err(compiler_error)?,
        };
        let Some(mut symbol) = symbol else { continue };
        if op.symbol(symbol).map_err(compiler_error)?.flags() & tsr_ast::symbol_flags::ALIAS != 0 {
            symbol = op.get_aliased_symbol(symbol).map_err(compiler_error)?;
        }
        if !symbols.insert(symbol) {
            continue;
        }
        let nodes = op
            .symbol_declarations(symbol)
            .map_err(compiler_error)?
            .iter()
            .flatten()
            .collect::<Vec<_>>();
        let mut implementation = None;
        for &node in &nodes {
            if op
                .implementation_of_overload(node)
                .map_err(compiler_error)?
            {
                implementation = Some(node);
                break;
            }
        }
        if let Some(node) = implementation {
            result.push(node);
        } else if nodes.len() > 1
            && nodes.iter().all(|node| {
                op.node(*node).is_ok_and(|node| {
                    matches!(
                        node.kind().known(),
                        Some(K::MethodSignature | K::MethodDeclaration | K::FunctionDeclaration)
                    )
                })
            })
        {
            let signature = op.get_resolved_signature(call).map_err(compiler_error)?;
            let resolved = op
                .signature_declaration(signature)
                .map_err(compiler_error)?
                .map(|node| node.id());
            result.push(
                resolved
                    .filter(|node| nodes.contains(node))
                    .unwrap_or(nodes[0]),
            );
        } else {
            result.extend(nodes);
        }
    }
    Ok(result)
}

fn declaration(
    root: &Path,
    built: &BuiltProject,
    op: &Operation<'_>,
    node: NodeId,
) -> Result<DeclarationSite, CheckerError> {
    let name = op
        .node(node)
        .map_err(compiler_error)?
        .name()
        .unwrap_or(node);
    let mut source = node;
    while let Some(parent) = op.node(source).map_err(compiler_error)?.parent() {
        source = parent;
    }
    let file = built
        .session
        .program()
        .files()
        .iter()
        .find(|file| file.source() == source)
        .ok_or_else(|| error("checker_failure", "declaration has no retained source file"))?;
    let source_file = file.bound().view().source_file().map_err(compiler_error)?;
    let source_name = std::str::from_utf8(source_file.file_name())
        .map_err(|_| error("unsupported_path", "non-UTF-8 declaration path"))?;
    let bundled = tsr_bundled::is_bundled(source_file.file_name());
    let (path, raw) = if bundled {
        (None, Arc::<[u8]>::from(source_file.text().as_bytes()))
    } else {
        let canonical = built
            .project
            .host
            .realpath(source_name.as_bytes())
            .map_err(compiler_error)?;
        let canonical = PathBuf::from(
            std::str::from_utf8(canonical.as_bytes())
                .map_err(|_| error("unsupported_path", "non-UTF-8 declaration path"))?,
        );
        let raw = built.project.host.raw(Path::new(source_name))?;
        (Some(canonical), raw)
    };
    let prefix = source_prefix(&raw, source_file.text().as_bytes())?;
    let mut jsdoc = tsr_parser::ParserJsDocProvider::default();
    let mut navigator = tsr_astnav::Navigator::new(file.bound().view().ast(), source, &mut jsdoc);
    let relative = path
        .as_deref()
        .filter(|path| path.starts_with(root))
        .map(|path| relative(root, path))
        .transpose()?;
    let context = if bundled {
        "lib"
    } else if relative.is_none() {
        "outside"
    } else if relative
        .as_deref()
        .is_some_and(|p| p.contains("node_modules/@types/"))
    {
        "types"
    } else if relative
        .as_deref()
        .is_some_and(|p| p.contains("node_modules/"))
    {
        "vendored"
    } else {
        "repo"
    };
    Ok(DeclarationSite {
        outside_root: relative.is_none(),
        file: relative,
        start: navigator
            .get_start_of_node(name, false)
            .map_err(compiler_error)?
            + prefix,
        end: i64::from(op.node(name).map_err(compiler_error)?.end()) + prefix,
        source_hash: blake3::hash(&raw).to_hex().to_string(),
        context: Some(context.into()),
    })
}

fn normalize_type(root: &Path, text: &str) -> Result<String, CheckerError> {
    let mut output = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("import(\"") {
        output.push_str(&rest[..start + 8]);
        rest = &rest[start + 8..];
        let Some(end) = rest.find('"') else { break };
        let specifier = &rest[..end];
        let path = Path::new(specifier);
        if path.is_absolute() {
            if path.starts_with(root) {
                output.push_str(&relative(root, path)?);
            } else {
                output.push_str("outside:");
                output.push_str(
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("unknown"),
                );
            }
        } else {
            output.push_str(specifier);
        }
        rest = &rest[end..];
    }
    output.push_str(rest);
    Ok(crate::checker::protocol::bounded_receiver_type(&output))
}

impl BuiltProject {
    pub fn resolve(
        &mut self,
        root: &Path,
        query: &MemberQuery,
        request: &Request,
    ) -> Result<MemberProjectResult, CheckerError> {
        request.diagnostics.query(query, "validate-source");
        request.check()?;
        let path = query_file(root, &query.file)?;
        if !self.project.roots.contains(&path) {
            return Err(error(
                "project_mismatch",
                format!(
                    "{} is not owned by {}",
                    query.file, self.project.summary.project_id
                ),
            ));
        }
        let raw = std::fs::read(&path).map_err(|e| error("query_file_missing", e.to_string()))?;
        let hash = blake3::hash(&raw).to_hex().to_string();
        if hash != query.indexed_hash || self.project.host.raw(&path)?.as_ref() != raw {
            return Err(error(
                "hash_mismatch",
                format!("source changed after indexing or loading: {}", query.file),
            ));
        }
        request.diagnostics.operation("validate-call-spans");
        let spans = spans(query, &raw)?;
        let mut answer = ProjectAnswer {
            project_id: self.project.summary.project_id.clone(),
            status: "unknown".into(),
            receiver_type: None,
            declarations: Vec::new(),
            checker_input_fingerprint: self.fingerprint.clone(),
            error: None,
        };
        {
            request.diagnostics.operation("call-lookup");
            let file = self
                .session
                .program()
                .source_file(path_text(&path)?.as_bytes())
                .ok_or_else(|| {
                    error(
                        "project_mismatch",
                        "owned query source is absent from the compiler program",
                    )
                })?;
            if !self.calls.contains_key(&path) {
                self.calls.insert(
                    path.clone(),
                    calls(file, &self.project.host, &path, request)?,
                );
            }
            let &(call, member) = self.calls[&path].get(&spans).ok_or_else(|| {
                error(
                    "span_mismatch",
                    format!(
                        "exact indexed member-call occurrence not found: {}",
                        query.file
                    ),
                )
            })?;
            request.diagnostics.operation("checker-operation");
            let mut op = self.session.operation().map_err(compiler_error)?;
            request.diagnostics.operation("receiver-node");
            let receiver = op
                .node(member)
                .map_err(compiler_error)?
                .expression()
                .ok_or_else(|| error("checker_failure", "missing receiver"))?;
            request.diagnostics.operation("receiver-type");
            #[cfg(test)]
            super::test_support::query_fault(query)?;
            let ty = op.get_type_at_location(receiver).map_err(compiler_error)?;
            request.check()?;
            request.diagnostics.operation("receiver-type-flags");
            if op.type_flags(ty).map_err(compiler_error)? & type_flags::ANY_OR_UNKNOWN == 0 {
                request.diagnostics.operation("declaration-resolution");
                let nodes = declarations(&mut op, ty, member, call)?;
                request.diagnostics.operation("declaration-site");
                let mut sites = BTreeMap::new();
                for node in nodes {
                    let site = declaration(root, self, &op, node)?;
                    sites.insert(
                        (
                            site.file.clone(),
                            site.start,
                            site.end,
                            site.source_hash.clone(),
                            site.context.clone(),
                        ),
                        site,
                    );
                }
                answer.declarations = sites.into_values().collect();
                request.diagnostics.operation("type-printing");
                let text = op
                    .type_to_string_at(ty, Some(receiver), type_format_flags::NO_TRUNCATION)
                    .map_err(compiler_error)?;
                answer.receiver_type = Some(normalize_type(
                    root,
                    &String::from_utf8_lossy(text.as_bytes()),
                )?);
                if !answer.declarations.is_empty() {
                    answer.status = "resolved".into();
                }
            }
        }
        request.check()?;
        Ok(MemberProjectResult {
            indexed_hash: query.indexed_hash.clone(),
            source_hash: hash,
            answer,
        })
    }

    pub fn restart_checker(&mut self, request: &Request) -> Result<(), CheckerError> {
        request
            .diagnostics
            .project(&self.project.summary.project_id, "checker-recovery");
        request.check()?;
        let program = Arc::clone(self.session.program());
        self.session.retire();
        self.session = Session::from_program(program, &tsr_arena::Counters::new());
        self.session.checker().map_err(compiler_error)?;
        request.check()
    }

    pub fn refresh_manifest(&mut self) -> Result<(), CheckerError> {
        self.inputs = self.project.host.inputs()?;
        self.fingerprint = fingerprint(&self.project, &self.inputs)?;
        Ok(())
    }
}
