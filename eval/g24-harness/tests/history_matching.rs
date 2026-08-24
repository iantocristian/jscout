//! G24 history matching, tested against the plan rather than against
//! convenience.
//!
//! The core crate deliberately provides no matcher, so this file implements the
//! plan's five rules
//! (`docs/plans/g24-markdown-retrieval-proposal-2026-08-24.md` -> "History and continuity") and then measures what they
//! actually produce:
//!
//! 1. within each unchanged path an exact hash occurring once on each side
//!    matches directly, independent of heading text or source order; a matched
//!    block is `moved` when its relative order against other matched blocks
//!    changed;
//! 2. repeated exact hashes within one path match only when already matched
//!    neighbors leave exactly one one-to-one monotone pairing; otherwise every
//!    ambiguous copy remains unmatched;
//! 3. version one creates NO predecessor edges across repository paths;
//! 4. an edited block receives a predecessor only when exactly one unmatched
//!    old block and one unmatched new block occur between the same immediately
//!    adjacent matched neighbors;
//! 5. every other unmatched new block is `added`; every other confirmed
//!    unmatched old block is `removed`.
//!
//! Where an assertion encodes behavior that CONTRADICTS the plan, the assertion
//! documents the observed behavior and a `PLAN CLAIM` / `OBSERVED` comment sits
//! directly above it. No test was weakened to get a green run.
//!
//! Rules the plan left underspecified are marked `SPEC GAP:` at the point of
//! use; the harness report lists them.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use g24_harness::md::{self, ChunkBounds};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

// ---------------------------------------------------------------------------
// Block records
// ---------------------------------------------------------------------------

/// One history unit: a retrieval-bearing body block of one document.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BlockRec {
    path: String,
    hash: String,
    breadcrumb: Vec<String>,
    nearest_heading: Option<String>,
    raw: String,
    line_start: usize,
}

/// SPEC GAP: the plan says "the ledger tracks retrieval-bearing body blocks"
/// but never states whether a block that renders empty (an HTML-comment-only
/// block) is a history unit. It produces no chunk and carries no retrieval
/// text, so it is excluded here, exactly like headings and thematic breaks.
fn body_blocks(path: &str, source: &str, bounds: &ChunkBounds) -> Vec<BlockRec> {
    let doc = md::index_document(path, source, bounds);
    doc.blocks
        .iter()
        .filter(|block| block.kind.is_retrieval_bearing() && !block.rendered.trim().is_empty())
        .map(|block| BlockRec {
            path: path.to_string(),
            hash: block.content_hash.clone(),
            breadcrumb: block.breadcrumb.clone(),
            nearest_heading: block.nearest_heading.clone(),
            raw: block.raw.clone(),
            line_start: block.line_start,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The matcher: the plan's five rules
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rule {
    /// Rule 1: exact hash occurring once on each side.
    UniqueHash,
    /// Rule 2: repeated hash resolved by a unique monotone pairing between the
    /// same already-matched neighbors.
    RepeatedMonotone,
    /// Rule 4: exactly one unmatched old and one unmatched new block between the
    /// same immediately adjacent matched neighbors.
    NeighborAnchored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pair {
    old: usize,
    new: usize,
    rule: Rule,
}

#[derive(Debug, Clone, Default)]
struct Matching {
    pairs: Vec<Pair>,
    added: Vec<usize>,
    removed: Vec<usize>,
}

/// How rule 4 treats a gap that has a matched neighbor on only one side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgePolicy {
    /// The plan as written: "between the SAME immediately adjacent matched
    /// neighbors" needs a matched neighbor on both sides, so an edit to the
    /// first or last block of a document is never matched.
    Strict,
    /// Counterfactual used only by the H6 test: document start/end act as
    /// sentinel neighbors. Never used by the ledger.
    Lenient,
}

/// A matched pair, identified identically on both sides so "the same
/// neighbors" is decidable.
type Anchor = (usize, usize);
/// (nearest matched neighbor before, nearest matched neighbor after).
type Interval = (Option<Anchor>, Option<Anchor>);

fn interval_old(index: usize, anchors: &[Anchor]) -> Interval {
    let mut before: Option<Anchor> = None;
    let mut after: Option<Anchor> = None;
    for &(old, new) in anchors {
        if old < index && before.is_none_or(|(b, _)| old > b) {
            before = Some((old, new));
        }
        if old > index && after.is_none_or(|(a, _)| old < a) {
            after = Some((old, new));
        }
    }
    (before, after)
}

fn interval_new(index: usize, anchors: &[Anchor]) -> Interval {
    let mut before: Option<Anchor> = None;
    let mut after: Option<Anchor> = None;
    for &(old, new) in anchors {
        if new < index && before.is_none_or(|(_, b)| new > b) {
            before = Some((old, new));
        }
        if new > index && after.is_none_or(|(_, a)| new < a) {
            after = Some((old, new));
        }
    }
    (before, after)
}

/// Match the retrieval-bearing blocks of ONE path between two snapshots.
///
/// Rule 3 is structural: this function is only ever called with `old` and `new`
/// drawn from the same repository path, so no cross-path edge can exist.
fn match_path(old: &[BlockRec], new: &[BlockRec], policy: EdgePolicy) -> Matching {
    let mut old_to_new: Vec<Option<usize>> = vec![None; old.len()];
    let mut new_to_old: Vec<Option<usize>> = vec![None; new.len()];
    let mut pairs: Vec<Pair> = Vec::new();

    // Deterministic iteration: BTreeMap keyed by content hash.
    let mut old_by_hash: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, rec) in old.iter().enumerate() {
        old_by_hash
            .entry(rec.hash.as_str())
            .or_default()
            .push(index);
    }
    let mut new_by_hash: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, rec) in new.iter().enumerate() {
        new_by_hash
            .entry(rec.hash.as_str())
            .or_default()
            .push(index);
    }

    // -- Rule 1: exact hash occurring exactly once on each side. Source order
    // and heading text are irrelevant, so this deliberately does not compare
    // positions.
    for (hash, olds) in &old_by_hash {
        if olds.len() != 1 {
            continue;
        }
        let Some(news) = new_by_hash.get(hash) else {
            continue;
        };
        if news.len() != 1 {
            continue;
        }
        let (o, n) = (olds[0], news[0]);
        old_to_new[o] = Some(n);
        new_to_old[n] = Some(o);
        pairs.push(Pair {
            old: o,
            new: n,
            rule: Rule::UniqueHash,
        });
    }

    // -- Rule 2: repeated exact hashes, resolved only by already-matched
    // neighbors.
    //
    // SPEC GAP: "already matched neighboring blocks" does not say whether a
    // block matched by rule 2 itself becomes an anchor for another ambiguous
    // hash. Iterating to a fixpoint over hashes in sorted order is deterministic
    // and strictly more capable, so that is what is implemented.
    loop {
        let anchors: Vec<Anchor> = pairs.iter().map(|p| (p.old, p.new)).collect();
        let mut progress = false;
        for (hash, olds) in &old_by_hash {
            let free_old: Vec<usize> = olds
                .iter()
                .copied()
                .filter(|i| old_to_new[*i].is_none())
                .collect();
            if free_old.is_empty() {
                continue;
            }
            let Some(news) = new_by_hash.get(hash) else {
                continue;
            };
            let free_new: Vec<usize> = news
                .iter()
                .copied()
                .filter(|i| new_to_old[*i].is_none())
                .collect();
            if free_new.is_empty() {
                continue;
            }

            let mut by_interval_old: BTreeMap<Interval, Vec<usize>> = BTreeMap::new();
            for index in free_old {
                by_interval_old
                    .entry(interval_old(index, &anchors))
                    .or_default()
                    .push(index);
            }
            let mut by_interval_new: BTreeMap<Interval, Vec<usize>> = BTreeMap::new();
            for index in free_new {
                by_interval_new
                    .entry(interval_new(index, &anchors))
                    .or_default()
                    .push(index);
            }

            for (interval, olds_here) in &by_interval_old {
                let Some(news_here) = by_interval_new.get(interval) else {
                    continue;
                };
                // SPEC GAP: "exactly one one-to-one monotone pairing". Between
                // the same neighbors, an order-preserving bijection of k copies
                // onto k copies is unique, so equal counts resolve; unequal
                // counts admit several monotone pairings and resolve nothing.
                // Without the equal-count reading a document containing two
                // identical paragraphs would churn on every rescan.
                if olds_here.len() != news_here.len() {
                    continue;
                }
                for (o, n) in olds_here.iter().zip(news_here.iter()) {
                    old_to_new[*o] = Some(*n);
                    new_to_old[*n] = Some(*o);
                    pairs.push(Pair {
                        old: *o,
                        new: *n,
                        rule: Rule::RepeatedMonotone,
                    });
                    progress = true;
                }
            }
        }
        if !progress {
            break;
        }
    }

    // -- Rule 4: an edited block, uniquely anchored between the same two
    // immediately adjacent matched neighbors.
    let anchors: Vec<Anchor> = pairs.iter().map(|p| (p.old, p.new)).collect();
    let mut by_interval_old: BTreeMap<Interval, Vec<usize>> = BTreeMap::new();
    for (index, slot) in old_to_new.iter().enumerate() {
        if slot.is_none() {
            by_interval_old
                .entry(interval_old(index, &anchors))
                .or_default()
                .push(index);
        }
    }
    let mut by_interval_new: BTreeMap<Interval, Vec<usize>> = BTreeMap::new();
    for (index, slot) in new_to_old.iter().enumerate() {
        if slot.is_none() {
            by_interval_new
                .entry(interval_new(index, &anchors))
                .or_default()
                .push(index);
        }
    }
    for (interval, olds_here) in &by_interval_old {
        if olds_here.len() != 1 {
            continue;
        }
        let Some(news_here) = by_interval_new.get(interval) else {
            continue;
        };
        if news_here.len() != 1 {
            continue;
        }
        if policy == EdgePolicy::Strict && (interval.0.is_none() || interval.1.is_none()) {
            // Document-edge region: only one adjacent matched neighbor exists,
            // so "between the same immediately adjacent matched neighbors" is
            // not satisfied.
            continue;
        }
        let (o, n) = (olds_here[0], news_here[0]);
        old_to_new[o] = Some(n);
        new_to_old[n] = Some(o);
        pairs.push(Pair {
            old: o,
            new: n,
            rule: Rule::NeighborAnchored,
        });
    }

    // -- Rule 5: everything else.
    pairs.sort_by_key(|p| (p.old, p.new));
    let removed = (0..old.len())
        .filter(|i| old_to_new[*i].is_none())
        .collect();
    let added = (0..new.len())
        .filter(|i| new_to_old[*i].is_none())
        .collect();
    Matching {
        pairs,
        added,
        removed,
    }
}

/// Rule 1's movement test: "a matched block is `moved` when its relative order
/// against other matched blocks changed", i.e. it participates in at least one
/// inversion. Pure insertion or deletion cannot create an inversion.
fn moved_flags(pairs: &[Pair]) -> Vec<bool> {
    let mut out = vec![false; pairs.len()];
    for a in 0..pairs.len() {
        for b in 0..pairs.len() {
            if a == b {
                continue;
            }
            if (pairs[a].old < pairs[b].old) != (pairs[a].new < pairs[b].new) {
                out[a] = true;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Ledger
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Lifecycle {
    Baseline,
    Added,
    Continued,
    Removed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Flag {
    BodyChanged,
    ContextChanged,
    Moved,
}

#[derive(Debug, Clone)]
struct Event {
    snapshot: u64,
    path: String,
    lifecycle: Lifecycle,
    /// Stable logical-occurrence id; a continued block keeps the old one.
    occurrence: u64,
    /// This event's observation id.
    observation: u64,
    /// Predecessor OBSERVATION id, only when uniquely established.
    predecessor: Option<u64>,
    /// Path of the predecessor block; proves rule 3 at the event level.
    predecessor_path: Option<String>,
    flags: BTreeSet<Flag>,
    hash: String,
    rule: Option<Rule>,
    line_start: usize,
}

impl Event {
    fn has(&self, flag: Flag) -> bool {
        self.flags.contains(&flag)
    }
}

#[derive(Debug, Clone)]
struct Live {
    rec: BlockRec,
    occurrence: u64,
    observation: u64,
    /// Observed-provenance freshness: the snapshot sequence of the latest
    /// freshness-bearing event (post-baseline `added` or `body_changed`).
    /// `None` is the plan's `unknown`.
    freshness: Option<u64>,
}

struct Ledger {
    bounds: ChunkBounds,
    snapshot: u64,
    current: HashMap<String, Vec<Live>>,
    gap: HashSet<String>,
    next_occurrence: u64,
    next_observation: u64,
    all_events: Vec<Event>,
}

impl Ledger {
    fn new(bounds: ChunkBounds) -> Self {
        Ledger {
            bounds,
            snapshot: 0,
            current: HashMap::new(),
            gap: HashSet::new(),
            next_occurrence: 1,
            next_observation: 1,
            all_events: Vec::new(),
        }
    }

    fn occurrence_id(&mut self) -> u64 {
        let id = self.next_occurrence;
        self.next_occurrence += 1;
        id
    }

    fn observation_id(&mut self) -> u64 {
        let id = self.next_observation;
        self.next_observation += 1;
        id
    }

    /// One successful scan. `failed` names paths whose read/parse failed
    /// permanently: they emit no lifecycle event at all and leave the current
    /// projection.
    fn scan(&mut self, docs: &[(&str, &str)], failed: &[&str]) -> Vec<Event> {
        self.snapshot += 1;
        let mut out: Vec<Event> = Vec::new();
        let mut previous = std::mem::take(&mut self.current);
        let failed_set: HashSet<&str> = failed.iter().copied().collect();

        // "A permanent per-file failure ... emits no block lifecycle event, and
        // removes the file from the current projection. Matching never crosses
        // that failure gap."
        for path in failed {
            previous.remove(*path);
            self.gap.insert((*path).to_string());
        }

        let mut next: HashMap<String, Vec<Live>> = HashMap::new();
        for (path, source) in docs {
            if failed_set.contains(path) {
                continue;
            }
            let recs = body_blocks(path, source, &self.bounds);
            let live = match previous.remove(*path) {
                Some(prev) => self.apply_matched_path(path, prev, recs, &mut out),
                None => {
                    let post_gap = self.gap.remove(*path);
                    // Rule 5 makes a post-baseline first sighting `added`; the
                    // failure-gap rule makes a post-gap first sighting
                    // `baseline`. SPEC GAP: the plan never reconciles these two
                    // treatments of "content appears with no predecessor", and
                    // the choice decides observed freshness (see H11B).
                    let lifecycle = if self.snapshot == 1 || post_gap {
                        Lifecycle::Baseline
                    } else {
                        Lifecycle::Added
                    };
                    self.introduce(path, recs, lifecycle, &mut out)
                }
            };
            next.insert((*path).to_string(), live);
        }

        // A successful scan emits `removed` when a previous block is confirmed
        // absent. Deterministic order for reporting.
        let mut vanished: Vec<(String, Vec<Live>)> = previous.into_iter().collect();
        vanished.sort_by(|a, b| a.0.cmp(&b.0));
        for (path, prev) in vanished {
            for live in prev {
                let event = self.removal(&path, &live);
                out.push(event);
            }
        }

        self.current = next;
        self.all_events.extend(out.iter().cloned());
        out
    }

    fn introduce(
        &mut self,
        path: &str,
        recs: Vec<BlockRec>,
        lifecycle: Lifecycle,
        out: &mut Vec<Event>,
    ) -> Vec<Live> {
        let snapshot = self.snapshot;
        let mut live = Vec::with_capacity(recs.len());
        for rec in recs {
            let occurrence = self.occurrence_id();
            let observation = self.observation_id();
            // "For observed provenance, a post-baseline `added` event
            // establishes freshness at its snapshot sequence"; a baseline
            // without Git provenance is `unknown`.
            let freshness = if lifecycle == Lifecycle::Added {
                Some(snapshot)
            } else {
                None
            };
            out.push(Event {
                snapshot,
                path: path.to_string(),
                lifecycle,
                occurrence,
                observation,
                predecessor: None,
                predecessor_path: None,
                flags: BTreeSet::new(),
                hash: rec.hash.clone(),
                rule: None,
                line_start: rec.line_start,
            });
            live.push(Live {
                rec,
                occurrence,
                observation,
                freshness,
            });
        }
        live
    }

    fn removal(&mut self, path: &str, live: &Live) -> Event {
        let observation = self.observation_id();
        Event {
            snapshot: self.snapshot,
            path: path.to_string(),
            lifecycle: Lifecycle::Removed,
            occurrence: live.occurrence,
            observation,
            predecessor: Some(live.observation),
            predecessor_path: Some(live.rec.path.clone()),
            flags: BTreeSet::new(),
            hash: live.rec.hash.clone(),
            rule: None,
            line_start: live.rec.line_start,
        }
    }

    fn apply_matched_path(
        &mut self,
        path: &str,
        prev: Vec<Live>,
        recs: Vec<BlockRec>,
        out: &mut Vec<Event>,
    ) -> Vec<Live> {
        let old_recs: Vec<BlockRec> = prev.iter().map(|l| l.rec.clone()).collect();
        let matching = match_path(&old_recs, &recs, EdgePolicy::Strict);
        let moved = moved_flags(&matching.pairs);
        let snapshot = self.snapshot;
        let mut live: Vec<Option<Live>> = vec![None; recs.len()];

        for (index, pair) in matching.pairs.iter().enumerate() {
            let old = &prev[pair.old];
            let new = &recs[pair.new];
            let mut flags = BTreeSet::new();
            if old.rec.hash != new.hash {
                flags.insert(Flag::BodyChanged);
            }
            // "context_changed: nearest heading or other retrieval context
            // changed; source-offset and ordinal changes alone do not count."
            // SPEC GAP: whether ancestor breadcrumb changes count. They are
            // retrieval context (an FTS column), so they do.
            if old.rec.nearest_heading != new.nearest_heading
                || old.rec.breadcrumb != new.breadcrumb
            {
                flags.insert(Flag::ContextChanged);
            }
            if moved[index] {
                flags.insert(Flag::Moved);
            }

            if flags.is_empty() {
                // "Unchanged blocks add no rows and retain their last
                // current-observation reference."
                live[pair.new] = Some(Live {
                    rec: new.clone(),
                    ..old.clone()
                });
                continue;
            }
            let observation = self.observation_id();
            // "`body_changed` advances it. `context_changed` or `moved` alone
            // carries the prior freshness forward."
            let freshness = if flags.contains(&Flag::BodyChanged) {
                Some(snapshot)
            } else {
                old.freshness
            };
            out.push(Event {
                snapshot,
                path: path.to_string(),
                lifecycle: Lifecycle::Continued,
                occurrence: old.occurrence,
                observation,
                predecessor: Some(old.observation),
                predecessor_path: Some(old.rec.path.clone()),
                flags,
                hash: new.hash.clone(),
                rule: Some(pair.rule),
                line_start: new.line_start,
            });
            live[pair.new] = Some(Live {
                rec: new.clone(),
                occurrence: old.occurrence,
                observation,
                freshness,
            });
        }

        for &index in &matching.added {
            let rec = recs[index].clone();
            let occurrence = self.occurrence_id();
            let observation = self.observation_id();
            out.push(Event {
                snapshot,
                path: path.to_string(),
                lifecycle: Lifecycle::Added,
                occurrence,
                observation,
                predecessor: None,
                predecessor_path: None,
                flags: BTreeSet::new(),
                hash: rec.hash.clone(),
                rule: None,
                line_start: rec.line_start,
            });
            live[index] = Some(Live {
                rec,
                occurrence,
                observation,
                freshness: Some(snapshot),
            });
        }

        for &index in &matching.removed {
            let event = self.removal(path, &prev[index]);
            out.push(event);
        }

        live.into_iter()
            .map(|slot| slot.expect("every new block is paired or added"))
            .collect()
    }

    fn live(&self, path: &str) -> &[Live] {
        self.current.get(path).map(|v| v.as_slice()).unwrap_or(&[])
    }
}

// ---------------------------------------------------------------------------
// Small helpers used by the assertions
// ---------------------------------------------------------------------------

fn for_path<'a>(events: &'a [Event], path: &str) -> Vec<&'a Event> {
    events.iter().filter(|event| event.path == path).collect()
}

fn count(events: &[Event], lifecycle: Lifecycle) -> usize {
    events
        .iter()
        .filter(|event| event.lifecycle == lifecycle)
        .count()
}

fn summary(events: &[Event]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for event in events {
        let flags: Vec<String> = event.flags.iter().map(|f| format!("{f:?}")).collect();
        parts.push(format!(
            "{:?}@{}:{} line {} flags[{}] pred {:?} rule {:?}",
            event.lifecycle,
            event.path,
            event.snapshot,
            event.line_start,
            flags.join("+"),
            event.predecessor,
            event.rule
        ));
    }
    parts.join("\n    ")
}

/// Bounds that force several body blocks into one chunk, so an insertion
/// visibly regroups the retrieval projection.
fn tight_bounds() -> ChunkBounds {
    ChunkBounds {
        target: 80,
        normal_max: 120,
        ..ChunkBounds::default()
    }
}

// ---------------------------------------------------------------------------
// H1 - the headline claim
// ---------------------------------------------------------------------------

const H1_BEFORE: &str = "\
# Guide

Alpha paragraph about installation.

Beta paragraph about configuration.

Gamma paragraph about deployment.

Delta paragraph about rollback.
";

const H1_AFTER: &str = "\
# Guide

Alpha paragraph about installation.

Zeta paragraph about the brand new uniquely distinguishable step.

Beta paragraph about configuration.

Gamma paragraph about deployment.

Delta paragraph about rollback.
";

#[test]
fn h1_one_insert_yields_one_added_event_even_when_chunks_regroup() {
    let bounds = tight_bounds();
    let mut ledger = Ledger::new(bounds);

    let baseline = ledger.scan(&[("docs/guide.md", H1_BEFORE)], &[]);
    assert_eq!(
        baseline.len(),
        4,
        "four body blocks at baseline:\n    {}",
        summary(&baseline)
    );
    assert!(baseline.iter().all(|e| e.lifecycle == Lifecycle::Baseline));

    let events = ledger.scan(&[("docs/guide.md", H1_AFTER)], &[]);

    // The headline acceptance claim: exactly one `added` observation and no
    // event of any kind for the untouched blocks.
    assert_eq!(
        events.len(),
        1,
        "expected exactly one event, got:\n    {}",
        summary(&events)
    );
    assert_eq!(events[0].lifecycle, Lifecycle::Added);
    assert!(
        events[0].flags.is_empty(),
        "an insertion carries no change flag"
    );
    assert!(
        events[0].predecessor.is_none(),
        "an added block has no predecessor"
    );
    assert_eq!(count(&events, Lifecycle::Removed), 0);
    assert_eq!(count(&events, Lifecycle::Continued), 0);

    // "Line, byte-span, or ordinal shifts caused only by an insertion or
    // deletion are not movement": the three blocks after the insertion shift by
    // two lines and one ordinal each and still produce no `moved` row.
    let live = ledger.live("docs/guide.md");
    assert_eq!(live.len(), 5);
    let before_lines: Vec<usize> = body_blocks("docs/guide.md", H1_BEFORE, &bounds)
        .iter()
        .map(|r| r.line_start)
        .collect();
    let after_lines: Vec<usize> = live.iter().map(|l| l.rec.line_start).collect();
    assert_eq!(before_lines, vec![3, 5, 7, 9], "Alpha Beta Gamma Delta");
    assert_eq!(
        after_lines,
        vec![3, 5, 7, 9, 11],
        "Zeta inserted at line 5, the rest shifted"
    );
    assert_eq!(live[2].rec.raw, "Beta paragraph about configuration.");

    // ... and the retrieval projection really did regroup underneath.
    let before = md::index_document("docs/guide.md", H1_BEFORE, &bounds);
    let after = md::index_document("docs/guide.md", H1_AFTER, &bounds);
    let ids_before: HashSet<&str> = before
        .chunks
        .iter()
        .map(|c| c.embedding_identity.as_str())
        .collect();
    let ids_after: HashSet<&str> = after
        .chunks
        .iter()
        .map(|c| c.embedding_identity.as_str())
        .collect();
    let survivors = ids_before.intersection(&ids_after).count();
    println!(
        "H1: chunks {} -> {}, surviving embedding identities {}, block events {}",
        before.chunks.len(),
        after.chunks.len(),
        survivors,
        events.len()
    );
    assert!(
        before.chunks.len() != after.chunks.len() || survivors < ids_before.len(),
        "the fixture must actually regroup chunks, otherwise H1 proves nothing"
    );
    assert_eq!(
        survivors,
        0,
        "every chunk was rebuilt ({} -> {}), yet only one block event fired",
        before.chunks.len(),
        after.chunks.len()
    );
}

// ---------------------------------------------------------------------------
// H2 - duplicates
// ---------------------------------------------------------------------------

const H2_BEFORE: &str = "\
# Notes

Shared boilerplate line.

Unique first paragraph.

Shared boilerplate line.

Unique second paragraph.
";

const H2_AFTER: &str = "\
# Notes

Shared boilerplate line.

Unique first paragraph.

Shared boilerplate line.

Shared boilerplate line.

Unique second paragraph.
";

#[test]
fn h2_ambiguous_duplicates_receive_no_predecessor() {
    let mut ledger = Ledger::new(ChunkBounds::default());
    ledger.scan(&[("docs/notes.md", H2_BEFORE)], &[]);
    let events = ledger.scan(&[("docs/notes.md", H2_AFTER)], &[]);
    println!("H2 events:\n    {}", summary(&events));

    // No predecessor edge is created for any ambiguous copy.
    assert!(
        events.iter().all(|e| e.lifecycle != Lifecycle::Continued),
        "an ambiguous duplicate must not be continued:\n    {}",
        summary(&events)
    );
    assert!(events.iter().all(|e| e.rule.is_none()));

    // The first "Shared boilerplate line." copy IS resolved: its interval
    // (document start .. "Unique first paragraph.") holds one copy on each
    // side, which is exactly one monotone pairing. It is unchanged, so it adds
    // no row. The ambiguity is confined to the second interval.
    assert_eq!(
        count(&events, Lifecycle::Added),
        2,
        "both copies in the ambiguous gap are added"
    );
    assert_eq!(
        count(&events, Lifecycle::Removed),
        1,
        "the pre-existing copy is removed"
    );
}

#[test]
fn h2b_duplicating_a_block_reports_the_untouched_original_as_removed_and_readded() {
    let mut ledger = Ledger::new(ChunkBounds::default());
    ledger.scan(&[("docs/notes.md", H2_BEFORE)], &[]);
    let before_live: Vec<Live> = ledger.live("docs/notes.md").to_vec();
    let original = before_live[2].clone();
    assert_eq!(original.rec.line_start, 7, "the second boilerplate copy");
    assert_eq!(original.freshness, None, "baseline provenance is unknown");

    let events = ledger.scan(&[("docs/notes.md", H2_AFTER)], &[]);

    // PLAN CLAIM (storage model): "Unchanged blocks add no rows and retain
    // their last current-observation reference."
    // OBSERVED: appending a third identical copy makes the *untouched* second
    // copy ambiguous, so it is reported `removed` and its content reappears as
    // a brand-new `added` occurrence with a new logical id.
    let removed: Vec<&Event> = events
        .iter()
        .filter(|e| e.lifecycle == Lifecycle::Removed)
        .collect();
    assert_eq!(removed.len(), 1);
    assert_eq!(
        removed[0].occurrence, original.occurrence,
        "the untouched block was retired"
    );
    assert_eq!(removed[0].hash, original.rec.hash);

    // ... and the replacement occurrences are maximally fresh even though
    // nobody authored anything at that position.
    let live = ledger.live("docs/notes.md");
    let fresh: Vec<Option<u64>> = live.iter().map(|l| l.freshness).collect();
    println!(
        "H2B freshness after duplication: {fresh:?} (snapshot {})",
        ledger.snapshot
    );
    assert_eq!(
        live[2].freshness,
        Some(2),
        "the untouched copy is now 'observed at snapshot 2'"
    );
    assert_eq!(live[3].freshness, Some(2));
    assert!(
        live.iter().all(|l| l.occurrence != original.occurrence),
        "the original logical occurrence is gone from the projection"
    );
}

// ---------------------------------------------------------------------------
// H3 - split and merge
// ---------------------------------------------------------------------------

const H3_WHOLE: &str = "\
# Doc

Anchor top paragraph.

The original sentence one. The original sentence two.

Anchor bottom paragraph.
";

const H3_SPLIT: &str = "\
# Doc

Anchor top paragraph.

The original sentence one.

The original sentence two.

Anchor bottom paragraph.
";

#[test]
fn h3_split_and_merge_produce_no_predecessor() {
    let mut split_ledger = Ledger::new(ChunkBounds::default());
    split_ledger.scan(&[("docs/doc.md", H3_WHOLE)], &[]);
    let split = split_ledger.scan(&[("docs/doc.md", H3_SPLIT)], &[]);
    println!("H3 split events:\n    {}", summary(&split));
    assert_eq!(count(&split, Lifecycle::Removed), 1);
    assert_eq!(count(&split, Lifecycle::Added), 2);
    assert_eq!(
        count(&split, Lifecycle::Continued),
        0,
        "one old vs two new is not rule 4"
    );
    assert!(split
        .iter()
        .all(|e| e.predecessor.is_none() || e.lifecycle == Lifecycle::Removed));

    let mut merge_ledger = Ledger::new(ChunkBounds::default());
    merge_ledger.scan(&[("docs/doc.md", H3_SPLIT)], &[]);
    let merge = merge_ledger.scan(&[("docs/doc.md", H3_WHOLE)], &[]);
    println!("H3 merge events:\n    {}", summary(&merge));
    assert_eq!(count(&merge, Lifecycle::Removed), 2);
    assert_eq!(count(&merge, Lifecycle::Added), 1);
    assert_eq!(
        count(&merge, Lifecycle::Continued),
        0,
        "two old vs one new is not rule 4"
    );

    // The two anchors on either side never move and never emit a row.
    assert!(split.iter().all(|e| !e.has(Flag::Moved)));
    assert!(merge.iter().all(|e| !e.has(Flag::Moved)));
}

// ---------------------------------------------------------------------------
// H4 / H9 - cross-path
// ---------------------------------------------------------------------------

const RENAME_SOURCE: &str = "\
# Deployment runbook

Rotate the signing key before the release train departs.

Announce the freeze window in the operations channel.

Verify the rollback artifact is reachable from the mirror.
";

#[test]
fn h4_pure_rename_creates_no_cross_path_predecessor() {
    let mut ledger = Ledger::new(ChunkBounds::default());
    ledger.scan(&[("docs/runbook.md", RENAME_SOURCE)], &[]);
    let before: Vec<Live> = ledger.live("docs/runbook.md").to_vec();

    // Every hash is globally unique, so a cross-path matcher would have no
    // ambiguity to hide behind.
    let hashes: HashSet<&str> = before.iter().map(|l| l.rec.hash.as_str()).collect();
    assert_eq!(
        hashes.len(),
        before.len(),
        "fixture blocks must be globally unique"
    );

    let events = ledger.scan(&[("docs/ops/runbook.md", RENAME_SOURCE)], &[]);
    println!("H4 events:\n    {}", summary(&events));

    assert_eq!(
        count(&events, Lifecycle::Added),
        3,
        "every block at the new path is added"
    );
    assert_eq!(
        count(&events, Lifecycle::Removed),
        3,
        "every block at the old path is removed"
    );
    assert_eq!(count(&events, Lifecycle::Continued), 0);
    assert!(
        events
            .iter()
            .filter(|e| e.lifecycle == Lifecycle::Added)
            .all(|e| e.predecessor.is_none()),
        "no cross-path predecessor edge exists"
    );

    // The content is byte-identical across the rename, which is exactly what a
    // cross-path matcher would have used.
    let after: Vec<&str> = ledger
        .live("docs/ops/runbook.md")
        .iter()
        .map(|l| l.rec.hash.as_str())
        .collect();
    let old: Vec<&str> = before.iter().map(|l| l.rec.hash.as_str()).collect();
    assert_eq!(after, old, "identical hashes, still no succession");
}

#[test]
fn h9_moved_by_changed_path_is_unreachable() {
    // PLAN CLAIM: "`moved`: the matched block changed path or reordered
    // relative to matched neighboring blocks."
    // OBSERVED: rule 3 confines matching to one path, so a matched pair always
    // shares its path and the "changed path" half of the definition can never
    // fire. The assertion below is the proof at the event level.
    let mut ledger = Ledger::new(ChunkBounds::default());
    ledger.scan(&[("a.md", RENAME_SOURCE), ("keep.md", H3_WHOLE)], &[]);
    ledger.scan(&[("b.md", RENAME_SOURCE), ("keep.md", H3_WHOLE)], &[]);
    ledger.scan(&[("c/d.md", RENAME_SOURCE), ("keep.md", H3_WHOLE)], &[]);

    let continued: Vec<&Event> = ledger
        .all_events
        .iter()
        .filter(|e| e.lifecycle == Lifecycle::Continued)
        .collect();
    println!(
        "H9: {} continued events across three renames",
        continued.len()
    );
    assert!(continued.is_empty(), "a rename never continues a block");

    for event in &ledger.all_events {
        if let Some(pred_path) = &event.predecessor_path {
            assert_eq!(
                pred_path, &event.path,
                "no event ever links two different paths (rule 3)"
            );
        }
    }
    assert!(
        !ledger.all_events.iter().any(|e| e.has(Flag::Moved)),
        "no `moved` flag was ever set by a path change"
    );
}

#[test]
fn h9b_one_relocation_flags_every_block_it_passed_as_moved() {
    const BEFORE: &str = "\
# Doc

Para one.

Para two.

Para three.

Para four.
";
    const AFTER: &str = "\
# Doc

Para two.

Para three.

Para four.

Para one.
";
    let mut ledger = Ledger::new(ChunkBounds::default());
    ledger.scan(&[("docs/order.md", BEFORE)], &[]);
    let events = ledger.scan(&[("docs/order.md", AFTER)], &[]);
    println!("H9B events:\n    {}", summary(&events));

    // PLAN CLAIM: "a matched block is `moved` when its relative order against
    // other matched blocks changed" -- no minimal-inversion (longest increasing
    // subsequence) reading is specified.
    // OBSERVED: relocating ONE paragraph flags all four matched blocks as
    // moved, so one edit writes four ledger rows for three blocks that did not
    // move. A minimal reading would have written one.
    assert_eq!(events.len(), 4);
    assert!(events.iter().all(|e| e.lifecycle == Lifecycle::Continued));
    assert!(events.iter().all(|e| e.has(Flag::Moved)));
    assert!(
        events.iter().all(|e| !e.has(Flag::BodyChanged)),
        "a move must not claim a body change"
    );
    // Move-only carries prior freshness forward: all four stay `unknown`.
    assert!(ledger
        .live("docs/order.md")
        .iter()
        .all(|l| l.freshness.is_none()));
}

// ---------------------------------------------------------------------------
// H5 - neighbor-anchored edits
// ---------------------------------------------------------------------------

const H5_BEFORE: &str = "\
# Doc

Anchor alpha stays put.

Middle paragraph original text.

Anchor omega stays put.
";

const H5_ONE_EDIT: &str = "\
# Doc

Anchor alpha stays put.

Middle paragraph revised text.

Anchor omega stays put.
";

const H5_TWO_BEFORE: &str = "\
# Doc

Anchor alpha stays put.

First middle paragraph original.

Second middle paragraph original.

Anchor omega stays put.
";

const H5_TWO_EDITS: &str = "\
# Doc

Anchor alpha stays put.

First middle paragraph revised.

Second middle paragraph revised.

Anchor omega stays put.
";

#[test]
fn h5_edit_gets_predecessor_only_when_uniquely_neighbor_anchored() {
    let mut one = Ledger::new(ChunkBounds::default());
    one.scan(&[("docs/doc.md", H5_BEFORE)], &[]);
    let before = one.live("docs/doc.md")[1].clone();
    let events = one.scan(&[("docs/doc.md", H5_ONE_EDIT)], &[]);
    println!("H5 single edit:\n    {}", summary(&events));

    assert_eq!(events.len(), 1, "exactly one row for one edit");
    assert_eq!(events[0].lifecycle, Lifecycle::Continued);
    assert_eq!(events[0].rule, Some(Rule::NeighborAnchored));
    assert!(events[0].has(Flag::BodyChanged));
    assert!(!events[0].has(Flag::Moved) && !events[0].has(Flag::ContextChanged));
    assert_eq!(
        events[0].predecessor,
        Some(before.observation),
        "the predecessor is unique"
    );
    assert_eq!(
        events[0].occurrence, before.occurrence,
        "the logical occurrence survives an edit"
    );
    assert_ne!(
        events[0].observation, before.observation,
        "an edit appends a new observation"
    );
    assert_eq!(
        one.live("docs/doc.md")[1].observation,
        events[0].observation,
        "the projection points at the new observation"
    );
    assert_eq!(
        one.live("docs/doc.md")[1].freshness,
        Some(2),
        "body_changed advances freshness"
    );

    // Two edits inside ONE gap: two unmatched old and two unmatched new blocks
    // between the same neighbors, so neither is uniquely anchored.
    let mut two = Ledger::new(ChunkBounds::default());
    two.scan(&[("docs/doc.md", H5_TWO_BEFORE)], &[]);
    let events = two.scan(&[("docs/doc.md", H5_TWO_EDITS)], &[]);
    println!("H5 two edits in one gap:\n    {}", summary(&events));
    assert_eq!(
        count(&events, Lifecycle::Continued),
        0,
        "ambiguous gap yields no predecessor"
    );
    assert_eq!(count(&events, Lifecycle::Added), 2);
    assert_eq!(count(&events, Lifecycle::Removed), 2);
}

#[test]
fn h5b_heading_rename_is_context_only_and_carries_freshness_forward() {
    const BEFORE: &str = "\
# Doc

## Old heading

Body paragraph under the heading.
";
    let mut ledger = Ledger::new(ChunkBounds::default());
    ledger.scan(&[("docs/h.md", BEFORE)], &[]);
    // Make the body freshness-bearing first so "carries forward" is observable.
    let edited = "# Doc\n\n## Old heading\n\nBody paragraph under the heading, revised.\n";
    ledger.scan(&[("docs/h.md", edited)], &[]);
    assert_eq!(ledger.live("docs/h.md")[0].freshness, Some(2));

    let renamed = "# Doc\n\n## New heading\n\nBody paragraph under the heading, revised.\n";
    let events = ledger.scan(&[("docs/h.md", renamed)], &[]);
    println!("H5B heading rename:\n    {}", summary(&events));
    assert_eq!(events.len(), 1);
    assert!(events[0].has(Flag::ContextChanged));
    assert!(
        !events[0].has(Flag::BodyChanged),
        "a heading rename is not an authored body change"
    );
    assert_eq!(
        ledger.live("docs/h.md")[0].freshness,
        Some(2),
        "context-only keeps the prior freshness (still snapshot 2, not 3)"
    );
    // Rule 1 matched it "independent of heading text", as specified.
    assert_eq!(events[0].rule, Some(Rule::UniqueHash));
}

// ---------------------------------------------------------------------------
// H6 - document-edge edits
// ---------------------------------------------------------------------------

const H6_BEFORE: &str = "\
# Doc

First paragraph original text.

Stable middle paragraph.

Last paragraph original text.
";

const H6_FIRST_EDITED: &str = "\
# Doc

First paragraph revised text.

Stable middle paragraph.

Last paragraph original text.
";

const H6_LAST_EDITED: &str = "\
# Doc

First paragraph original text.

Stable middle paragraph.

Last paragraph revised text.
";

#[test]
fn h6_document_edge_edits_stay_unmatched_though_only_one_pairing_exists() {
    let bounds = ChunkBounds::default();
    for (label, after) in [("first", H6_FIRST_EDITED), ("last", H6_LAST_EDITED)] {
        let mut ledger = Ledger::new(bounds);
        ledger.scan(&[("docs/edge.md", H6_BEFORE)], &[]);
        let events = ledger.scan(&[("docs/edge.md", after)], &[]);
        println!("H6 {label}-block edit:\n    {}", summary(&events));

        // Rule 4 as written needs matched neighbors on BOTH sides, which a
        // document-edge region does not have.
        assert_eq!(
            count(&events, Lifecycle::Continued),
            0,
            "{label}: no predecessor at the edge"
        );
        assert_eq!(count(&events, Lifecycle::Added), 1);
        assert_eq!(count(&events, Lifecycle::Removed), 1);

        // PLAN CLAIM: "If duplicate content, multiple valid monotone pairings,
        // document-edge edits, ... leaves MORE THAN ONE predecessor or
        // successor possible, no predecessor is recorded."
        // OBSERVED: at a document edge exactly ONE pairing is possible. The
        // lenient run below -- identical input, sentinel edges -- finds it and
        // is unique. The conservatism is an artifact of the rule's wording, not
        // of ambiguity, and the plan's stated justification does not hold.
        let old = body_blocks("docs/edge.md", H6_BEFORE, &bounds);
        let new = body_blocks("docs/edge.md", after, &bounds);
        let strict = match_path(&old, &new, EdgePolicy::Strict);
        let lenient = match_path(&old, &new, EdgePolicy::Lenient);
        assert_eq!(
            strict.pairs.len(),
            2,
            "{label}: only the two untouched blocks match"
        );
        assert_eq!(
            lenient.pairs.len(),
            3,
            "{label}: the edge edit has exactly one candidate"
        );
        let edge = lenient
            .pairs
            .iter()
            .find(|p| p.rule == Rule::NeighborAnchored)
            .unwrap();
        println!(
            "H6 {label}: lenient pairing old#{} -> new#{} is unique (strict leaves it unmatched)",
            edge.old, edge.new
        );

        // The freshness outcome is the same either way (`added` and
        // `body_changed` both establish freshness at this snapshot), so the
        // cost of the strict reading is the lost lineage edge and a spurious
        // `removed`, not a wrong recency.
        assert_eq!(
            ledger
                .live("docs/edge.md")
                .iter()
                .filter(|l| l.freshness == Some(2))
                .count(),
            1
        );
    }
}

// ---------------------------------------------------------------------------
// H7 - the permanent per-file failure gap
// ---------------------------------------------------------------------------

#[test]
fn h7_failure_gap_emits_no_events_and_inherits_no_freshness() {
    const V1: &str = "\
# Runbook

Step one of the procedure.

Step two of the procedure.

Step three of the procedure.
";
    const V2: &str = "\
# Runbook

Step one of the procedure.

Step two of the procedure, corrected.

Step three of the procedure.
";
    let mut ledger = Ledger::new(ChunkBounds::default());
    ledger.scan(&[("docs/a.md", V1), ("docs/b.md", H3_WHOLE)], &[]);
    let edit = ledger.scan(&[("docs/a.md", V2), ("docs/b.md", H3_WHOLE)], &[]);
    assert_eq!(
        count(&edit, Lifecycle::Continued),
        1,
        "a neighbor-anchored edit:\n    {}",
        summary(&edit)
    );

    let before_gap: Vec<Live> = ledger.live("docs/a.md").to_vec();
    assert_eq!(
        before_gap[1].freshness,
        Some(2),
        "the corrected step is fresh at snapshot 2"
    );
    let old_occurrences: HashSet<u64> = before_gap.iter().map(|l| l.occurrence).collect();
    let old_hashes: Vec<String> = before_gap.iter().map(|l| l.rec.hash.clone()).collect();

    // Snapshot 3: a permanent per-file failure.
    let during = ledger.scan(&[("docs/b.md", H3_WHOLE)], &["docs/a.md"]);
    println!(
        "H7 failing scan events: {}",
        if during.is_empty() {
            "<none>".into()
        } else {
            summary(&during)
        }
    );
    assert!(
        during.is_empty(),
        "a rejection emits no block lifecycle event, not even `removed`"
    );
    assert!(
        ledger.live("docs/a.md").is_empty(),
        "the file left the current projection"
    );
    assert!(
        !ledger
            .all_events
            .iter()
            .any(|e| e.path == "docs/a.md" && e.lifecycle == Lifecycle::Removed),
        "no `removed` was ever recorded for the rejected file"
    );

    // Snapshot 4: the same bytes parse again.
    let after = ledger.scan(&[("docs/a.md", V2), ("docs/b.md", H3_WHOLE)], &[]);
    println!("H7 recovery events:\n    {}", summary(&after));
    let recovered = for_path(&after, "docs/a.md");
    assert_eq!(recovered.len(), 3);
    assert!(
        recovered.iter().all(|e| e.lifecycle == Lifecycle::Baseline),
        "post-gap is baseline"
    );
    assert!(
        recovered.iter().all(|e| e.predecessor.is_none()),
        "matching never crossed the gap"
    );
    assert!(
        recovered
            .iter()
            .all(|e| !old_occurrences.contains(&e.occurrence)),
        "new logical-occurrence ids"
    );

    // The bytes are identical to snapshot 2, so continuity existed in reality
    // and was deliberately not claimed.
    let now: Vec<String> = ledger
        .live("docs/a.md")
        .iter()
        .map(|l| l.rec.hash.clone())
        .collect();
    assert_eq!(now, old_hashes, "identical content across the gap");
    assert!(
        ledger
            .live("docs/a.md")
            .iter()
            .all(|l| l.freshness.is_none()),
        "freshness is `unknown` after the gap: snapshot-2 recency was NOT inherited"
    );
}

// ---------------------------------------------------------------------------
// H10 - repeated identical scans
// ---------------------------------------------------------------------------

#[test]
fn h10_repeated_identical_scans_add_no_rows() {
    let docs: &[(&str, &str)] = &[
        ("docs/guide.md", H1_BEFORE),
        // Duplicated content stresses rule 2: without the equal-count monotone
        // reading these blocks would churn on every scan.
        ("docs/notes.md", H2_BEFORE),
        ("docs/doc.md", H3_WHOLE),
    ];
    let mut ledger = Ledger::new(ChunkBounds::default());
    let baseline = ledger.scan(docs, &[]);
    assert!(baseline.iter().all(|e| e.lifecycle == Lifecycle::Baseline));
    let baseline_count = baseline.len();

    for round in 2..=4 {
        let events = ledger.scan(docs, &[]);
        assert!(
            events.is_empty(),
            "scan {round} of identical content emitted rows:\n    {}",
            summary(&events)
        );
    }
    println!("H10: {baseline_count} baseline rows, then 0 rows across 3 identical rescans");
    assert_eq!(ledger.all_events.len(), baseline_count);
    // Occurrence and observation ids are untouched by a no-op scan.
    assert_eq!(ledger.next_observation as usize, baseline_count + 1);
}

// ---------------------------------------------------------------------------
// H11 - rename and freshness in a non-Git repository
// ---------------------------------------------------------------------------

#[test]
fn h11_pure_rename_makes_an_untouched_file_maximally_fresh() {
    let bounds = ChunkBounds::default();
    let mut ledger = Ledger::new(bounds);
    ledger.scan(
        &[
            ("docs/runbook.md", RENAME_SOURCE),
            ("docs/other.md", H5_BEFORE),
        ],
        &[],
    );

    // Snapshot 2: a genuine edit somewhere else, the newest real authoring.
    ledger.scan(
        &[
            ("docs/runbook.md", RENAME_SOURCE),
            ("docs/other.md", H5_ONE_EDIT),
        ],
        &[],
    );
    let genuine = ledger.live("docs/other.md")[1].clone();
    assert_eq!(
        genuine.freshness,
        Some(2),
        "the only genuinely authored change"
    );
    assert!(ledger
        .live("docs/runbook.md")
        .iter()
        .all(|l| l.freshness.is_none()));

    // Snapshot 3: a pure rename. Not one byte of content changed.
    let events = ledger.scan(
        &[
            ("docs/ops/runbook.md", RENAME_SOURCE),
            ("docs/other.md", H5_ONE_EDIT),
        ],
        &[],
    );
    println!("H11 rename events:\n    {}", summary(&events));

    let renamed = ledger.live("docs/ops/runbook.md").to_vec();
    assert_eq!(renamed.len(), 3);
    // PLAN (entry, point 4): "Without usable Git provenance, a pure rename
    // therefore restarts observed freshness for every block."
    // OBSERVED: confirmed. Every block of an untouched file becomes the newest
    // observed content in the corpus, outranking the one block that a human
    // actually edited.
    assert!(
        renamed.iter().all(|l| l.freshness == Some(3)),
        "every renamed block is fresh at the rename snapshot: {:?}",
        renamed.iter().map(|l| l.freshness).collect::<Vec<_>>()
    );
    assert!(
        renamed.iter().all(|l| l.freshness > genuine.freshness),
        "the untouched renamed file is now strictly newer than the real edit"
    );
    let max = ledger
        .current
        .values()
        .flatten()
        .filter_map(|l| l.freshness)
        .max()
        .unwrap();
    assert_eq!(max, 3, "maximally fresh in the whole corpus");

    // The same rename is a no-op for the vector cache: the plan's embedding
    // identity excludes the path, so every vector is reused while the ledger
    // treats every block as newly authored.
    let before = md::index_document("docs/runbook.md", RENAME_SOURCE, &bounds);
    let after = md::index_document("docs/ops/runbook.md", RENAME_SOURCE, &bounds);
    let ids_before: Vec<&str> = before
        .chunks
        .iter()
        .map(|c| c.embedding_identity.as_str())
        .collect();
    let ids_after: Vec<&str> = after
        .chunks
        .iter()
        .map(|c| c.embedding_identity.as_str())
        .collect();
    assert_eq!(
        ids_before, ids_after,
        "identical embedding identities across the rename"
    );
    println!(
        "H11: {} vectors reused unchanged, {} blocks re-dated to snapshot 3",
        ids_after.len(),
        renamed.len()
    );
}

#[test]
fn h11b_gap_recovery_and_rename_disagree_on_lifecycle_for_the_same_situation() {
    // Both scans face the identical epistemic situation: known content appears
    // with no in-projection predecessor. The plan resolves them oppositely.
    let bounds = ChunkBounds::default();

    let mut renamed = Ledger::new(bounds);
    renamed.scan(&[("docs/x.md", RENAME_SOURCE)], &[]);
    let rename_events = renamed.scan(&[("docs/y.md", RENAME_SOURCE)], &[]);

    let mut gapped = Ledger::new(bounds);
    gapped.scan(&[("docs/x.md", RENAME_SOURCE)], &[]);
    gapped.scan(&[], &["docs/x.md"]);
    let gap_events = gapped.scan(&[("docs/x.md", RENAME_SOURCE)], &[]);

    let rename_new: Vec<&Event> = rename_events
        .iter()
        .filter(|e| e.path == "docs/y.md")
        .collect();
    assert!(rename_new.iter().all(|e| e.lifecycle == Lifecycle::Added));
    assert!(gap_events
        .iter()
        .all(|e| e.lifecycle == Lifecycle::Baseline));

    let rename_fresh: Vec<Option<u64>> = renamed
        .live("docs/y.md")
        .iter()
        .map(|l| l.freshness)
        .collect();
    let gap_fresh: Vec<Option<u64>> = gapped
        .live("docs/x.md")
        .iter()
        .map(|l| l.freshness)
        .collect();
    println!("H11B rename freshness {rename_fresh:?} vs gap-recovery freshness {gap_fresh:?}");

    // OBSERVED: identical inputs, opposite freshness. Rule 5 makes the renamed
    // blocks maximally fresh; the failure-gap rule makes the recovered blocks
    // `unknown`.
    assert!(rename_fresh.iter().all(|f| *f == Some(2)));
    assert!(gap_fresh.iter().all(|f| f.is_none()));
}

// ---------------------------------------------------------------------------
// H8B - a deterministic false succession
// ---------------------------------------------------------------------------

#[test]
fn h8b_rule_four_asserts_succession_between_an_unrelated_delete_and_insert() {
    const BEFORE: &str = "\
# Doc

Anchor alpha stays put.

A paragraph about certificate rotation that is deleted this round.

Anchor omega stays put.
";
    const AFTER: &str = "\
# Doc

Anchor alpha stays put.

An unrelated new paragraph about lunch orders on Fridays.

Anchor omega stays put.
";
    let mut ledger = Ledger::new(ChunkBounds::default());
    ledger.scan(&[("docs/doc.md", BEFORE)], &[]);
    let events = ledger.scan(&[("docs/doc.md", AFTER)], &[]);
    println!("H8B events:\n    {}", summary(&events));

    // PLAN CLAIM: "Ordinal position alone never establishes continuity ...
    // False succession is worse than missing succession."
    // OBSERVED: an independent deletion and an independent insertion that land
    // in the same gap are recorded as ONE continued occurrence with an explicit
    // predecessor edge and `body_changed`. The two documents are
    // indistinguishable from a real edit, so rule 4 cannot avoid this; the
    // claim that no false succession is asserted is therefore false as stated.
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].lifecycle, Lifecycle::Continued);
    assert_eq!(events[0].rule, Some(Rule::NeighborAnchored));
    assert!(events[0].has(Flag::BodyChanged));
    assert!(
        events[0].predecessor.is_some(),
        "a predecessor edge to an unrelated block"
    );
}

// ---------------------------------------------------------------------------
// H8 - property test
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct GenBlock {
    truth: u64,
    text: String,
}

#[derive(Debug, Clone)]
enum Op {
    Insert(usize, usize),
    Delete(usize),
    Edit(usize, u64),
    Relocate(usize, usize),
    Duplicate(usize, usize),
}

fn render(blocks: &[GenBlock]) -> String {
    let mut out = String::from("# Random document\n\n");
    for block in blocks {
        out.push_str(&block.text);
        out.push_str("\n\n");
    }
    out
}

fn base_text(word: usize) -> String {
    format!("Paragraph about subject {word} and what it means for the reader.")
}

fn random_doc(rng: &mut StdRng, pool: usize, next_truth: &mut u64) -> Vec<GenBlock> {
    let count = rng.random_range(3..9);
    (0..count)
        .map(|_| {
            let truth = *next_truth;
            *next_truth += 1;
            GenBlock {
                truth,
                text: base_text(rng.random_range(0..pool)),
            }
        })
        .collect()
}

fn random_ops(rng: &mut StdRng, pool: usize, len: usize, next_truth: &mut u64) -> Vec<Op> {
    let count = rng.random_range(1..5);
    let mut ops = Vec::new();
    let mut size = len;
    for _ in 0..count {
        if size == 0 {
            break;
        }
        let op = match rng.random_range(0..5) {
            0 => {
                let truth = *next_truth;
                *next_truth += 1;
                size += 1;
                let _ = truth;
                Op::Insert(rng.random_range(0..=size - 1), rng.random_range(0..pool))
            }
            1 => {
                let op = Op::Delete(rng.random_range(0..size));
                size -= 1;
                op
            }
            2 => Op::Edit(rng.random_range(0..size), rng.random_range(0..1_000_000u64)),
            3 => Op::Relocate(rng.random_range(0..size), rng.random_range(0..size)),
            _ => {
                let op = Op::Duplicate(rng.random_range(0..size), rng.random_range(0..=size));
                size += 1;
                op
            }
        };
        ops.push(op);
    }
    ops
}

fn apply(blocks: &mut Vec<GenBlock>, ops: &[Op], next_truth: &mut u64) {
    for op in ops {
        match op {
            Op::Insert(at, word) => {
                let truth = *next_truth;
                *next_truth += 1;
                let at = (*at).min(blocks.len());
                blocks.insert(
                    at,
                    GenBlock {
                        truth,
                        text: base_text(*word),
                    },
                );
            }
            Op::Delete(at) => {
                if *at < blocks.len() {
                    blocks.remove(*at);
                }
            }
            Op::Edit(at, nonce) => {
                if *at < blocks.len() {
                    blocks[*at].text = format!("Paragraph rewritten with nonce {nonce}.");
                }
            }
            Op::Relocate(from, to) => {
                if *from < blocks.len() {
                    let block = blocks.remove(*from);
                    let to = (*to).min(blocks.len());
                    blocks.insert(to, block);
                }
            }
            Op::Duplicate(from, to) => {
                if *from < blocks.len() {
                    let truth = *next_truth;
                    *next_truth += 1;
                    let text = blocks[*from].text.clone();
                    let to = (*to).min(blocks.len());
                    blocks.insert(to, GenBlock { truth, text });
                }
            }
        }
    }
}

#[derive(Debug, Default)]
struct Stats {
    trials: usize,
    pairs: usize,
    false_successions: usize,
    trials_with_false: usize,
    indistinguishable_swaps: usize,
    spurious_removals: usize,
    trials_with_spurious: usize,
    worst: Option<(u64, usize, String, String, String)>,
}

/// Run `trials` random edit scripts over documents drawn from a `pool` of
/// distinct paragraph texts. A small pool makes duplicate content common (the
/// regime rule 2 exists for); a large pool makes every paragraph distinct.
fn run_property(pool: usize, trials: u64) -> Stats {
    let bounds = ChunkBounds::default();
    let mut stats = Stats::default();

    for seed in 0..trials {
        let mut rng = StdRng::seed_from_u64(seed);
        let mut next_truth = 1u64;
        let old_blocks = random_doc(&mut rng, pool, &mut next_truth);
        let mut new_blocks = old_blocks.clone();
        let ops = random_ops(&mut rng, pool, old_blocks.len(), &mut next_truth);
        apply(&mut new_blocks, &ops, &mut next_truth);
        if new_blocks.is_empty() {
            continue;
        }
        stats.trials += 1;

        let old_source = render(&old_blocks);
        let new_source = render(&new_blocks);
        let old_recs = body_blocks("docs/random.md", &old_source, &bounds);
        let new_recs = body_blocks("docs/random.md", &new_source, &bounds);
        assert_eq!(
            old_recs.len(),
            old_blocks.len(),
            "seed {seed}: one block per paragraph"
        );
        assert_eq!(
            new_recs.len(),
            new_blocks.len(),
            "seed {seed}: one block per paragraph"
        );

        let matching = match_path(&old_recs, &new_recs, EdgePolicy::Strict);

        // -- INVARIANT: the matching is a partial injection in both directions,
        // so no block ever receives two predecessors or two successors.
        let mut seen_old = HashSet::new();
        let mut seen_new = HashSet::new();
        for pair in &matching.pairs {
            assert!(
                seen_old.insert(pair.old),
                "seed {seed}: old block {} matched twice",
                pair.old
            );
            assert!(
                seen_new.insert(pair.new),
                "seed {seed}: new block {} matched twice",
                pair.new
            );
            assert_eq!(
                old_recs[pair.old].path, new_recs[pair.new].path,
                "seed {seed}: rule 3"
            );
        }
        assert_eq!(
            matching.pairs.len() + matching.removed.len(),
            old_recs.len(),
            "seed {seed}: every old block is paired or removed exactly once"
        );
        assert_eq!(
            matching.pairs.len() + matching.added.len(),
            new_recs.len(),
            "seed {seed}: every new block is paired or added exactly once"
        );

        // -- Ground-truth succession check.
        let mut local_false = 0usize;
        for pair in &matching.pairs {
            stats.pairs += 1;
            let (old_truth, new_truth_id) =
                (old_blocks[pair.old].truth, new_blocks[pair.new].truth);
            if old_truth == new_truth_id {
                continue;
            }
            if old_recs[pair.old].hash == new_recs[pair.new].hash {
                // Byte-identical copies: which copy is "the same one" is not
                // observable and the ledger asserts no body change.
                stats.indistinguishable_swaps += 1;
            } else {
                // A body_changed predecessor edge between two blocks with no
                // lineage relation at all.
                local_false += 1;
                stats.false_successions += 1;
            }
        }
        if local_false > 0 {
            stats.trials_with_false += 1;
            if stats
                .worst
                .as_ref()
                .is_none_or(|(_, count, ..)| local_false > *count)
            {
                stats.worst = Some((
                    seed,
                    local_false,
                    format!("{ops:?}"),
                    old_source.clone(),
                    new_source.clone(),
                ));
            }
        }

        // -- Ledger-level accounting, including rows written for content that
        // survived untouched.
        let mut ledger = Ledger::new(bounds);
        ledger.scan(&[("docs/random.md", &old_source)], &[]);
        let events = ledger.scan(&[("docs/random.md", &new_source)], &[]);
        let mut per_occurrence: HashMap<u64, usize> = HashMap::new();
        for event in &events {
            *per_occurrence.entry(event.occurrence).or_default() += 1;
        }
        assert!(
            per_occurrence.values().all(|n| *n == 1),
            "seed {seed}: an occurrence received two rows in one scan"
        );

        // Count removals of content that did not actually lose a copy: the new
        // document still holds at least as many copies of that exact block as
        // the old one did, so nothing was deleted; the matcher merely could not
        // say which copy was which.
        let mut old_counts: HashMap<&str, usize> = HashMap::new();
        for rec in &old_recs {
            *old_counts.entry(rec.hash.as_str()).or_default() += 1;
        }
        let mut new_counts: HashMap<&str, usize> = HashMap::new();
        for rec in &new_recs {
            *new_counts.entry(rec.hash.as_str()).or_default() += 1;
        }
        let mut local_spurious = 0usize;
        for event in &events {
            if event.lifecycle != Lifecycle::Removed {
                continue;
            }
            let hash = event.hash.as_str();
            if new_counts.get(hash).copied().unwrap_or(0) >= old_counts[hash] {
                local_spurious += 1;
            }
        }
        stats.spurious_removals += local_spurious;
        if local_spurious > 0 {
            stats.trials_with_spurious += 1;
        }
    }

    stats
}

fn report(label: &str, stats: &Stats) {
    println!(
        "H8 [{label}]: {} scripts, {} predecessor edges\n\
         H8 [{label}]:   false successions: {} edges in {} scripts\n\
         H8 [{label}]:   identical-copy relabelings (no lineage claim beyond order): {}\n\
         H8 [{label}]:   `removed` rows where no copy of that content actually left the file: \
         {} rows in {} scripts",
        stats.trials,
        stats.pairs,
        stats.false_successions,
        stats.trials_with_false,
        stats.indistinguishable_swaps,
        stats.spurious_removals,
        stats.trials_with_spurious
    );
    if let Some((seed, count, ops, old_source, new_source)) = &stats.worst {
        println!(
            "H8 [{label}] worst case: seed {seed}, {count} false succession(s), ops {ops}\n\
             --- before ---\n{}\n--- after ---\n{}",
            old_source.trim_end(),
            new_source.trim_end()
        );
    }
}

#[test]
fn h8_property_no_double_edges_and_measured_false_succession() {
    // Duplicate-heavy corpus: eight distinct paragraph texts, so repeated
    // content (rule 2's regime) is the norm.
    let duplicating = run_property(8, 2_000);
    report("duplicate-heavy", &duplicating);
    // Distinct corpus: every paragraph text is unique, so rule 1 does almost
    // all the work and only rule 4 can misfire.
    let distinct = run_property(4_000, 2_000);
    report("all-distinct", &distinct);

    // -- INVARIANT that holds everywhere (asserted per trial inside
    // `run_property`): the matching is a partial injection in both directions,
    // so no block ever receives two predecessors or two successors, and every
    // block is accounted for exactly once.
    assert!(
        duplicating.pairs > 0 && distinct.pairs > 0,
        "the property test must match something"
    );

    // PLAN CLAIM (entry, acceptance): conservative one-to-one matching,
    // "ordinal position alone never establishes continuity", "false succession
    // is worse than missing succession".
    // OBSERVED: rule 4 asserts succession between unrelated blocks whenever one
    // unmatched deletion and one unmatched insertion land in the same gap. It
    // is not an implementation slip: the two snapshots are byte-identical to
    // those a real edit would produce, so no two-snapshot rule can separate
    // them. It happens in BOTH regimes, so uniqueness of content does not
    // protect against it.
    assert!(
        duplicating.false_successions > 0,
        "expected rule 4 to assert a false succession in the duplicate-heavy regime"
    );
    assert!(
        distinct.false_successions > 0,
        "expected rule 4 to assert a false succession even with all-distinct content"
    );

    // OBSERVED: duplicate content is what drives spurious `removed` rows for
    // blocks that never left the document; with all-distinct content those rows
    // are far rarer.
    assert!(duplicating.spurious_removals > 0);
    assert!(
        duplicating.spurious_removals > distinct.spurious_removals,
        "duplicate content should dominate the churn: {} vs {}",
        duplicating.spurious_removals,
        distinct.spurious_removals
    );
}
