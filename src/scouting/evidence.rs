//! Bounded, line-numbered evidence packs. Full source is the default input;
//! design scouting can instead retain complete candidate declarations plus a
//! small surrounding window. Deterministic entity occurrences annotate each
//! file. Rendering is fully deterministic so the pack can participate in the
//! input fingerprint.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::Connection;

use crate::semantic::WorkflowCandidate;

#[derive(Debug, Clone)]
pub struct FileEvidence {
    pub hash: String,
    pub line_count: i64,
}

#[derive(Debug, Clone)]
pub struct EvidencePack {
    pub rendered: String,
    pub files: BTreeMap<String, FileEvidence>,
}

/// Depth-1 structural context is bounded per direction; a symbol with more
/// neighbours reports the omitted count rather than truncating silently.
const CONTEXT_EDGES_PER_DIRECTION: usize = 40;

/// Design claims can cite only candidate declaration spans. Keeping those
/// spans complete plus nearby imports/control context avoids shipping an
/// unrelated 4,000-line file merely because one symbol localized there.
pub const DESIGN_CONTEXT_LINES: usize = 12;

/// Build the pack for a candidate set inside the caller's read snapshot.
/// Fails when any candidate file changed on disk since indexing: scouting
/// against un-indexed edits would publish evidence that immediately reads
/// as stale.
pub fn build(
    root: &Path,
    conn: &Connection,
    candidates: &[WorkflowCandidate],
) -> Result<EvidencePack> {
    build_titled(root, conn, candidates, "Candidates")
}

/// `build` with a caller-chosen heading for the anchor listing. Card packs
/// carry a single subject rather than a candidate set.
pub fn build_titled(
    root: &Path,
    conn: &Connection,
    candidates: &[WorkflowCandidate],
    section: &str,
) -> Result<EvidencePack> {
    build_titled_with_windows(root, conn, candidates, section, None)
}

/// Build a design pack from complete candidate declaration spans and bounded
/// surrounding source. Original line numbers are preserved, and omitted
/// ranges are explicit. Hashes and line counts still cover the whole file so
/// freshness validation remains identical to a full-source pack.
pub fn build_titled_design(
    root: &Path,
    conn: &Connection,
    candidates: &[WorkflowCandidate],
    section: &str,
) -> Result<EvidencePack> {
    build_titled_with_windows(root, conn, candidates, section, Some(DESIGN_CONTEXT_LINES))
}

fn build_titled_with_windows(
    root: &Path,
    conn: &Connection,
    candidates: &[WorkflowCandidate],
    section: &str,
    context_lines: Option<usize>,
) -> Result<EvidencePack> {
    let mut files: BTreeMap<String, FileEvidence> = BTreeMap::new();
    let mut sources: BTreeMap<String, String> = BTreeMap::new();
    for candidate in candidates {
        if files.contains_key(&candidate.file) {
            continue;
        }
        let indexed_hash: String = conn
            .query_row(
                "SELECT hash FROM files WHERE path=?1",
                [&candidate.file],
                |row| row.get(0),
            )
            .with_context(|| format!("candidate file `{}` is not indexed", candidate.file))?;
        let source = std::fs::read_to_string(root.join(&candidate.file))
            .with_context(|| format!("read candidate file `{}`", candidate.file))?;
        if blake3::hash(source.as_bytes()).to_hex().as_str() != indexed_hash {
            bail!(
                "candidate file `{}` changed since indexing; run `jscout index` first",
                candidate.file
            );
        }
        files.insert(
            candidate.file.clone(),
            FileEvidence {
                hash: indexed_hash,
                line_count: source.lines().count() as i64,
            },
        );
        sources.insert(candidate.file.clone(), source);
    }

    let mut rendered = String::new();
    rendered.push_str(&format!("## {section}\n\n"));
    for candidate in candidates {
        rendered.push_str(&format!(
            "- {} ({}) in {} lines {}-{}{}\n",
            candidate.anchor,
            candidate.display_name,
            candidate.file,
            candidate.evidence_start_line,
            candidate.evidence_end_line,
            if candidate.seed { " [seed]" } else { "" },
        ));
    }

    for (file, source) in &sources {
        rendered.push_str(&format!("\n## File: {file}\n"));
        let annotations = entity_annotations(conn, file)?;
        if !annotations.is_empty() {
            rendered.push_str("Deterministic entities:\n");
            for annotation in annotations {
                rendered.push_str(&format!("- {annotation}\n"));
            }
        }
        let line_count = source.lines().count();
        let ranges =
            context_lines.map(|context| candidate_windows(candidates, file, line_count, context));
        if ranges.is_some() {
            rendered.push_str(&format!(
                "Candidate source windows (complete declarations plus {DESIGN_CONTEXT_LINES} surrounding lines; omitted ranges are not evidence):\n"
            ));
        }
        rendered.push_str("```\n");
        render_numbered_source(&mut rendered, source, ranges.as_deref());
        rendered.push_str("```\n");
    }

    Ok(EvidencePack { rendered, files })
}

fn candidate_windows(
    candidates: &[WorkflowCandidate],
    file: &str,
    line_count: usize,
    context_lines: usize,
) -> Vec<(usize, usize)> {
    let mut ranges = candidates
        .iter()
        .filter(|candidate| candidate.file == file)
        .map(|candidate| {
            let start = usize::try_from(candidate.evidence_start_line)
                .unwrap_or(1)
                .max(1)
                .saturating_sub(context_lines)
                .max(1)
                .min(line_count.max(1));
            let end = usize::try_from(candidate.evidence_end_line)
                .unwrap_or(line_count)
                .max(start)
                .saturating_add(context_lines)
                .min(line_count);
            (start, end)
        })
        .collect::<Vec<_>>();
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        if let Some(previous) = merged.last_mut()
            && start <= previous.1.saturating_add(1)
        {
            previous.1 = previous.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    merged
}

fn render_numbered_source(rendered: &mut String, source: &str, ranges: Option<&[(usize, usize)]>) {
    let lines = source.lines().collect::<Vec<_>>();
    let ranges = ranges.map(|ranges| ranges.to_vec()).unwrap_or_else(|| {
        (!lines.is_empty())
            .then_some((1, lines.len()))
            .into_iter()
            .collect()
    });
    let mut previous_end: usize = 0;
    for (start, end) in ranges {
        if start > previous_end.saturating_add(1) {
            rendered.push_str(&format!(
                "      | /* … jscout omitted source lines {}-{} … */\n",
                previous_end + 1,
                start - 1,
            ));
        }
        for line_number in start..=end {
            if let Some(line) = lines.get(line_number - 1) {
                rendered.push_str(&format!("{line_number:>5} | {line}\n"));
            }
        }
        previous_end = previous_end.max(end);
    }
    if previous_end < lines.len() {
        rendered.push_str(&format!(
            "      | /* … jscout omitted source lines {}-{} … */\n",
            previous_end + 1,
            lines.len(),
        ));
    }
}

/// Deterministic depth-1 in/out edges of one anchor, rendered for a card
/// pack. These are indexed facts: the prompt forbids restating them as
/// claims, and they are never citable evidence because only the subject's
/// declaring file is shipped as numbered source.
pub fn structural_context(conn: &Connection, anchor: &str) -> Result<(String, usize)> {
    let mut statement = conn.prepare_cached(
        "SELECT DISTINCT 'out', edge.kind, edge.confidence, edge.dst_key,
                node.display_name, COALESCE(file.path, ''), COALESCE(node.line, 0)
         FROM resolved_edges edge
         JOIN graph_nodes node ON node.node_key=edge.dst_key
         LEFT JOIN files file ON file.id=node.file_id
         WHERE edge.src_key=?1
         UNION
         SELECT DISTINCT 'in', edge.kind, edge.confidence, edge.src_key,
                node.display_name, COALESCE(file.path, ''), COALESCE(node.line, 0)
         FROM resolved_edges edge
         JOIN graph_nodes node ON node.node_key=edge.src_key
         LEFT JOIN files file ON file.id=node.file_id
         WHERE edge.dst_key=?1
         ORDER BY 1, 2, 4, 3",
    )?;
    let rows = statement.query_map([anchor], |row| {
        Ok((
            row.get::<_, String>(0)?,
            format!(
                "{} ({}) {} `{}` at {}:{}",
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
            ),
        ))
    })?;
    let mut outgoing = Vec::new();
    let mut incoming = Vec::new();
    for row in rows {
        let (direction, line) = row?;
        if direction == "out" {
            outgoing.push(line);
        } else {
            incoming.push(line);
        }
    }
    let total = outgoing.len() + incoming.len();

    let mut rendered = String::from(
        "\n## Direct structural context\n\n\
         Deterministic depth-1 edges of the subject. These facts are already \
         indexed; do not restate them as claims.\n",
    );
    for (label, mut edges) in [("Outgoing", outgoing), ("Incoming", incoming)] {
        let omitted = edges.len().saturating_sub(CONTEXT_EDGES_PER_DIRECTION);
        edges.truncate(CONTEXT_EDGES_PER_DIRECTION);
        rendered.push_str(&format!("\n{label}:\n"));
        if edges.is_empty() {
            rendered.push_str("- none\n");
        }
        for edge in edges {
            rendered.push_str(&format!("- {edge}\n"));
        }
        if omitted > 0 {
            rendered.push_str(&format!("- ({omitted} further edges omitted)\n"));
        }
    }
    Ok((rendered, total))
}

/// Runtime and general entity occurrences in one file, ordered by position.
/// Contract-plane rows are documentary and stay out of workflow evidence.
fn entity_annotations(conn: &Connection, file: &str) -> Result<Vec<String>> {
    let mut statement = conn.prepare_cached(
        "SELECT occurrence.line, entity.plane, entity.entity_type, entity.name, occurrence.role
         FROM entity_occurrences occurrence
         JOIN entities entity ON entity.id=occurrence.entity_id
         JOIN files file ON file.id=occurrence.file_id
         WHERE file.path=?1 AND entity.plane IN ('runtime', 'general')
         ORDER BY occurrence.line, entity.entity_type, entity.name, occurrence.role",
    )?;
    let rows = statement.query_map([file], |row| {
        Ok(format!(
            "line {}: {}/{} `{}` ({})",
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use crate::semantic::{WorkflowCandidateOptions, workflow_candidates};
    use crate::{indexer, store};

    #[test]
    fn renders_a_deterministic_numbered_pack_and_rejects_disk_drift() -> Result<()> {
        let repo = tempfile::tempdir()?;
        std::fs::write(
            repo.path().join("flow.ts"),
            "export function finish() { return process.env.API_KEY; }\n\
             export function start() { return finish(); }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let candidates = workflow_candidates(
            repo.path(),
            &conn,
            &["flow.ts:start".into()],
            &WorkflowCandidateOptions::default(),
        )?;

        let first = super::build(repo.path(), &conn, &candidates.candidates)?;
        let second = super::build(repo.path(), &conn, &candidates.candidates)?;
        assert_eq!(
            first.rendered, second.rendered,
            "pack must be deterministic"
        );
        assert!(first.rendered.contains("## File: flow.ts"));
        assert!(first.rendered.contains("    1 | export function finish()"));
        assert!(
            first
                .rendered
                .contains("general/environment_variable `API_KEY`"),
            "entity annotations belong in the pack"
        );
        assert_eq!(first.files["flow.ts"].line_count, 2);

        // Un-indexed disk drift must fail the build, not ship stale evidence.
        std::fs::write(repo.path().join("flow.ts"), "export function finish() {}\n")?;
        let error = super::build(repo.path(), &conn, &candidates.candidates)
            .expect_err("changed file must be rejected");
        assert!(error.to_string().contains("changed since indexing"));
        Ok(())
    }

    #[test]
    fn design_pack_keeps_complete_candidate_spans_and_omits_unrelated_source() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let source = (1..=80)
            .map(|line| {
                if line == 40 {
                    "export function target() { return 40; }".to_string()
                } else {
                    format!("const line{line} = {line};")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(repo.path().join("large.ts"), source)?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let candidate = crate::semantic::WorkflowCandidate {
            anchor: "sym:large.ts#::target@1".into(),
            display_name: "target".into(),
            file: "large.ts".into(),
            evidence_start_line: 40,
            evidence_end_line: 40,
            relevance: 1.0,
            seed: true,
        };

        let pack = super::build_titled_design(
            repo.path(),
            &conn,
            &[candidate],
            "Design evidence candidates",
        )?;
        assert!(pack.rendered.contains("   40 | export function target()"));
        assert!(pack.rendered.contains("omitted source lines 1-27"));
        assert!(pack.rendered.contains("omitted source lines 53-80"));
        assert!(!pack.rendered.contains("    1 | const line1"));
        assert_eq!(pack.files["large.ts"].line_count, 80);
        Ok(())
    }

    #[test]
    fn structural_context_renders_both_directions_deterministically() -> Result<()> {
        let repo = tempfile::tempdir()?;
        std::fs::write(
            repo.path().join("flow.ts"),
            "export function finish() { return 1; }\n\
             export function start() { return finish(); }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;

        let (rendered, edges) = super::structural_context(&conn, "sym:flow.ts#::finish@1")?;
        let (repeat, repeat_edges) = super::structural_context(&conn, "sym:flow.ts#::finish@1")?;
        assert_eq!((rendered.clone(), edges), (repeat, repeat_edges));
        assert!(rendered.contains("## Direct structural context"));
        assert!(rendered.contains("do not restate them as claims"));
        assert!(
            rendered.contains("call (certain) sym:flow.ts#::start@1 `start` at flow.ts:2"),
            "incoming callers carry kind, confidence, anchor, and site: {rendered}"
        );
        assert!(rendered.contains("Outgoing:\n- none"), "{rendered}");
        assert_eq!(edges, 1);
        Ok(())
    }
}
