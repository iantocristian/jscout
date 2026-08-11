//! Deterministic call-site queries: exact AST matching over indexed member
//! calls. The index narrows candidate files (member-call prop plus FTS over
//! argument tokens); matching re-parses those files so every answer carries
//! the complete call span, the static receiver chain, and the matched
//! argument structure. Evidence joins calls by span containment, never by
//! start-line equality — a multiline call owns every line inside it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use oxc_ast::ast::{Argument, CallExpression, Expression, ObjectExpression, ObjectPropertyKind};
use oxc_ast_visit::Visit;
use rusqlite::Connection;
use serde::Serialize;

use crate::chunk::LineIndex;
use crate::{heur, parse, structural};

/// One `--arg KEY[=VALUE]` filter. All filters must match top-level
/// properties of the same object-literal argument.
#[derive(Debug, Clone)]
pub struct ArgFilter {
    pub key: String,
    pub value: Option<String>,
}

impl ArgFilter {
    /// `KEY` or `KEY=VALUE`; the value is compared against literal argument
    /// text (string/number/boolean/null/expressionless template).
    pub fn parse(text: &str) -> Result<Self> {
        let (key, value) = match text.split_once('=') {
            Some((key, value)) => (key.trim(), Some(value.trim().to_string())),
            None => (text.trim(), None),
        };
        if key.is_empty() {
            bail!("--arg requires KEY or KEY=VALUE; received `{text}`");
        }
        Ok(Self {
            key: key.to_string(),
            value,
        })
    }
}

#[derive(Debug, Clone)]
pub struct CallQuery {
    pub method: String,
    pub args: Vec<ArgFilter>,
    /// 1-based argument position the object literal must occupy.
    pub arg_position: Option<usize>,
    /// Dotted suffix the static receiver chain must end with, e.g.
    /// `wave.card` matches `dbs.wave.card`.
    pub receiver_suffix: Option<String>,
    pub file_origins: Vec<String>,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct MatchedOption {
    pub key: String,
    /// Literal text when the property value is a literal; None for
    /// key-presence matches on non-literal values.
    pub value: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallSite {
    pub file: String,
    /// Complete CallExpression line range, inclusive.
    pub start_line: i64,
    pub end_line: i64,
    /// Complete CallExpression byte span.
    pub span: [u32; 2],
    pub receiver: Option<String>,
    pub method: String,
    pub argument_count: usize,
    /// 1-based position of the argument that satisfied the filters.
    pub matched_argument: Option<usize>,
    pub matched_options: Vec<MatchedOption>,
    /// Innermost enclosing declaration anchor; None for module-level calls.
    pub anchor: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallQueryResult {
    pub snapshot: String,
    pub method: String,
    pub files_scanned: usize,
    pub matches: Vec<CallSite>,
    pub truncated: bool,
}

struct CandidateFile {
    id: i64,
    path: String,
    hash: String,
    physical: PathBuf,
}

pub fn query(root: &Path, conn: &Connection, query: &CallQuery) -> Result<CallQueryResult> {
    crate::origin::validate_all(&query.file_origins)?;
    let method = query.method.trim();
    if method.is_empty() {
        bail!("a method name is required, e.g. `jscout calls <root> insert`");
    }
    let snapshot = structural::current_snapshot(conn)?;

    let mut files = candidate_files(root, conn, method, &query.file_origins)?;
    if let Some(keep) = fts_file_ids(conn, &query.args)? {
        files.retain(|file| keep.contains(&file.id));
    }

    let mut matches = Vec::new();
    let mut truncated = false;
    for file in &files {
        let source = std::fs::read_to_string(&file.physical)
            .with_context(|| format!("read candidate file `{}`", file.path))?;
        if blake3::hash(source.as_bytes()).to_hex().as_str() != file.hash {
            bail!(
                "candidate file `{}` changed since indexing; run `jscout index` first",
                file.path
            );
        }
        let lines = LineIndex::new(&source);
        let sites = parse::with_parsed(&source, Path::new(&file.path), |ret, _| {
            let mut collector = CallCollector {
                query,
                method,
                sites: Vec::new(),
            };
            collector.visit_program(&ret.program);
            collector.sites
        })?;
        if sites.is_empty() {
            continue;
        }
        let declarations = symbol_declarations(conn, file.id)?;
        for site in sites {
            if matches.len() >= query.limit {
                truncated = true;
                break;
            }
            let anchor = declarations
                .iter()
                .filter(|(_, start, end)| *start <= site.span[0] && site.span[1] <= *end)
                .min_by_key(|(_, start, end)| end - start)
                .map(|(key, _, _)| key.clone());
            matches.push(CallSite {
                file: file.path.clone(),
                start_line: i64::from(lines.line(site.span[0])),
                end_line: i64::from(lines.line(site.span[1].saturating_sub(1))),
                span: site.span,
                receiver: site.receiver,
                method: method.to_string(),
                argument_count: site.argument_count,
                matched_argument: site.matched_argument,
                matched_options: site.matched_options,
                anchor,
            });
        }
        if truncated {
            break;
        }
    }

    Ok(CallQueryResult {
        snapshot,
        method: method.to_string(),
        files_scanned: files.len(),
        matches,
        truncated,
    })
}

/// Files containing at least one member call of the method, restricted to
/// the allowed origins, with the physical path needed to re-read the source.
fn candidate_files(
    root: &Path,
    conn: &Connection,
    method: &str,
    file_origins: &[String],
) -> Result<Vec<CandidateFile>> {
    let repository = file_origins.iter().any(|origin| origin == "repository");
    let workspace = file_origins.iter().any(|origin| origin == "workspace");
    let dependency = file_origins.iter().any(|origin| origin == "dependency");
    let mut stmt = conn.prepare(
        "SELECT DISTINCT f.id, f.path, f.hash, f.origin, f.package_path, p.canonical_root
         FROM member_calls call
         JOIN files f ON f.id=call.file_id
         LEFT JOIN package_instances p ON p.id=f.package_instance_id
         WHERE call.prop=?1
           AND ((?2 AND f.origin='repository')
             OR (?3 AND f.origin='workspace')
             OR (?4 AND f.origin='dependency'))
         ORDER BY f.path",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![method, repository, workspace, dependency],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        },
    )?;
    let mut files = Vec::new();
    for row in rows {
        let (id, path, hash, origin, package_path, package_root) = row?;
        let physical = if origin == "dependency" {
            let package_root = package_root
                .ok_or_else(|| anyhow::anyhow!("dependency file {path} has no package root"))?;
            let package_path = package_path
                .ok_or_else(|| anyhow::anyhow!("dependency file {path} has no package path"))?;
            PathBuf::from(package_root).join(package_path)
        } else {
            root.join(&path)
        };
        files.push(CandidateFile {
            id,
            path,
            hash,
            physical,
        });
    }
    Ok(files)
}

/// Narrow candidates to files whose indexed chunks contain every usable
/// argument token. Terms may match in different chunks of one file, so the
/// intersection is per file, not per chunk.
fn fts_file_ids(conn: &Connection, args: &[ArgFilter]) -> Result<Option<BTreeSet<i64>>> {
    let mut keep: Option<BTreeSet<i64>> = None;
    let terms = args
        .iter()
        .flat_map(|filter| [Some(filter.key.as_str()), filter.value.as_deref()])
        .flatten()
        .filter(|term| term.chars().any(char::is_alphanumeric));
    for term in terms {
        let quoted = format!("\"{}\"", term.replace('"', "\"\""));
        let mut stmt = conn.prepare_cached(
            "SELECT DISTINCT chunk.file_id
             FROM chunks_fts
             JOIN chunks chunk ON chunk.id=chunks_fts.rowid
             WHERE chunks_fts MATCH ?1",
        )?;
        let rows = stmt.query_map([&quoted], |row| row.get::<_, i64>(0))?;
        let ids = rows.collect::<std::result::Result<BTreeSet<i64>, _>>()?;
        keep = Some(match keep {
            Some(existing) => existing.intersection(&ids).copied().collect(),
            None => ids,
        });
    }
    Ok(keep)
}

/// Symbol declaration spans for one file from the projection, for innermost
/// enclosing-anchor attribution.
fn symbol_declarations(conn: &Connection, file_id: i64) -> Result<Vec<(String, u32, u32)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT node_key, meta_json FROM graph_nodes
         WHERE node_kind='symbol' AND file_id=?1",
    )?;
    let rows = stmt.query_map([file_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut declarations = Vec::new();
    for row in rows {
        let (key, meta) = row?;
        let meta: serde_json::Value = serde_json::from_str(&meta)?;
        if let (Some(start), Some(end)) = (
            meta["declaration"][0].as_u64(),
            meta["declaration"][1].as_u64(),
        ) {
            declarations.push((key, start as u32, end as u32));
        }
    }
    Ok(declarations)
}

struct FoundSite {
    span: [u32; 2],
    receiver: Option<String>,
    argument_count: usize,
    matched_argument: Option<usize>,
    matched_options: Vec<MatchedOption>,
}

struct CallCollector<'q> {
    query: &'q CallQuery,
    method: &'q str,
    sites: Vec<FoundSite>,
}

impl<'a> Visit<'a> for CallCollector<'_> {
    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if let Expression::StaticMemberExpression(member) = &call.callee
            && member.property.name == self.method
            && self.receiver_matches(heur::member_path(&member.object).as_deref())
            && let Some((matched_argument, matched_options)) = self.match_arguments(call)
        {
            self.sites.push(FoundSite {
                span: [call.span.start, call.span.end],
                receiver: heur::member_path(&member.object),
                argument_count: call.arguments.len(),
                matched_argument,
                matched_options,
            });
        }
        oxc_ast_visit::walk::walk_call_expression(self, call);
    }
}

impl CallCollector<'_> {
    fn receiver_matches(&self, receiver: Option<&str>) -> bool {
        let Some(suffix) = self.query.receiver_suffix.as_deref() else {
            return true;
        };
        let Some(receiver) = receiver else {
            return false;
        };
        let receiver: Vec<&str> = receiver.split('.').collect();
        let suffix: Vec<&str> = suffix.split('.').collect();
        receiver.ends_with(&suffix)
    }

    /// All filters must match top-level properties of one object-literal
    /// argument. With no filters every call of the method matches.
    fn match_arguments(
        &self,
        call: &CallExpression<'_>,
    ) -> Option<(Option<usize>, Vec<MatchedOption>)> {
        if self.query.args.is_empty() {
            return Some((None, Vec::new()));
        }
        for (index, argument) in call.arguments.iter().enumerate() {
            let position = index + 1;
            if self
                .query
                .arg_position
                .is_some_and(|required| required != position)
            {
                continue;
            }
            if let Argument::ObjectExpression(object) = argument
                && let Some(matched) = object_matches(object, &self.query.args)
            {
                return Some((Some(position), matched));
            }
        }
        None
    }
}

fn object_matches(
    object: &ObjectExpression<'_>,
    filters: &[ArgFilter],
) -> Option<Vec<MatchedOption>> {
    let mut matched = Vec::new();
    for filter in filters {
        let mut satisfied = None;
        for property in &object.properties {
            let ObjectPropertyKind::ObjectProperty(property) = property else {
                continue;
            };
            if property.key.static_name().as_deref() != Some(filter.key.as_str()) {
                continue;
            }
            let text = literal_text(&property.value);
            match &filter.value {
                Some(expected) if text.as_deref() == Some(expected.as_str()) => {
                    satisfied = Some(text);
                    break;
                }
                Some(_) => {}
                None => {
                    satisfied = Some(text);
                    break;
                }
            }
        }
        let value = satisfied?;
        matched.push(MatchedOption {
            key: filter.key.clone(),
            value,
        });
    }
    Some(matched)
}

/// Exact literal text for comparison: strings unquoted, numbers as written,
/// booleans/null spelled out, expressionless templates raw.
fn literal_text(expr: &Expression<'_>) -> Option<String> {
    match expr {
        Expression::StringLiteral(literal) => Some(literal.value.to_string()),
        Expression::NumericLiteral(literal) => Some(
            literal
                .raw
                .as_ref()
                .map(|raw| raw.to_string())
                .unwrap_or_else(|| literal.value.to_string()),
        ),
        Expression::BooleanLiteral(literal) => Some(literal.value.to_string()),
        Expression::NullLiteral(_) => Some("null".into()),
        Expression::TemplateLiteral(template) if template.expressions.is_empty() => template
            .quasis
            .first()
            .map(|quasi| quasi.value.raw.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use super::{ArgFilter, CallQuery};
    use crate::{indexer, store};

    fn base_query(method: &str) -> CallQuery {
        CallQuery {
            method: method.into(),
            args: Vec::new(),
            arg_position: None,
            receiver_suffix: None,
            file_origins: crate::origin::defaults(),
            limit: 200,
        }
    }

    #[test]
    fn multiline_option_matches_report_the_enclosing_call_span() -> Result<()> {
        let repo = tempfile::tempdir()?;
        std::fs::write(
            repo.path().join("store.ts"),
            "export async function remap(dbs: any) {\n\
             \x20 await dbs.wave.card.insert(\n\
             \x20   {\n\
             \x20     id: 'fake',\n\
             \x20     parent: '0',\n\
             \x20     attributes: {\n\
             \x20       location: 'London',\n\
             \x20     },\n\
             \x20   },\n\
             \x20   'test:remap-set',\n\
             \x20   { merge: 'replace' },\n\
             \x20 );\n\
             }\n\
             export async function patchOnly(dbs: any) {\n\
             \x20 await dbs.wave.card.insert({ id: 'x' }, 'k', { merge: 'patch' });\n\
             }\n\
             export function unrelated(insert: (o: object) => void) {\n\
             \x20 insert({ merge: 'replace' });\n\
             }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;

        let mut query = base_query("insert");
        query.args = vec![ArgFilter::parse("merge=replace")?];
        let result = super::query(repo.path(), &conn, &query)?;

        // The bare `insert(...)` call is not a member call and must not match.
        assert_eq!(result.matches.len(), 1, "{:#?}", result.matches);
        let site = &result.matches[0];
        assert_eq!(site.file, "store.ts");
        assert_eq!(site.receiver.as_deref(), Some("dbs.wave.card"));
        assert_eq!(site.matched_argument, Some(3));
        assert_eq!(site.argument_count, 3);
        assert_eq!(site.matched_options.len(), 1);
        assert_eq!(site.matched_options[0].value.as_deref(), Some("replace"));
        // The option literal sits ten lines below the call start; the span
        // must cover it: this is the containment rule line-joins violate.
        assert_eq!((site.start_line, site.end_line), (2, 12));
        assert!(
            site.anchor.as_deref().unwrap_or("").contains("::remap@"),
            "innermost enclosing declaration expected, got {:?}",
            site.anchor
        );

        // Key-presence matches both member calls; value narrows to one.
        query.args = vec![ArgFilter::parse("merge")?];
        assert_eq!(super::query(repo.path(), &conn, &query)?.matches.len(), 2);

        // Receiver suffix filtering.
        query.args = vec![ArgFilter::parse("merge=replace")?];
        query.receiver_suffix = Some("wave.card".into());
        assert_eq!(super::query(repo.path(), &conn, &query)?.matches.len(), 1);
        query.receiver_suffix = Some("other.card".into());
        assert_eq!(super::query(repo.path(), &conn, &query)?.matches.len(), 0);

        // Position restriction: the options object is the third argument.
        query.receiver_suffix = None;
        query.arg_position = Some(1);
        assert_eq!(super::query(repo.path(), &conn, &query)?.matches.len(), 0);
        query.arg_position = Some(3);
        assert_eq!(super::query(repo.path(), &conn, &query)?.matches.len(), 1);
        Ok(())
    }

    #[test]
    fn rejects_disk_drift_instead_of_answering_from_stale_index() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let file = repo.path().join("app.ts");
        std::fs::write(
            &file,
            "export const run = (db: any) => db.items.insert({ merge: 'replace' });\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;

        let mut query = base_query("insert");
        query.args = vec![ArgFilter::parse("merge=replace")?];
        assert_eq!(super::query(repo.path(), &conn, &query)?.matches.len(), 1);

        std::fs::write(&file, "export const run = () => null;\n")?;
        let error = super::query(repo.path(), &conn, &query)
            .expect_err("changed candidate file must be rejected");
        assert!(error.to_string().contains("changed since indexing"));
        Ok(())
    }

    #[test]
    fn member_call_rows_store_full_spans_and_receiver_chains() -> Result<()> {
        let repo = tempfile::tempdir()?;
        std::fs::write(
            repo.path().join("multi.ts"),
            "export const go = (dbs: any) =>\n\
             \x20 dbs.wave.card.insert(\n\
             \x20   { id: 'fake' },\n\
             \x20   { merge: 'replace' },\n\
             \x20 );\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let (line, end_line, receiver): (i64, i64, Option<String>) = conn.query_row(
            "SELECT line, end_line, receiver FROM member_calls WHERE prop='insert'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!((line, end_line), (2, 5));
        assert_eq!(receiver.as_deref(), Some("dbs.wave.card"));
        Ok(())
    }
}
