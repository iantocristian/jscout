//! F1-F8: does G24's freshness ordering do what the plan says it does?
//!
//! Two models are implemented here and compared head to head:
//!
//! - MODEL A, the REFUTED original proposal: multiply the post-fusion score by
//!   `(1 - max_penalty) + max_penalty * 2^(-age/half_life)` with a 30% cap,
//!   which was claimed to "prevent a recent but weakly relevant passage from
//!   defeating a substantially better older passage".
//! - MODEL B, the REPLACEMENT rule from the revised plan: record `base_rank`,
//!   swap adjacent pairs only when the lower candidate is strictly newer AND
//!   both resulting positions stay within `max_rank_movement` of their own
//!   `base_rank`, repeating top-to-bottom until a full scan makes no swap.
//!
//! Fusion is NOT modelled: [`rrf`] is copied verbatim from the code plane
//! (`src/search.rs:1585`) and always called with the
//! constant the code plane uses (`k = 60.0`, `src/search.rs:2085`), so every
//! measurement here is against jscout's real score distribution.
//!
//! Methodology: where reality contradicts the plan the assertion pins the
//! OBSERVED behavior and the comment names the claim that failed. Nothing is
//! relaxed to make the suite green.
//!
//! # Findings
//!
//! - F1 VIOLATED (the refutation is confirmed, and it is worse than stated):
//!   under k = 60 a maximally fresh candidate at base rank 27 beats a maximally
//!   decayed rank-1 candidate - 26 ranks of movement. The worst displacement
//!   anywhere in the pool is 42 ranks at pool 80 and 168 at pool 500, growing
//!   at ~`max_penalty` ranks per rank of pool depth. A merely 90-day-old rank-1
//!   hit is already beaten by a fresh rank-4 hit.
//! - F2 HOLDS: no candidate ever moved more than `max_rank_movement` from its
//!   base rank across 600 random corpora.
//! - F3 HOLDS: every run settled, and each swap removed exactly one comparable
//!   inversion, as the plan's proof claims. Termination rests on the word
//!   "strictly", not on the movement guard: the non-strict comparator never
//!   settles.
//! - F4 SPLIT: the reordering is deterministic and ignores ids, epochs, and
//!   working-tree secondary metadata. The plan's "reciprocal-rank fusion with
//!   deterministic tie-breaks" is VIOLATED by the code plane's `rrf`, which is
//!   nondeterministic on ties (8 distinct orders from one input).
//! - F5 SPLIT: git/observed never reorder, unknown never moves, working_tree is
//!   newest within git - all HOLD. But "git orders against git by latest author
//!   time" is VIOLATED whenever an incomparable candidate sits between two git
//!   candidates: adjacency alone blocks the exchange.
//! - F6 HOLDS: `limit + max_rank_movement` retention is exactly sufficient and
//!   exactly necessary - with the caveat that a candidate rising alongside
//!   others can be pushed down instead of up.
//! - F7: the bounded rule leaves the freshest hit out of the top-5 in 22.7% of
//!   random corpora, every one of them reachable within the bound; and the
//!   greedy top-down scan can spend an upper candidate's movement budget and
//!   strand the freshest document entirely.
//! - F8: bounded reordering does surface fresher content (75.7% vs 60.9%
//!   baseline) while keeping every guarantee; the decay model reaches 95.6% and
//!   keeps none of them.

use std::collections::{BTreeMap, HashMap, HashSet};

use g24_harness::git;
use g24_harness::md;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

// ---------------------------------------------------------------------------
// The code plane's real reciprocal rank fusion
// ---------------------------------------------------------------------------

/// VERBATIM COPY of `fn rrf` from jscout `src/search.rs:1585`, including the
/// `HashMap` collection and the `total_cmp` sort with no tie-break. Copied so
/// the freshness measurements run against the real score distribution rather
/// than a convenient model of it.
fn rrf(rankings: &[Vec<(i64, f64)>], k: f64) -> Vec<(i64, f64)> {
    let mut scores: HashMap<i64, f64> = HashMap::new();
    for ranking in rankings {
        for (rank, (id, _)) in ranking.iter().enumerate() {
            *scores.entry(*id).or_insert(0.0) += 1.0 / (k + rank as f64 + 1.0);
        }
    }
    let mut out: Vec<(i64, f64)> = scores.into_iter().collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    out
}

/// The k the code plane passes at `src/search.rs:2085`.
const RRF_K: f64 = 60.0;

/// The plan's default `max_rank_movement`.
const MAX_RANK_MOVEMENT: usize = 2;

/// The refuted proposal's cap: "a 30% cap".
const MAX_PENALTY: f64 = 0.30;

/// INVENTED (spec gap): the original proposal named the 30% cap but never named
/// a half-life. 365 days is used throughout, and every headline number is also
/// derived in closed form so the dependence on this choice is explicit.
const HALF_LIFE_DAYS: f64 = 365.0;

const DAY: i64 = 86_400;

/// [`rrf`] plus an id tie-break, used wherever an experiment needs a stable
/// base order. The real `rrf` has NO tie-break; see
/// `f4_the_real_rrf_has_no_deterministic_tie_break` for what that costs.
fn rrf_stable(rankings: &[Vec<(i64, f64)>]) -> Vec<(i64, f64)> {
    let mut fused = rrf(rankings, RRF_K);
    fused.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    fused
}

/// Post-fusion scores for a single ranking of `n` candidates in rank order,
/// i.e. what `--lexical-only` documentation search produces.
fn lexical_fused(n: usize) -> Vec<(i64, f64)> {
    let ranking: Vec<(i64, f64)> = (1..=n as i64).map(|id| (id, 0.0)).collect();
    rrf_stable(&[ranking])
}

/// Post-fusion scores when BM25 and the vector index agree on the order, i.e.
/// the hybrid default.
fn hybrid_agreeing_fused(n: usize) -> Vec<(i64, f64)> {
    let ranking: Vec<(i64, f64)> = (1..=n as i64).map(|id| (id, 0.0)).collect();
    rrf_stable(&[ranking.clone(), ranking])
}

// ---------------------------------------------------------------------------
// MODEL A: the refuted multiplicative decay
// ---------------------------------------------------------------------------

/// `(1 - max_penalty) + max_penalty * 2^(-age/half_life)`.
fn decay_multiplier(age_days: f64, max_penalty: f64, half_life_days: f64) -> f64 {
    (1.0 - max_penalty) + max_penalty * 2f64.powf(-age_days / half_life_days)
}

/// Apply MODEL A to a fused ranking. `ages[i]` is the age of `fused[i]`.
///
/// INVENTED (spec gap): the original proposal never said how to break ties in
/// the decayed score. Ties are broken by base position, which is the most
/// favourable possible reading — it removes the fusion nondeterminism from the
/// measurement instead of adding to it.
fn decay_order(fused: &[(i64, f64)], ages_days: &[f64]) -> Vec<i64> {
    let mut scored: Vec<(i64, f64, usize)> = fused
        .iter()
        .zip(ages_days)
        .enumerate()
        .map(|(position, ((id, score), age))| {
            (
                *id,
                score * decay_multiplier(*age, MAX_PENALTY, HALF_LIFE_DAYS),
                position,
            )
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.2.cmp(&b.2)));
    scored.into_iter().map(|(id, _, _)| id).collect()
}

/// Deepest one-based base rank from which a maximally fresh candidate reaches
/// rank 1 over a field of `stale_age_days`-old candidates, plus the largest
/// rank displacement any candidate experiences.
fn fresh_promotion_probe(fused: &[(i64, f64)], stale_age_days: f64) -> (usize, usize) {
    let mut deepest_to_top = 1usize;
    let mut max_displacement = 0usize;
    for probe in 0..fused.len() {
        let ages: Vec<f64> = (0..fused.len())
            .map(|i| if i == probe { 0.0 } else { stale_age_days })
            .collect();
        let order = decay_order(fused, &ages);
        let final_position = order.iter().position(|id| *id == fused[probe].0).unwrap();
        max_displacement = max_displacement.max(probe.abs_diff(final_position));
        if final_position == 0 {
            deepest_to_top = deepest_to_top.max(probe + 1);
        }
    }
    (deepest_to_top, max_displacement)
}

/// Closed form of the same quantity: a fresh candidate at zero-based rank `r`
/// out-scores a decayed rank-1 candidate exactly while
/// `1/(k+1+r) > multiplier * 1/(k+1)`.
fn predicted_deepest_to_top(stale_age_days: f64) -> usize {
    let multiplier = decay_multiplier(stale_age_days, MAX_PENALTY, HALF_LIFE_DAYS);
    let r = (RRF_K + 1.0) * (1.0 - multiplier) / multiplier;
    // strict inequality: an exact integer solution does not win
    let deepest_r = if (r - r.floor()).abs() < f64::EPSILON {
        r as usize - 1
    } else {
        r.floor() as usize
    };
    deepest_r + 1
}

/// Closed form of the largest displacement in a pool of `pool` candidates: a
/// decayed candidate at zero-based rank `j` still outranks the fresh candidate
/// at zero-based rank `p` exactly while `multiplier/(k+1+j) > 1/(k+1+p)`, so
/// the fresh candidate lands at position `ceil(multiplier*(k+1+p) - (k+1))`.
fn predicted_max_displacement(pool: usize, stale_age_days: f64) -> usize {
    let multiplier = decay_multiplier(stale_age_days, MAX_PENALTY, HALF_LIFE_DAYS);
    let mut worst = 0usize;
    for probe in 0..pool {
        let threshold = multiplier * (RRF_K + 1.0 + probe as f64) - (RRF_K + 1.0);
        let landing = if threshold <= 0.0 {
            0
        } else {
            threshold.ceil() as usize
        };
        worst = worst.max(probe - landing.min(probe));
    }
    worst
}

// ---------------------------------------------------------------------------
// MODEL B: the bounded, order-based replacement rule
// ---------------------------------------------------------------------------

/// A retrieval chunk's single freshness basis, per "A retrieval chunk has one
/// basis, chosen deterministically".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Basis {
    /// Latest usable git author time among contributing body lines.
    Git { author_time: i64 },
    /// Any contributing body line is modified in the worktree.
    WorkingTree { latest_committed: Option<i64> },
    /// Latest freshness-bearing observation, by snapshot sequence.
    Observed { snapshot_seq: u64 },
    /// No usable provenance.
    Unknown,
}

impl Basis {
    fn label(self) -> &'static str {
        match self {
            Basis::Git { .. } => "git",
            Basis::WorkingTree { .. } => "working_tree",
            Basis::Observed { .. } => "observed",
            Basis::Unknown => "unknown",
        }
    }
}

/// "the lower candidate is strictly newer under the partial order":
///
/// - within git provenance: `working_tree` is newest, then latest author time;
/// - within observed provenance: the later snapshot sequence wins;
/// - git-basis and observed-basis candidates never reorder against each other;
/// - unknown participates in no reordering.
fn strictly_newer(lower: &Candidate, upper: &Candidate) -> bool {
    match (lower.basis, upper.basis) {
        (Basis::Git { author_time: low }, Basis::Git { author_time: up }) => low > up,
        (Basis::WorkingTree { .. }, Basis::Git { .. }) => true,
        (Basis::Git { .. }, Basis::WorkingTree { .. }) => false,
        // INVENTED (spec gap): the plan orders `working_tree` above git but
        // never orders two `working_tree` chunks against each other. Treated as
        // tied, so relevance order survives; the plan's own "secondary
        // metadata" committed time is deliberately NOT used as a sort key.
        (Basis::WorkingTree { .. }, Basis::WorkingTree { .. }) => false,
        (Basis::Observed { snapshot_seq: low }, Basis::Observed { snapshot_seq: up }) => low > up,
        _ => false,
    }
}

/// A "newer or equal" comparator: the same rule with the word *strictly*
/// dropped. Used only to show what that word is holding up (F3).
fn newer_or_equal(lower: &Candidate, upper: &Candidate) -> bool {
    strictly_newer(lower, upper) || !strictly_newer(upper, lower)
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Candidate {
    id: i64,
    /// One-based relevance rank recorded before any freshness reordering.
    base_rank: usize,
    basis: Basis,
    /// Age in days for MODEL A only; `None` where no clock exists.
    age_days: Option<f64>,
}

#[derive(Debug, Clone, Default)]
struct ReorderStats {
    scans: usize,
    swaps: usize,
    hit_scan_bound: bool,
    /// Comparable-inversion count after each swap, when tracing is on.
    inversion_trace: Vec<usize>,
}

fn within_bound(new_rank: usize, base_rank: usize, max_rank_movement: usize) -> bool {
    new_rank.abs_diff(base_rank) <= max_rank_movement
}

/// The plan's reordering rule, generic over the "strictly newer" predicate so
/// F3 can feed it a deliberately broken one.
fn reorder_with<F>(
    order: &mut [Candidate],
    max_rank_movement: usize,
    scan_bound: usize,
    newer: &F,
    trace: bool,
) -> ReorderStats
where
    F: Fn(&Candidate, &Candidate) -> bool,
{
    let mut stats = ReorderStats::default();
    loop {
        if stats.scans >= scan_bound {
            stats.hit_scan_bound = true;
            return stats;
        }
        stats.scans += 1;
        let mut swapped = false;
        let mut index = 0usize;
        while index + 1 < order.len() {
            let upper = order[index];
            let lower = order[index + 1];
            // One-based resulting positions after the candidate swap.
            if newer(&lower, &upper)
                && within_bound(index + 2, upper.base_rank, max_rank_movement)
                && within_bound(index + 1, lower.base_rank, max_rank_movement)
            {
                order.swap(index, index + 1);
                swapped = true;
                stats.swaps += 1;
                if trace {
                    stats.inversion_trace.push(count_inversions(order, newer));
                }
            }
            index += 1;
        }
        if !swapped {
            return stats;
        }
    }
}

fn reorder(order: &mut [Candidate], max_rank_movement: usize) -> ReorderStats {
    let bound = order.len() * order.len() + 8;
    reorder_with(order, max_rank_movement, bound, &strictly_newer, false)
}

/// Pairs `(i < j)` where the lower candidate is strictly newer: the potential
/// function the plan's termination argument rests on.
fn count_inversions<F>(order: &[Candidate], newer: &F) -> usize
where
    F: Fn(&Candidate, &Candidate) -> bool,
{
    let mut total = 0;
    for i in 0..order.len() {
        for j in (i + 1)..order.len() {
            if newer(&order[j], &order[i]) {
                total += 1;
            }
        }
    }
    total
}

fn ids(order: &[Candidate]) -> Vec<i64> {
    order.iter().map(|candidate| candidate.id).collect()
}

// ---------------------------------------------------------------------------
// Synthetic corpora
// ---------------------------------------------------------------------------

/// "now" for every synthetic corpus, so ages are reproducible.
const NOW: i64 = 1_760_000_000;

fn candidate(id: i64, base_rank: usize, basis: Basis) -> Candidate {
    let age_days = match basis {
        Basis::Git { author_time } => Some((NOW - author_time) as f64 / DAY as f64),
        Basis::WorkingTree { .. } => Some(0.0),
        // Observed snapshots are a local clock; mapped to an age only so MODEL
        // A has something to consume at all (see F8).
        Basis::Observed { snapshot_seq } => Some((60 - snapshot_seq.min(60)) as f64),
        Basis::Unknown => None,
    };
    Candidate {
        id,
        base_rank,
        basis,
        age_days,
    }
}

fn git_days_ago(days: i64) -> Basis {
    Basis::Git {
        author_time: NOW - days * DAY,
    }
}

fn random_basis(rng: &mut StdRng) -> Basis {
    match rng.random_range(0..100u32) {
        0..=54 => git_days_ago(rng.random_range(0..1800)),
        55..=69 => Basis::WorkingTree {
            latest_committed: Some(NOW - rng.random_range(0..1800) * DAY),
        },
        70..=89 => Basis::Observed {
            snapshot_seq: rng.random_range(1..40),
        },
        _ => Basis::Unknown,
    }
}

/// A random retrieval outcome: two independent rankings fused by the real RRF,
/// then a random provenance mix attached in base-rank order.
fn random_corpus(rng: &mut StdRng, size: usize) -> Vec<Candidate> {
    let mut bm25: Vec<i64> = (1..=size as i64).collect();
    let mut vector: Vec<i64> = (1..=size as i64).collect();
    shuffle(rng, &mut bm25);
    shuffle(rng, &mut vector);
    // The vector ranking usually retrieves a subset, not the whole pool.
    let vector_len = rng.random_range((size / 2).max(1)..=size);
    vector.truncate(vector_len);
    let bm25_ranking: Vec<(i64, f64)> = bm25.into_iter().map(|id| (id, 0.0)).collect();
    let vector_ranking: Vec<(i64, f64)> = vector.into_iter().map(|id| (id, 0.0)).collect();
    let fused = rrf_stable(&[bm25_ranking, vector_ranking]);
    fused
        .iter()
        .enumerate()
        .map(|(position, (id, _))| candidate(*id, position + 1, random_basis(rng)))
        .collect()
}

fn shuffle(rng: &mut StdRng, items: &mut [i64]) {
    for index in (1..items.len()).rev() {
        let other = rng.random_range(0..=index);
        items.swap(index, other);
    }
}

// ===========================================================================
// F1 - the refuted claim: a 30% multiplicative cap does not cap rank movement
// ===========================================================================

#[test]
fn f1_a_thirty_percent_score_cap_permits_a_twenty_six_rank_promotion() {
    let fused = lexical_fused(80);

    // Sanity on the real fusion: the whole top-80 spans a 2:1 score ratio, so a
    // 30% multiplicative haircut is enormous relative to rank differences.
    let top = fused[0].1;
    let last = fused[79].1;
    println!(
        "F1 real RRF (k={RRF_K}): rank1={top:.9} rank80={last:.9} ratio={:.4}",
        last / top
    );
    assert!(
        (top - 1.0 / 61.0).abs() < 1e-12,
        "rank 1 RRF score must be 1/(k+1)"
    );

    // A maximally decayed field (age -> infinity, multiplier exactly 1 - 0.30)
    // against one maximally fresh candidate.
    let (deepest, max_displacement) = fresh_promotion_probe(&fused, f64::INFINITY);
    let predicted = predicted_deepest_to_top(f64::INFINITY);
    println!(
        "F1 maximally fresh vs maximally decayed rank-1: deepest base_rank reaching rank 1 = {deepest} \
         (displacement {} ranks), closed form = {predicted}, max_rank_movement = {MAX_RANK_MOVEMENT}",
        deepest - 1
    );

    // REFUTED: the original proposal claimed the 30% cap "prevents a recent but
    // weakly relevant passage from defeating a substantially better older
    // passage". OBSERVED: under the code plane's own RRF (k=60) a maximally
    // fresh candidate ranked 27th defeats a maximally decayed rank-1 candidate,
    // a promotion of 26 ranks - 13x the replacement rule's whole movement bound.
    assert_eq!(deepest, 27, "measured deepest base rank reaching rank 1");
    assert_eq!(deepest, predicted, "measurement must match the closed form");

    // And 26 is NOT the largest displacement. OBSERVED: the worst displacement
    // in an 80-candidate pool is 42 ranks, by a fresh rank-80 candidate landing
    // at rank 38. The 30% cap bounds the SCORE ratio, and a bounded score ratio
    // maps to an unbounded rank distance because RRF scores are dense.
    assert_eq!(
        max_displacement, 42,
        "measured maximum rank displacement in a pool of 80"
    );
    assert_eq!(
        max_displacement,
        predicted_max_displacement(80, f64::INFINITY)
    );
    assert!(
        max_displacement > 20 * MAX_RANK_MOVEMENT,
        "a 30% score cap is not a rank-movement cap in any useful sense"
    );
}

#[test]
fn f1_the_worst_displacement_grows_without_bound_as_the_candidate_pool_deepens() {
    // The decisive property: there is no pool-independent bound to quote. Each
    // additional candidate in the pool adds about `max_penalty` ranks of
    // possible displacement (0.3 here), forever.
    let mut measured = Vec::new();
    for pool in [30usize, 80, 200, 500] {
        let (_, displacement) = fresh_promotion_probe(&lexical_fused(pool), f64::INFINITY);
        let predicted = predicted_max_displacement(pool, f64::INFINITY);
        // The closed form and the measurement can disagree by exactly one rank
        // when the deepest candidate's decayed score lands on an EXACT tie with
        // the fresh candidate's (pool 200: 0.7/182 == 1/260 in real arithmetic).
        // Which side of that tie f64 rounding falls on is not a plan question.
        assert!(
            displacement.abs_diff(predicted) <= 1,
            "pool {pool}: measured {displacement}, closed form {predicted}"
        );
        measured.push((pool, displacement));
    }
    println!("F1 worst displacement by pool size: {measured:?}");
    assert_eq!(measured, vec![(30, 27), (80, 42), (200, 77), (500, 168)]);
    // Slope between the two deepest points is max_penalty, as the algebra says.
    let slope = (168.0 - 77.0) / (500.0 - 200.0);
    assert!(
        (slope - MAX_PENALTY).abs() < 0.01,
        "displacement grows at ~max_penalty ranks per rank"
    );
}

#[test]
fn f1_the_promotion_bound_is_a_property_of_k_not_of_the_pool_or_the_fusion_arity() {
    // Pool size does not bound it (until the pool itself is smaller than 27).
    for size in [30usize, 80, 200] {
        let (deepest, _) = fresh_promotion_probe(&lexical_fused(size), f64::INFINITY);
        assert_eq!(
            deepest, 27,
            "pool size {size} must not change the promotion depth"
        );
    }
    let (deepest_small, _) = fresh_promotion_probe(&lexical_fused(12), f64::INFINITY);
    assert_eq!(
        deepest_small, 12,
        "a 12-candidate pool can only promote from rank 12"
    );

    // Hybrid fusion where BM25 and the vector index agree doubles every score,
    // so the ratio - and therefore the promotion depth - is unchanged.
    let (deepest_hybrid, _) = fresh_promotion_probe(&hybrid_agreeing_fused(80), f64::INFINITY);
    assert_eq!(
        deepest_hybrid, 27,
        "the multiplicative model is scale-free: fusion arity cancels"
    );

    // Only k moves it. With the code plane's k the cap is 26 ranks of movement.
    let ranking: Vec<(i64, f64)> = (1..=80i64).map(|id| (id, 0.0)).collect();
    let mut small_k = rrf(&[ranking], 5.0);
    small_k.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let (deepest_k5, _) = fresh_promotion_probe(&small_k, f64::INFINITY);
    println!("F1 k=5 deepest base_rank reaching rank 1 = {deepest_k5} (k=60 gives 27)");
    assert_eq!(
        deepest_k5, 3,
        "a smaller k compresses less, so the same 30% cap moves fewer ranks"
    );
}

#[test]
fn f1_a_mild_ninety_day_age_already_exceeds_the_replacement_bound() {
    let fused = lexical_fused(80);
    let (deepest, max_displacement) = fresh_promotion_probe(&fused, 90.0);
    let predicted = predicted_deepest_to_top(90.0);
    let multiplier = decay_multiplier(90.0, MAX_PENALTY, HALF_LIFE_DAYS);
    println!(
        "F1 90-day age at a 365-day half-life: multiplier={multiplier:.6}, deepest base_rank \
         reaching rank 1 = {deepest} (displacement {} ranks), closed form = {predicted}",
        deepest - 1
    );

    // OBSERVED: a mere 90-day-old rank-1 hit (multiplier 0.9529) is beaten by a
    // fresh rank-4 hit. Displacement 3 already exceeds max_rank_movement = 2,
    // so even the *mild* end of the decay curve is unbounded relative to the
    // replacement rule.
    assert_eq!(
        deepest, 4,
        "measured deepest base rank reaching rank 1 at a 90-day age"
    );
    assert_eq!(deepest, predicted, "measurement must match the closed form");
    // The displacement against rank 1 is 3 ranks; the worst displacement
    // anywhere in the same 80-candidate pool is 6 ranks (a fresh rank-80
    // candidate landing at rank 74), still 3x the replacement bound.
    assert_eq!(
        max_displacement, 6,
        "measured maximum rank displacement at a 90-day age"
    );
    assert_eq!(max_displacement, predicted_max_displacement(80, 90.0));
    assert!(
        deepest - 1 > MAX_RANK_MOVEMENT,
        "even a 90-day age beats the replacement bound"
    );
}

#[test]
fn f1_the_multiplicative_model_cannot_express_incomparable_or_unknown_provenance() {
    // A structural refutation, not a numeric one: every candidate must be given
    // SOME multiplier, and any multiplier is a comparison against every other
    // candidate. "no advantage or penalty" is not expressible.
    let fused = lexical_fused(10);
    let unknown_gets_no_penalty: Vec<f64> = (0..10)
        .map(|i| if i == 9 { 0.0 } else { f64::INFINITY })
        .collect();
    let order = decay_order(&fused, &unknown_gets_no_penalty);
    assert_eq!(
        order[0], 10,
        "multiplier 1.0 for unknown promotes it over the whole decayed field"
    );

    let unknown_gets_full_penalty: Vec<f64> = (0..10)
        .map(|i| if i == 0 { f64::INFINITY } else { 0.0 })
        .collect();
    let order = decay_order(&fused, &unknown_gets_full_penalty);
    assert_eq!(
        order[0], 2,
        "multiplier 0.7 for unknown demotes it below the whole fresh field"
    );
    // Either choice is a strong opinion; the model has no neutral element. The
    // replacement rule expresses it directly by refusing the comparison.
}

// ===========================================================================
// F2 - the replacement rule's movement bound
// ===========================================================================

#[test]
fn f2_no_candidate_ever_moves_more_than_max_rank_movement_from_its_base_rank() {
    let mut rng = StdRng::seed_from_u64(0xF2_0000);
    let mut worst_movement = 0usize;
    let mut moved_candidates = 0usize;
    let mut total_candidates = 0usize;
    let mut corpora_reordered = 0usize;
    const CORPORA: usize = 600;

    for _ in 0..CORPORA {
        let size = rng.random_range(2..=24);
        let max_rank_movement = rng.random_range(0..=4);
        let base = random_corpus(&mut rng, size);
        let mut order = base.clone();
        reorder(&mut order, max_rank_movement);

        // Still a permutation of the input: nothing invented, nothing dropped.
        let before: HashSet<i64> = ids(&base).into_iter().collect();
        let after: HashSet<i64> = ids(&order).into_iter().collect();
        assert_eq!(before, after, "reordering must preserve the candidate set");
        assert_eq!(order.len(), base.len());

        if ids(&order) != ids(&base) {
            corpora_reordered += 1;
        }
        for (position, candidate) in order.iter().enumerate() {
            let movement = (position + 1).abs_diff(candidate.base_rank);
            total_candidates += 1;
            if movement > 0 {
                moved_candidates += 1;
            }
            worst_movement = worst_movement.max(movement);
            assert!(
                movement <= max_rank_movement,
                "candidate {} moved {movement} ranks with max_rank_movement={max_rank_movement}: \
                 base={} final={} basis={:?}",
                candidate.id,
                candidate.base_rank,
                position + 1,
                candidate.basis
            );
            // Unknown provenance participates in no reordering at all.
            if candidate.basis == Basis::Unknown {
                assert_eq!(
                    position + 1,
                    candidate.base_rank,
                    "unknown-provenance candidate {} moved",
                    candidate.id
                );
            }
        }
    }
    println!(
        "F2 {CORPORA} random corpora: {corpora_reordered} reordered, {moved_candidates}/{total_candidates} \
         candidates moved, worst observed movement = {worst_movement}"
    );
    // HOLDS. The guard is checked on both resulting positions before every
    // swap, so the bound is an invariant, not a tendency.
    assert!(
        worst_movement <= 4,
        "the largest max_rank_movement drawn was 4"
    );
    assert!(
        corpora_reordered > CORPORA / 4,
        "the experiment must actually exercise reordering"
    );
}

#[test]
fn f2_the_bound_is_enforced_against_base_rank_not_against_the_previous_position() {
    // Six candidates, each strictly newer than the one above it: the maximum
    // pressure a single scan direction can produce.
    let order_in: Vec<Candidate> = (0..6)
        .map(|i| candidate(i as i64 + 1, i + 1, git_days_ago(100 - 10 * i as i64)))
        .collect();
    let mut order = order_in.clone();
    reorder(&mut order, MAX_RANK_MOVEMENT);
    println!(
        "F2 fully inverted field, m=2: {:?} -> {:?}",
        ids(&order_in),
        ids(&order)
    );

    // The newest candidate (id 6) can only reach rank 4, never rank 1: the
    // guard is relative to ITS OWN base rank, so repeated scans cannot walk a
    // candidate further up one step at a time. HOLDS.
    let position_of_6 = order.iter().position(|c| c.id == 6).unwrap() + 1;
    assert_eq!(
        position_of_6, 4,
        "id 6 (base 6, newest) may rise exactly max_rank_movement"
    );

    // OBSERVED, and worth stating plainly: the result is NOT "the freshness
    // order, bounded". It is a sawtooth of independent local rotations -
    // [3,2,1] then [6,5,4] - in which the oldest candidate of each window sits
    // ABOVE the newest candidate of the window below it. The plan's summary
    // "git orders against git by latest author time" does not survive contact
    // with its own bounded adjacent-swap procedure even when every candidate
    // shares one basis and nothing is incomparable.
    assert_eq!(
        ids(&order),
        vec![3, 2, 1, 6, 5, 4],
        "bounded reordering yields windowed rotations"
    );
    let author_times: Vec<i64> = order
        .iter()
        .map(|c| match c.basis {
            Basis::Git { author_time } => author_time,
            _ => unreachable!(),
        })
        .collect();
    let descending = author_times.windows(2).all(|pair| pair[0] >= pair[1]);
    assert!(
        !descending,
        "the output is not sorted by freshness, not even locally monotone"
    );
    for (position, candidate) in order.iter().enumerate() {
        assert!((position + 1).abs_diff(candidate.base_rank) <= MAX_RANK_MOVEMENT);
    }
}

// ===========================================================================
// F3 - termination
// ===========================================================================

#[test]
fn f3_the_loop_always_terminates_and_every_swap_removes_exactly_one_inversion() {
    let mut rng = StdRng::seed_from_u64(0xF3_0000);
    let mut worst_scans = 0usize;
    let mut worst_size = 0usize;
    const CORPORA: usize = 400;

    for _ in 0..CORPORA {
        let size = rng.random_range(2..=20);
        let max_rank_movement = rng.random_range(0..=5);
        let mut order = random_corpus(&mut rng, size);
        let before = count_inversions(&order, &strictly_newer);
        // A bound far above any plausible need; hitting it is the failure.
        let scan_bound = size * size + 8;
        let stats = reorder_with(
            &mut order,
            max_rank_movement,
            scan_bound,
            &strictly_newer,
            true,
        );
        assert!(
            !stats.hit_scan_bound,
            "loop did not settle within {scan_bound} scans"
        );

        // The plan's proof: "Every swap removes one comparable freshness
        // inversion, so the procedure terminates." An adjacent transposition
        // changes exactly one pair's relative order, so the count must fall by
        // exactly one per swap. Verified swap by swap.
        let mut expected = before;
        for observed in &stats.inversion_trace {
            expected -= 1;
            assert_eq!(
                *observed, expected,
                "a swap must remove exactly one comparable inversion"
            );
        }
        assert_eq!(stats.swaps, stats.inversion_trace.len());
        assert!(
            stats.swaps <= before,
            "more swaps than inversions to remove"
        );

        // A settled scan needs at most one pass more than the number of swaps.
        assert!(
            stats.scans <= stats.swaps + 1,
            "scans={} swaps={}",
            stats.scans,
            stats.swaps
        );
        if stats.scans > worst_scans {
            worst_scans = stats.scans;
            worst_size = size;
        }

        // The proof needs the relation to be a strict partial order. Checked on
        // the same data: never newer in both directions, never newer than
        // itself, and transitive within a basis.
        for left in &order {
            assert!(!strictly_newer(left, left), "irreflexive");
            for right in &order {
                assert!(
                    !(strictly_newer(left, right) && strictly_newer(right, left)),
                    "antisymmetric: {:?} vs {:?}",
                    left.basis,
                    right.basis
                );
                for third in &order {
                    if strictly_newer(left, right) && strictly_newer(right, third) {
                        assert!(strictly_newer(left, third), "transitive");
                    }
                }
            }
        }
    }
    println!("F3 {CORPORA} corpora: worst scan count = {worst_scans} (pool size {worst_size})");
}

#[test]
fn f3_adversarial_orderings_designed_to_cycle_still_terminate() {
    // Deliberately hostile shapes: maximum inversion pressure, alternating
    // provenance so swaps are repeatedly re-enabled, and duplicate timestamps.
    let shapes: Vec<Vec<Basis>> = vec![
        // strictly increasing freshness downward: every adjacent pair inverted
        (0..12).map(|i| git_days_ago(1200 - 100 * i)).collect(),
        // alternating git/observed: comparable pairs separated by barriers
        (0..12)
            .map(|i| {
                if i % 2 == 0 {
                    git_days_ago(1200 - 100 * i)
                } else {
                    Basis::Observed {
                        snapshot_seq: (i as u64) + 1,
                    }
                }
            })
            .collect(),
        // identical timestamps everywhere: every comparison is a tie
        (0..12).map(|_| git_days_ago(500)).collect(),
        // working_tree scattered through a decreasing git field
        (0..12)
            .map(|i| {
                if i % 3 == 2 {
                    Basis::WorkingTree {
                        latest_committed: None,
                    }
                } else {
                    git_days_ago(100 + 10 * i)
                }
            })
            .collect(),
        // newest last, oldest first, with unknowns interleaved
        (0..12)
            .map(|i| {
                if i % 4 == 1 {
                    Basis::Unknown
                } else {
                    git_days_ago(1200 - 100 * i)
                }
            })
            .collect(),
    ];

    for (shape_index, shape) in shapes.iter().enumerate() {
        for max_rank_movement in 0..=6 {
            let mut order: Vec<Candidate> = shape
                .iter()
                .enumerate()
                .map(|(i, basis)| candidate(i as i64 + 1, i + 1, *basis))
                .collect();
            let stats = reorder_with(&mut order, max_rank_movement, 200, &strictly_newer, false);
            assert!(
                !stats.hit_scan_bound,
                "shape {shape_index} with m={max_rank_movement} did not settle in 200 scans"
            );
            assert!(
                stats.scans <= order.len() + 1,
                "shape {shape_index}: {} scans",
                stats.scans
            );
        }
    }
    println!("F3 5 adversarial shapes x 7 movement bounds: every run settled, max scans <= n+1");
}

#[test]
fn f3_termination_depends_on_the_word_strictly_not_on_the_movement_guard() {
    // The plan says "strictly newer". Dropping that word - the obvious
    // implementation slip of writing >= instead of > - makes tied candidates
    // swap forever, and the movement guard does NOT save it: both candidates
    // stay one rank from their base ranks while oscillating.
    let mut order: Vec<Candidate> = (0..4)
        .map(|i| candidate(i as i64 + 1, i + 1, git_days_ago(500)))
        .collect();
    let stats = reorder_with(&mut order, MAX_RANK_MOVEMENT, 64, &newer_or_equal, false);
    println!(
        "F3 non-strict comparator: hit_scan_bound={} after {} scans / {} swaps",
        stats.hit_scan_bound, stats.scans, stats.swaps
    );
    assert!(
        stats.hit_scan_bound,
        "a non-strict comparator must be observed NOT to terminate"
    );

    // And the strict comparator on the same input settles immediately.
    let mut order: Vec<Candidate> = (0..4)
        .map(|i| candidate(i as i64 + 1, i + 1, git_days_ago(500)))
        .collect();
    let stats = reorder_with(&mut order, MAX_RANK_MOVEMENT, 64, &strictly_newer, false);
    assert!(!stats.hit_scan_bound);
    assert_eq!(stats.swaps, 0);
    assert_eq!(
        stats.scans, 1,
        "one clean scan is enough when nothing is inverted"
    );
}

// ===========================================================================
// F4 - determinism
// ===========================================================================

#[test]
fn f4_identical_input_yields_identical_output() {
    let mut rng = StdRng::seed_from_u64(0xF4_0000);
    for _ in 0..200 {
        let size = rng.random_range(2..=20);
        let max_rank_movement = rng.random_range(0..=4);
        let base = random_corpus(&mut rng, size);
        let mut first = base.clone();
        let mut second = base.clone();
        reorder(&mut first, max_rank_movement);
        reorder(&mut second, max_rank_movement);
        assert_eq!(
            ids(&first),
            ids(&second),
            "the reordering itself is deterministic"
        );
    }
}

#[test]
fn f4_the_reordering_ignores_irrelevant_input_detail() {
    let mut rng = StdRng::seed_from_u64(0xF4_1111);
    for _ in 0..200 {
        let size = rng.random_range(2..=18);
        let max_rank_movement = rng.random_range(0..=3);
        let base = random_corpus(&mut rng, size);

        let mut reference = base.clone();
        reorder(&mut reference, max_rank_movement);
        let reference_positions: Vec<usize> = reference
            .iter()
            .map(|candidate| candidate.base_rank)
            .collect();

        // 1. Chunk ids are labels: relabelling must not change the outcome.
        let relabelled: Vec<Candidate> = base
            .iter()
            .map(|candidate| Candidate {
                id: candidate.id * 7 + 1_000,
                ..*candidate
            })
            .collect();
        let mut relabelled_order = relabelled.clone();
        reorder(&mut relabelled_order, max_rank_movement);
        let relabelled_positions: Vec<usize> = relabelled_order
            .iter()
            .map(|candidate| candidate.base_rank)
            .collect();
        assert_eq!(
            reference_positions, relabelled_positions,
            "ids must not affect ordering"
        );

        // 2. Only the ORDER of timestamps matters, never the epoch: shifting
        //    every git author time by a constant must change nothing.
        let shifted: Vec<Candidate> = base
            .iter()
            .map(|candidate| match candidate.basis {
                Basis::Git { author_time } => Candidate {
                    basis: Basis::Git {
                        author_time: author_time - 5_000 * DAY,
                    },
                    ..*candidate
                },
                _ => *candidate,
            })
            .collect();
        let mut shifted_order = shifted.clone();
        reorder(&mut shifted_order, max_rank_movement);
        let shifted_positions: Vec<usize> = shifted_order
            .iter()
            .map(|candidate| candidate.base_rank)
            .collect();
        assert_eq!(
            reference_positions, shifted_positions,
            "an epoch shift must not affect ordering"
        );

        // 3. `working_tree`'s "secondary metadata" committed time is metadata:
        //    changing it must not change the ordering.
        let retagged: Vec<Candidate> = base
            .iter()
            .map(|candidate| match candidate.basis {
                Basis::WorkingTree { .. } => Candidate {
                    basis: Basis::WorkingTree {
                        latest_committed: Some(NOW),
                    },
                    ..*candidate
                },
                _ => *candidate,
            })
            .collect();
        let mut retagged_order = retagged.clone();
        reorder(&mut retagged_order, max_rank_movement);
        let retagged_positions: Vec<usize> = retagged_order
            .iter()
            .map(|candidate| candidate.base_rank)
            .collect();
        assert_eq!(
            reference_positions, retagged_positions,
            "working_tree secondary metadata must not be a sort key"
        );
    }
    println!("F4 200 corpora x 3 irrelevant perturbations: output ordering identical every time");
}

#[test]
fn f4_the_real_rrf_has_no_deterministic_tie_break() {
    // The plan's pipeline says "reciprocal-rank fusion with deterministic
    // tie-breaks", and its validation list says "hybrid RRF is deterministic
    // across insertion order". The code plane's `rrf` collects into a HashMap
    // and finishes with a STABLE sort on score alone, so tied candidates come
    // out in HashMap iteration order - which std reseeds per map instance.
    let forward: Vec<(i64, f64)> = (1..=6i64).map(|id| (id, 0.0)).collect();
    let backward: Vec<(i64, f64)> = (1..=6i64).rev().map(|id| (id, 0.0)).collect();

    let mut orders: BTreeMap<Vec<i64>, usize> = BTreeMap::new();
    const TRIALS: usize = 2_000;
    for _ in 0..TRIALS {
        let fused = rrf(&[forward.clone(), backward.clone()], RRF_K);
        *orders
            .entry(fused.iter().map(|(id, _)| *id).collect())
            .or_insert(0) += 1;
    }
    println!(
        "F4 real rrf over {TRIALS} identical calls: {} distinct orders",
        orders.len()
    );
    for (order, count) in &orders {
        println!("   {order:?} x{count}");
    }

    // The tie is exact, not a floating-point near-miss: RRF is symmetric here.
    let fused = rrf(&[forward.clone(), backward.clone()], RRF_K);
    let by_id: HashMap<i64, f64> = fused.iter().copied().collect();
    assert_eq!(
        by_id[&1].to_bits(),
        by_id[&6].to_bits(),
        "ids 1 and 6 tie bit-for-bit"
    );

    // VIOLATED: three exactly tied pairs produce 2^3 = 8 orders, all observed.
    // Base ranks - and therefore every freshness movement measured against
    // them - inherit that nondeterminism.
    assert!(
        orders.len() > 1,
        "OBSERVED: jscout's rrf is nondeterministic on ties; it produced only {} order(s) here",
        orders.len()
    );
    assert_eq!(
        orders.len(),
        8,
        "one order per subset of the three tied pairs"
    );

    // The fix the docs plane needs, for contrast: any total tie-break restores
    // determinism. `rrf_stable` breaks ties by id.
    let mut stable_orders: HashSet<Vec<i64>> = HashSet::new();
    for _ in 0..TRIALS {
        stable_orders.insert(
            rrf_stable(&[forward.clone(), backward.clone()])
                .iter()
                .map(|(id, _)| *id)
                .collect(),
        );
    }
    assert_eq!(
        stable_orders.len(),
        1,
        "an id tie-break makes fusion deterministic"
    );
}

// ===========================================================================
// F5 - comparable provenance only
// ===========================================================================

#[test]
fn f5_git_and_observed_never_reorder_against_each_other() {
    // A brand-new git chunk under a very old observed chunk: no swap.
    let mut order = vec![
        candidate(1, 1, Basis::Observed { snapshot_seq: 1 }),
        candidate(2, 2, git_days_ago(0)),
    ];
    reorder(&mut order, MAX_RANK_MOVEMENT);
    assert_eq!(ids(&order), vec![1, 2], "git must not overtake observed");

    // And the mirror image.
    let mut order = vec![
        candidate(1, 1, git_days_ago(1800)),
        candidate(2, 2, Basis::Observed { snapshot_seq: 999 }),
    ];
    reorder(&mut order, MAX_RANK_MOVEMENT);
    assert_eq!(ids(&order), vec![1, 2], "observed must not overtake git");

    // working_tree is a git basis, so it must not overtake observed either.
    let mut order = vec![
        candidate(1, 1, Basis::Observed { snapshot_seq: 1 }),
        candidate(
            2,
            2,
            Basis::WorkingTree {
                latest_committed: None,
            },
        ),
    ];
    reorder(&mut order, MAX_RANK_MOVEMENT);
    assert_eq!(
        ids(&order),
        vec![1, 2],
        "working_tree must not overtake observed"
    );
}

#[test]
fn f5_working_tree_sorts_newest_within_git_and_unknown_never_moves() {
    // working_tree beats even a git author time of "now".
    let mut order = vec![
        candidate(1, 1, git_days_ago(0)),
        candidate(
            2,
            2,
            Basis::WorkingTree {
                latest_committed: Some(NOW - 900 * DAY),
            },
        ),
    ];
    reorder(&mut order, MAX_RANK_MOVEMENT);
    assert_eq!(
        ids(&order),
        vec![2, 1],
        "working_tree is newest within git provenance"
    );

    // Two working_tree chunks are tied: relevance order survives. (SPEC GAP -
    // the plan never orders working_tree against working_tree.)
    let mut order = vec![
        candidate(
            1,
            1,
            Basis::WorkingTree {
                latest_committed: Some(NOW - 900 * DAY),
            },
        ),
        candidate(
            2,
            2,
            Basis::WorkingTree {
                latest_committed: Some(NOW),
            },
        ),
    ];
    reorder(&mut order, MAX_RANK_MOVEMENT);
    assert_eq!(ids(&order), vec![1, 2]);

    // Unknown neither rises nor falls, in either direction, against anything.
    for other in [
        git_days_ago(0),
        git_days_ago(1800),
        Basis::WorkingTree {
            latest_committed: None,
        },
        Basis::Observed { snapshot_seq: 7 },
        Basis::Unknown,
    ] {
        let mut order = vec![candidate(1, 1, Basis::Unknown), candidate(2, 2, other)];
        reorder(&mut order, MAX_RANK_MOVEMENT);
        assert_eq!(
            ids(&order),
            vec![1, 2],
            "unknown must not be overtaken by {}",
            other.label()
        );

        let mut order = vec![candidate(1, 1, other), candidate(2, 2, Basis::Unknown)];
        reorder(&mut order, MAX_RANK_MOVEMENT);
        assert_eq!(
            ids(&order),
            vec![1, 2],
            "unknown must not overtake {}",
            other.label()
        );
    }
}

#[test]
fn f5_an_incomparable_candidate_is_a_barrier_that_blocks_git_from_ordering_against_git() {
    // The plan's summary: "git orders against git by latest author time".
    // The plan's procedure: swap ADJACENT pairs only. Those two disagree the
    // moment an incomparable candidate sits between two git candidates.
    for barrier in [Basis::Unknown, Basis::Observed { snapshot_seq: 5 }] {
        let mut order = vec![
            candidate(1, 1, git_days_ago(1800)), // oldest, stays at rank 1
            candidate(2, 2, barrier),
            candidate(3, 3, git_days_ago(0)), // newest, movement to rank 1 is within m=2
        ];
        let stats = reorder(&mut order, MAX_RANK_MOVEMENT);

        // VIOLATED: the newest git candidate could legally sit at rank 1 (base
        // 3, movement 2) and the oldest could legally sit at rank 3, yet no
        // swap fires, because every adjacent pair is incomparable.
        assert_eq!(
            ids(&order),
            vec![1, 2, 3],
            "barrier {} blocks the git/git swap",
            barrier.label()
        );
        assert_eq!(stats.swaps, 0);
    }

    // Without the barrier the same two candidates do swap, which isolates the
    // barrier as the cause rather than the movement bound.
    let mut order = vec![
        candidate(1, 1, git_days_ago(1800)),
        candidate(3, 2, git_days_ago(0)),
    ];
    reorder(&mut order, MAX_RANK_MOVEMENT);
    assert_eq!(ids(&order), vec![3, 1]);

    // Frequency in random corpora: how often does a comparable inversion
    // survive because of a barrier rather than because of the movement bound?
    let mut rng = StdRng::seed_from_u64(0xF5_0000);
    let mut with_barrier_witness = 0usize;
    let mut barrier_witnesses = 0usize;
    let mut bound_blocked = 0usize;
    const CORPORA: usize = 500;
    for _ in 0..CORPORA {
        let size = rng.random_range(4..=16);
        let mut order = random_corpus(&mut rng, size);
        reorder(&mut order, MAX_RANK_MOVEMENT);
        let mut adjacent_left = 0usize;
        for index in 0..order.len().saturating_sub(1) {
            if strictly_newer(&order[index + 1], &order[index]) {
                adjacent_left += 1;
            }
        }
        // A precise barrier witness: an older candidate, an incomparable
        // candidate, and a strictly newer candidate in consecutive final
        // positions, where exchanging the outer two would satisfy BOTH movement
        // guards. Only adjacency keeps them in the wrong order.
        let witnesses = (0..order.len().saturating_sub(2))
            .filter(|index| {
                let (old, middle, new) = (order[*index], order[index + 1], order[index + 2]);
                let incomparable =
                    |a: &Candidate, b: &Candidate| !strictly_newer(a, b) && !strictly_newer(b, a);
                strictly_newer(&new, &old)
                    && incomparable(&middle, &old)
                    && incomparable(&middle, &new)
                    && within_bound(index + 1, new.base_rank, MAX_RANK_MOVEMENT)
                    && within_bound(index + 3, old.base_rank, MAX_RANK_MOVEMENT)
            })
            .count();
        barrier_witnesses += witnesses;
        if witnesses > 0 {
            with_barrier_witness += 1;
        }
        if adjacent_left > 0 {
            bound_blocked += 1;
        }
    }
    println!(
        "F5 {CORPORA} corpora at m={MAX_RANK_MOVEMENT}: {with_barrier_witness} contain an \
         incomparable candidate physically blocking a git/git (or observed/observed) exchange that \
         both movement bounds would have allowed ({barrier_witnesses} such triples in total); \
         {bound_blocked} left an ADJACENT comparable inversion unresolved by the movement bound"
    );
    assert!(
        with_barrier_witness > 0,
        "the barrier effect must be observable in random corpora"
    );
}

#[test]
fn f5_git_provenance_from_a_real_repository_orders_by_author_time() -> anyhow::Result<()> {
    let lab = git::GitLab::init()?;
    // A root commit that is NOT part of the measured document: `boundary` is
    // set on root-commit lines even in a complete repository (see the final
    // assertion), so the fixture keeps the root out of the way.
    lab.write("README.md", "root\n")?;
    let root = lab.commit_at("root", NOW - 900 * DAY, NOW - 900 * DAY)?;

    let alpha_only = "# Guide\n\n## Alpha\n\nAlpha paragraph one.\n";
    lab.write("docs/guide.md", alpha_only)?;
    // Alpha was authored long ago but INTEGRATED recently (rebase/cherry-pick).
    lab.commit_at("alpha", NOW - 400 * DAY, NOW - DAY)?;

    let both = "# Guide\n\n## Alpha\n\nAlpha paragraph one.\n\n## Beta\n\nBeta paragraph two.\n";
    lab.write("docs/guide.md", both)?;
    // Beta was authored recently but integrated long ago.
    lab.commit_at("beta", NOW - 10 * DAY, NOW - 800 * DAY)?;

    let document = md::index_document("docs/guide.md", both, &md::ChunkBounds::default());
    let body: Vec<&md::Chunk> = document
        .chunks
        .iter()
        .filter(|chunk| !chunk.is_stub)
        .collect();
    assert_eq!(
        body.len(),
        2,
        "fixture must produce one chunk per section: {body:?}"
    );

    let blame = git::blame_porcelain(
        lab.path(),
        "docs/guide.md",
        &["--no-replace-objects", "-c", "blame.ignoreRevsFile="],
    )?;
    let shallow = git::shallow_boundary_fingerprint(lab.path())?;
    assert!(shallow.is_none(), "the lab repository is not shallow");

    let alpha = chunk_basis(body[0], &blame, shallow.is_some());
    let beta = chunk_basis(body[1], &blame, shallow.is_some());
    let alpha_committer = chunk_committer_time(body[0], &blame);
    let beta_committer = chunk_committer_time(body[1], &blame);
    println!("F5 real blame: alpha={alpha:?} beta={beta:?} (committer times {alpha_committer:?} / {beta_committer:?})");

    assert_eq!(
        alpha,
        Basis::Git {
            author_time: NOW - 400 * DAY
        }
    );
    assert_eq!(
        beta,
        Basis::Git {
            author_time: NOW - 10 * DAY
        }
    );
    // Author time and committer time disagree about which section is newer, so
    // this fixture actually discriminates between the two rules.
    assert!(
        alpha_committer > beta_committer,
        "committer time ranks alpha newest"
    );

    // Beta is more relevant-last but author-newest: it rises.
    let mut order = vec![candidate(1, 1, alpha), candidate(2, 2, beta)];
    reorder(&mut order, MAX_RANK_MOVEMENT);
    assert_eq!(
        ids(&order),
        vec![2, 1],
        "the author-newer section is promoted"
    );

    // A committer-time implementation would have promoted nothing.
    let mut committer_order = vec![
        candidate(
            1,
            1,
            Basis::Git {
                author_time: alpha_committer.unwrap(),
            },
        ),
        candidate(
            2,
            2,
            Basis::Git {
                author_time: beta_committer.unwrap(),
            },
        ),
    ];
    reorder(&mut committer_order, MAX_RANK_MOVEMENT);
    assert_eq!(
        ids(&committer_order),
        vec![1, 2],
        "committer time gives the opposite answer"
    );

    // A worktree edit inside Alpha makes Alpha newest within git provenance.
    let edited = both.replace("Alpha paragraph one.", "Alpha paragraph one, revised.");
    lab.write("docs/guide.md", &edited)?;
    let edited_document = md::index_document("docs/guide.md", &edited, &md::ChunkBounds::default());
    let edited_body: Vec<&md::Chunk> = edited_document
        .chunks
        .iter()
        .filter(|chunk| !chunk.is_stub)
        .collect();
    let dirty_blame = git::blame_porcelain(lab.path(), "docs/guide.md", &["--no-replace-objects"])?;
    let dirty_alpha = chunk_basis(edited_body[0], &dirty_blame, false);
    assert!(
        matches!(dirty_alpha, Basis::WorkingTree { .. }),
        "an uncommitted edit must label the chunk working_tree, got {dirty_alpha:?}"
    );
    let mut order = vec![candidate(1, 1, beta), candidate(2, 2, dirty_alpha)];
    reorder(&mut order, MAX_RANK_MOVEMENT);
    assert_eq!(
        ids(&order),
        vec![2, 1],
        "working_tree outranks every committed author time"
    );

    // OBSERVED TRAP for "shallow-clone boundary commits contribute no
    // timestamp": git sets `boundary` on ROOT-commit lines in a complete
    // repository too. Gating on the flag alone would silently make the oldest
    // content in every non-shallow repository `unknown`.
    let root_blame = git::blame_porcelain(lab.path(), "README.md", &[])?;
    assert!(
        root_blame.iter().all(|line| line.boundary),
        "root-commit lines carry boundary"
    );
    assert!(root_blame.iter().all(|line| line.sha == root));
    assert!(
        git::shallow_boundary_fingerprint(lab.path())?.is_none(),
        "yet the repository is not shallow"
    );
    Ok(())
}

/// Aggregate blame lines onto one chunk exactly as the plan specifies:
/// `working_tree` when any contributing line is uncommitted, else the latest
/// usable author time, else unknown. Boundary commits contribute no timestamp
/// ONLY when the repository is actually shallow.
fn chunk_basis(chunk: &md::Chunk, blame: &[git::BlameLine], repo_is_shallow: bool) -> Basis {
    let mut newest: Option<i64> = None;
    let mut working_tree = false;
    for line in blame
        .iter()
        .filter(|line| line.final_line >= chunk.line_start && line.final_line <= chunk.line_end)
    {
        if line.not_committed_yet {
            working_tree = true;
            continue;
        }
        if repo_is_shallow && line.boundary {
            continue;
        }
        newest = Some(newest.map_or(line.author_time, |best: i64| best.max(line.author_time)));
    }
    if working_tree {
        return Basis::WorkingTree {
            latest_committed: newest,
        };
    }
    match newest {
        Some(author_time) => Basis::Git { author_time },
        None => Basis::Unknown,
    }
}

fn chunk_committer_time(chunk: &md::Chunk, blame: &[git::BlameLine]) -> Option<i64> {
    blame
        .iter()
        .filter(|line| line.final_line >= chunk.line_start && line.final_line <= chunk.line_end)
        .filter(|line| !line.not_committed_yet)
        .map(|line| line.committer_time)
        .max()
}

// ===========================================================================
// F6 - the retention arithmetic
// ===========================================================================

/// Retain `retained` candidates, reorder, then truncate to `limit`.
fn pipeline(
    base: &[Candidate],
    retained: usize,
    limit: usize,
    max_rank_movement: usize,
) -> Vec<i64> {
    let mut order: Vec<Candidate> = base.iter().take(retained).copied().collect();
    reorder(&mut order, max_rank_movement);
    ids(&order).into_iter().take(limit).collect()
}

#[test]
fn f6_limit_plus_max_rank_movement_is_exactly_enough_and_not_one_more() {
    const LIMIT: usize = 10;
    const M: usize = MAX_RANK_MOVEMENT;

    // One newest candidate at `fresh_rank` in an otherwise equally stale field,
    // which isolates the retention arithmetic from any interference between
    // simultaneously rising candidates.
    let field = |size: usize, fresh_rank: usize| -> Vec<Candidate> {
        (0..size)
            .map(|i| {
                let basis = if i + 1 == fresh_rank {
                    git_days_ago(1)
                } else {
                    git_days_ago(1000)
                };
                candidate(i as i64 + 1, i + 1, basis)
            })
            .collect()
    };

    // 1. A candidate at base_rank limit + max_rank_movement DOES enter the
    //    result, landing exactly at rank `limit`.
    let base = field(LIMIT + M, LIMIT + M);
    let mut order = base.clone();
    reorder(&mut order, M);
    let entrant = order.iter().position(|c| c.base_rank == LIMIT + M).unwrap() + 1;
    println!(
        "F6 base_rank {} lands at rank {entrant} (limit {LIMIT})",
        LIMIT + M
    );
    assert_eq!(
        entrant, LIMIT,
        "base_rank limit+m must reach exactly rank limit"
    );
    assert!(pipeline(&base, LIMIT + M, LIMIT, M).contains(&((LIMIT + M) as i64)));

    // 2. One deeper is impossible: base_rank limit + m + 1 can reach at best
    //    rank limit + 1, which is outside the result.
    let base = field(LIMIT + M + 4, LIMIT + M + 1);
    let mut order = base.clone();
    reorder(&mut order, M);
    let just_outside = order
        .iter()
        .position(|c| c.base_rank == LIMIT + M + 1)
        .unwrap()
        + 1;
    assert_eq!(
        just_outside,
        LIMIT + 1,
        "base_rank limit+m+1 stops one short, by arithmetic"
    );
    assert!(!pipeline(&base, LIMIT + M + 4, LIMIT, M).contains(&((LIMIT + M + 1) as i64)));

    // 3. Retaining one FEWER than limit + m loses a result that belonged in it.
    let base = field(LIMIT + M, LIMIT + M);
    let with_full_retention = pipeline(&base, LIMIT + M, LIMIT, M);
    let with_one_less = pipeline(&base, LIMIT + M - 1, LIMIT, M);
    assert_ne!(
        with_full_retention, with_one_less,
        "limit+m-1 is not enough retention"
    );
    assert!(!with_one_less.contains(&((LIMIT + M) as i64)));

    // 4. Retaining MORE than limit + m never changes the result: over-retention
    //    is waste, not safety.
    for extra in 1..=6 {
        let base = field(LIMIT + M + extra, LIMIT + M);
        assert_eq!(
            pipeline(&base, LIMIT + M, LIMIT, M),
            pipeline(&base, LIMIT + M + extra, LIMIT, M),
            "retaining {extra} extra candidates changed the top-{LIMIT}"
        );
    }

    // 5. OBSERVED CAVEAT: the arithmetic is exact only for a lone riser. When
    //    several candidates rise at once the deepest one can be pushed DOWN
    //    instead - it is not the beneficiary of its own bound. In a field whose
    //    freshness increases monotonically downward, base_rank limit+m+1 ends
    //    at rank limit+5, four ranks worse than the arithmetic suggests, having
    //    been overtaken by candidates below it.
    let monotone: Vec<Candidate> = (0..(LIMIT + M + 4))
        .map(|i| candidate(i as i64 + 1, i + 1, git_days_ago(1000 - 10 * i as i64)))
        .collect();
    let mut order = monotone.clone();
    reorder(&mut order, M);
    let deep = order
        .iter()
        .position(|c| c.base_rank == LIMIT + M + 1)
        .unwrap()
        + 1;
    println!(
        "F6 monotone field: base_rank {} lands at rank {deep}",
        LIMIT + M + 1
    );
    assert_eq!(
        deep,
        LIMIT + 5,
        "a simultaneous riser is displaced downward, within its bound"
    );
    assert!(
        (LIMIT + M + 1).abs_diff(deep) <= M,
        "still inside its own movement bound"
    );
}

#[test]
fn f6_the_retention_identity_holds_over_random_corpora_in_both_directions() {
    let mut rng = StdRng::seed_from_u64(0xF6_0000);
    let mut short_retention_differs = 0usize;
    let mut long_retention_differs = 0usize;
    const CORPORA: usize = 500;
    const LIMIT: usize = 5;

    for _ in 0..CORPORA {
        let max_rank_movement = rng.random_range(1..=3);
        let size = LIMIT + max_rank_movement + rng.random_range(1..=8);
        let base = random_corpus(&mut rng, size);
        let exact = pipeline(&base, LIMIT + max_rank_movement, LIMIT, max_rank_movement);
        let generous = pipeline(&base, size, LIMIT, max_rank_movement);
        // Sufficiency: retaining exactly limit + m always reproduces the result
        // computed from the whole pool.
        assert_eq!(exact, generous, "limit+m retention must be sufficient");
        if exact
            != pipeline(
                &base,
                LIMIT + max_rank_movement - 1,
                LIMIT,
                max_rank_movement,
            )
        {
            short_retention_differs += 1;
        }
        if generous != pipeline(&base, LIMIT, LIMIT, max_rank_movement) {
            long_retention_differs += 1;
        }
    }
    println!(
        "F6 {CORPORA} corpora, limit={LIMIT}: retaining limit+m always matched the full pool; \
         retaining limit+m-1 changed the result in {short_retention_differs} corpora; retaining \
         only `limit` changed it in {long_retention_differs}"
    );
    // Necessity: the bound is tight, so under-retention must be observable.
    assert!(
        short_retention_differs > 0,
        "limit+m-1 must sometimes lose a legitimate entrant"
    );
}

// ===========================================================================
// F7 - adversarial: orderings a user would call wrong
// ===========================================================================

#[test]
fn f7_a_much_newer_document_stays_buried_behind_a_marginally_more_relevant_stale_one() {
    // Twelve candidates. The top eleven are a five-year-old deprecated guide;
    // rank 12 is yesterday's rewrite. limit = 10, m = 2.
    let mut base: Vec<Candidate> = (0..11)
        .map(|i| candidate(i as i64 + 1, i + 1, git_days_ago(1800)))
        .collect();
    base.push(candidate(12, 12, git_days_ago(1)));

    let result = pipeline(&base, 12, 10, MAX_RANK_MOVEMENT);
    println!("F7 yesterday's rewrite at base_rank 12, limit 10, m=2 -> top-10 {result:?}");
    // It rises to rank 10... which is exactly the last slot of the result.
    assert!(result.contains(&12));

    // Push it one rank deeper and it is gone entirely, no matter how new.
    let mut deeper: Vec<Candidate> = (0..12)
        .map(|i| candidate(i as i64 + 1, i + 1, git_days_ago(1800)))
        .collect();
    deeper.push(candidate(13, 13, git_days_ago(0)));
    let result = pipeline(&deeper, 13, 10, MAX_RANK_MOVEMENT);
    assert!(
        !result.contains(&13),
        "a document written today at base_rank 13 cannot be surfaced"
    );

    // And the freshness of the buried document is irrelevant: the bound is
    // purely positional, so even `working_tree` (the newest basis there is)
    // stays out.
    let mut dirty = deeper.clone();
    dirty[12] = candidate(
        13,
        13,
        Basis::WorkingTree {
            latest_committed: None,
        },
    );
    assert!(!pipeline(&dirty, 13, 10, MAX_RANK_MOVEMENT).contains(&13));
}

#[test]
fn f7_the_top_down_scan_spends_the_movement_budget_on_the_wrong_candidate() {
    // m = 1. Three candidates, freshness increasing downward.
    let base = vec![
        candidate(1, 1, git_days_ago(300)),
        candidate(2, 2, git_days_ago(200)),
        candidate(3, 3, git_days_ago(100)), // the freshest
    ];
    let mut order = base.clone();
    reorder(&mut order, 1);
    println!("F7 m=1 greedy scan: {:?} -> {:?}", ids(&base), ids(&order));

    // OBSERVED: the top-down scan swaps (1,2) first, which consumes candidate
    // 1's entire movement budget; candidate 3 - the freshest - is then blocked
    // and never moves at all. The alternative legal outcome [1,3,2] would have
    // surfaced the freshest document instead.
    assert_eq!(
        ids(&order),
        vec![2, 1, 3],
        "the freshest candidate does not move"
    );
    let freshest_final = order.iter().position(|c| c.id == 3).unwrap() + 1;
    assert_eq!(
        freshest_final, 3,
        "the freshest document is left where it started"
    );

    // The alternative is legal under the same bound - the rule simply cannot
    // reach it, because it is not reachable by a top-down adjacent scan.
    let alternative = [base[0], base[2], base[1]];
    for (position, cand) in alternative.iter().enumerate() {
        assert!(
            (position + 1).abs_diff(cand.base_rank) <= 1,
            "[1,3,2] respects m=1"
        );
    }
}

#[test]
fn f7_how_often_bounded_movement_fails_to_surface_the_freshest_reachable_document() {
    let mut rng = StdRng::seed_from_u64(0xF7_0000);
    const CORPORA: usize = 1_000;
    const LIMIT: usize = 5;
    let mut freshest_outside_result = 0usize;
    let mut freshest_reachable_but_left_behind = 0usize;
    let mut stale_kept_above_freshest = 0usize;
    let mut corpora_with_git_freshest = 0usize;

    for _ in 0..CORPORA {
        let size = rng.random_range(8..=20);
        let base = random_corpus(&mut rng, size);
        let retained = LIMIT + MAX_RANK_MOVEMENT;
        let mut order: Vec<Candidate> = base.iter().take(retained).copied().collect();
        reorder(&mut order, MAX_RANK_MOVEMENT);

        // The freshest git-basis candidate in the retained pool: the document a
        // user asking "what's current?" expects to see.
        let Some(freshest) = order
            .iter()
            .filter(|c| matches!(c.basis, Basis::Git { .. } | Basis::WorkingTree { .. }))
            .copied()
            .max_by_key(|c| match c.basis {
                Basis::WorkingTree { .. } => i64::MAX,
                Basis::Git { author_time } => author_time,
                _ => i64::MIN,
            })
        else {
            continue;
        };
        corpora_with_git_freshest += 1;
        let final_position = order.iter().position(|c| c.id == freshest.id).unwrap() + 1;
        if final_position > LIMIT {
            freshest_outside_result += 1;
            // Every candidate in a `limit + m` pool is within reach of the
            // result by arithmetic, so each miss is a failure of the RULE, not
            // of the retention bound. Counted separately to make that explicit.
            if freshest.base_rank <= LIMIT + MAX_RANK_MOVEMENT {
                freshest_reachable_but_left_behind += 1;
            }
        }
        // Comparable, strictly older candidates still sitting above it that it
        // could legally have displaced.
        let blocked = order
            .iter()
            .take(final_position - 1)
            .enumerate()
            .filter(|(position, above)| {
                strictly_newer(&freshest, above)
                    && freshest.base_rank.abs_diff(position + 1) <= MAX_RANK_MOVEMENT
            })
            .count();
        if blocked > 0 {
            stale_kept_above_freshest += 1;
        }
    }
    println!(
        "F7 {CORPORA} corpora (limit {LIMIT}, m {MAX_RANK_MOVEMENT}, {corpora_with_git_freshest} with a \
         git-basis freshest hit):\n     freshest hit missing from the top-{LIMIT}: {freshest_outside_result} \
         ({:.1}%)\n     of those, reachable within the bound but still left out: \
         {freshest_reachable_but_left_behind}\n     freshest hit left below a strictly older candidate it \
         could legally have displaced: {stale_kept_above_freshest} ({:.1}%)",
        100.0 * freshest_outside_result as f64 / corpora_with_git_freshest as f64,
        100.0 * stale_kept_above_freshest as f64 / corpora_with_git_freshest as f64
    );

    // The reachable-but-left-behind and blocked counts are the interesting
    // numbers: they are failures of the RULE, not of the bound's arithmetic.
    assert!(
        stale_kept_above_freshest > 0,
        "the greedy adjacent scan must be observed leaving reachable freshness on the table"
    );
    assert!(freshest_outside_result > 0);
    assert_eq!(
        freshest_reachable_but_left_behind, freshest_outside_result,
        "in a limit+m pool every miss is a rule failure, never a retention failure"
    );
}

// ===========================================================================
// F8 - head to head
// ===========================================================================

#[derive(Default, Debug)]
struct ModelScore {
    orders_changed: usize,
    max_displacement: usize,
    freshest_surfaced: usize,
    cross_basis_reorders: usize,
    unknown_moved: usize,
}

#[test]
fn f8_head_to_head_on_synthetic_corpora() {
    let mut rng = StdRng::seed_from_u64(0xF8_0000);
    const CORPORA: usize = 1_000;
    const LIMIT: usize = 5;
    let retained = LIMIT + MAX_RANK_MOVEMENT;

    let mut baseline = ModelScore::default();
    let mut bounded = ModelScore::default();
    let mut decayed = ModelScore::default();
    let mut evaluated = 0usize;

    for _ in 0..CORPORA {
        let size = rng.random_range(retained..=24);
        let corpus = random_corpus(&mut rng, size);
        let pool: Vec<Candidate> = corpus.iter().take(retained).copied().collect();
        let base_ids: Vec<i64> = ids(&pool);

        // MODEL B.
        let mut bounded_order = pool.clone();
        reorder(&mut bounded_order, MAX_RANK_MOVEMENT);
        let bounded_ids = ids(&bounded_order);

        // MODEL A over the same pool and the same real RRF scores.
        let ranking: Vec<(i64, f64)> = base_ids.iter().map(|id| (*id, 0.0)).collect();
        let fused = rrf_stable(&[ranking]);
        // INVENTED (spec gap): unknown provenance has no age; the most
        // charitable reading of "no penalty" is multiplier 1.0, i.e. age 0.
        let ages: Vec<f64> = pool.iter().map(|c| c.age_days.unwrap_or(0.0)).collect();
        let decay_ids = decay_order(&fused, &ages);

        let freshest = pool
            .iter()
            .filter(|c| matches!(c.basis, Basis::Git { .. } | Basis::WorkingTree { .. }))
            .copied()
            .max_by_key(|c| match c.basis {
                Basis::WorkingTree { .. } => i64::MAX,
                Basis::Git { author_time } => author_time,
                _ => i64::MIN,
            });
        let Some(freshest) = freshest else { continue };
        evaluated += 1;

        for (score, order) in [
            (&mut baseline, &base_ids),
            (&mut bounded, &bounded_ids),
            (&mut decayed, &decay_ids),
        ] {
            if *order != base_ids {
                score.orders_changed += 1;
            }
            for (position, id) in order.iter().enumerate() {
                let base_rank = pool.iter().find(|c| c.id == *id).unwrap().base_rank;
                score.max_displacement = score
                    .max_displacement
                    .max((position + 1).abs_diff(base_rank));
            }
            if order.iter().take(LIMIT).any(|id| *id == freshest.id) {
                score.freshest_surfaced += 1;
            }
            // Provenance discipline: did anything reorder across bases, or did
            // an unknown-provenance candidate move?
            for (position, id) in order.iter().enumerate() {
                let candidate = pool.iter().find(|c| c.id == *id).unwrap();
                if candidate.basis == Basis::Unknown && position + 1 != candidate.base_rank {
                    score.unknown_moved += 1;
                }
            }
            for upper in 0..order.len() {
                for lower in (upper + 1)..order.len() {
                    let up = pool.iter().find(|c| c.id == order[upper]).unwrap();
                    let low = pool.iter().find(|c| c.id == order[lower]).unwrap();
                    let base_inverted = low.base_rank < up.base_rank;
                    let comparable = strictly_newer(low, up) || strictly_newer(up, low);
                    if base_inverted && !comparable {
                        score.cross_basis_reorders += 1;
                    }
                }
            }
        }
    }

    let percent = |value: usize| 100.0 * value as f64 / evaluated as f64;
    println!(
        "F8 {evaluated} corpora, pool {retained}, limit {LIMIT}:\n\
         \x20    model            orders changed   max displacement   freshest in top-{LIMIT}   cross-basis reorders   unknown moved\n\
         \x20    no-freshness     {:>6} ({:>5.1}%)   {:>16}   {:>16.1}%   {:>20}   {:>13}\n\
         \x20    bounded (m={MAX_RANK_MOVEMENT})    {:>6} ({:>5.1}%)   {:>16}   {:>16.1}%   {:>20}   {:>13}\n\
         \x20    decay (30%)      {:>6} ({:>5.1}%)   {:>16}   {:>16.1}%   {:>20}   {:>13}",
        baseline.orders_changed, percent(baseline.orders_changed), baseline.max_displacement,
        percent(baseline.freshest_surfaced), baseline.cross_basis_reorders, baseline.unknown_moved,
        bounded.orders_changed, percent(bounded.orders_changed), bounded.max_displacement,
        percent(bounded.freshest_surfaced), bounded.cross_basis_reorders, bounded.unknown_moved,
        decayed.orders_changed, percent(decayed.orders_changed), decayed.max_displacement,
        percent(decayed.freshest_surfaced), decayed.cross_basis_reorders, decayed.unknown_moved,
    );

    // The no-freshness arm is the control: it changes nothing, by definition.
    assert_eq!(baseline.orders_changed, 0);
    assert_eq!(baseline.max_displacement, 0);
    assert_eq!(baseline.cross_basis_reorders, 0);

    // MODEL B keeps every promise it makes about movement and provenance.
    assert!(bounded.max_displacement <= MAX_RANK_MOVEMENT);
    assert_eq!(bounded.unknown_moved, 0, "unknown provenance never moves");
    assert_eq!(
        bounded.cross_basis_reorders, 0,
        "no incomparable pair ever reorders"
    );

    // MODEL A keeps none of them: it moves candidates far past any bound,
    // reorders across incomparable clocks, and moves unknown-provenance rows.
    assert!(
        decayed.max_displacement > MAX_RANK_MOVEMENT,
        "the multiplicative model must be observed exceeding the bound"
    );
    assert!(
        decayed.cross_basis_reorders > 0,
        "the multiplicative model reorders incomparable pairs"
    );
    assert!(
        decayed.unknown_moved > 0,
        "the multiplicative model moves unknown-provenance rows"
    );

    // The product goal: does bounded reordering actually surface fresher
    // content? It does - modestly, and strictly less often than the unbounded
    // model, which is the trade the plan is knowingly making.
    assert!(
        bounded.freshest_surfaced > baseline.freshest_surfaced,
        "bounded reordering must surface the freshest hit more often than no freshness at all"
    );
    assert!(
        decayed.freshest_surfaced >= bounded.freshest_surfaced,
        "the unbounded model surfaces at least as much freshness - at the cost of every guarantee"
    );
    assert!(bounded.orders_changed > 0 && bounded.orders_changed < evaluated);
}

#[test]
fn f8_movement_bounds_one_to_three_trade_freshness_against_relevance_disruption() {
    // The plan's own evaluation asks to compare `--no-freshness` against
    // movement bounds of 1-3. Same corpora, same retained pool for every arm
    // (limit + 3, the deepest retention any arm needs) so the "freshest hit"
    // target is the same document in all three arms; a per-arm pool would make
    // the arms measure different questions.
    const CORPORA: usize = 800;
    const LIMIT: usize = 5;
    let retained = LIMIT + 3;
    let mut rows: Vec<(usize, usize, usize, usize, usize)> = Vec::new();

    for max_rank_movement in 1..=3usize {
        let mut local = StdRng::seed_from_u64(0xF8_1111);
        let mut changed = 0usize;
        let mut surfaced = 0usize;
        let mut evaluated = 0usize;
        let mut worst_movement = 0usize;
        for _ in 0..CORPORA {
            let size = local.random_range(12..=24);
            let corpus = random_corpus(&mut local, size);
            let pool: Vec<Candidate> = corpus.iter().take(retained).copied().collect();
            let Some(freshest) = pool
                .iter()
                .filter(|c| matches!(c.basis, Basis::Git { .. } | Basis::WorkingTree { .. }))
                .copied()
                .max_by_key(|c| match c.basis {
                    Basis::WorkingTree { .. } => i64::MAX,
                    Basis::Git { author_time } => author_time,
                    _ => i64::MIN,
                })
            else {
                continue;
            };
            evaluated += 1;
            let base_ids = ids(&pool);
            let mut order = pool.clone();
            reorder(&mut order, max_rank_movement);
            if ids(&order) != base_ids {
                changed += 1;
            }
            if ids(&order).iter().take(LIMIT).any(|id| *id == freshest.id) {
                surfaced += 1;
            }
            for (position, cand) in order.iter().enumerate() {
                worst_movement = worst_movement.max((position + 1).abs_diff(cand.base_rank));
            }
        }
        rows.push((
            max_rank_movement,
            evaluated,
            changed,
            surfaced,
            worst_movement,
        ));
    }

    println!(
        "F8 movement bound sweep (limit {LIMIT}, retained pool {retained}, identical corpora):"
    );
    for (m, evaluated, changed, surfaced, worst_movement) in &rows {
        println!(
            "     m={m}: {changed}/{evaluated} result orders changed ({:.1}%), freshest hit in \
             top-{LIMIT} {:.1}%, worst movement {worst_movement}",
            100.0 * *changed as f64 / *evaluated as f64,
            100.0 * *surfaced as f64 / *evaluated as f64
        );
    }
    // OBSERVED: the *number* of disturbed orders barely moves with m (any m >= 1
    // is enough to fire at least one swap in nearly every corpus). What m buys
    // is how FAR candidates travel, and with it how often the freshest hit
    // actually reaches the result. That is the real shape of the 1-3 trade.
    assert!(
        rows[0].2 <= rows[1].2 && rows[1].2 <= rows[2].2,
        "larger bounds change more orders"
    );
    assert!(
        rows[0].3 <= rows[1].3 && rows[1].3 <= rows[2].3,
        "larger bounds surface the freshest hit at least as often: {rows:?}"
    );
    assert_eq!(
        (rows[0].4, rows[1].4, rows[2].4),
        (1, 2, 3),
        "each arm reaches exactly its own bound and never exceeds it"
    );
}
