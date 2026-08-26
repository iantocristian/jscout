//! Deterministic, relevance-bounded freshness ordering for documentation hits.
//!
//! This module only defines the partial order and the adjacent-swap algorithm.
//! Resolving provenance and attaching it to retrieval candidates are separate
//! indexing and retrieval concerns.

/// The freshness clock available for one documentation retrieval chunk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FreshnessBasis {
    /// Latest usable Git author timestamp among the chunk's contributing lines.
    Git { author_time: i64 },
    /// At least one contributing body line differs from the recorded commit.
    WorkingTree {
        /// Informational only: all working-tree chunks tie for ordering.
        latest_committed_author_time: Option<i64>,
    },
    /// Latest freshness-bearing observation event for the chunk.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "reserved for the deferred observation ledger")
    )]
    Observed {
        snapshot_sequence: i64,
        observed_at: i64,
    },
    /// No usable freshness clock is available.
    Unknown,
}

impl FreshnessBasis {
    /// Stable external name for the basis.
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Git { .. } => "git",
            Self::WorkingTree { .. } => "working_tree",
            Self::Observed { .. } => "observed",
            Self::Unknown => "unknown",
        }
    }

    /// Structured value behind the basis name.
    ///
    /// Keeping this structured lets CLI and JSON renderers choose their own
    /// timestamp representation without changing ordering semantics.
    #[must_use]
    pub const fn value(self) -> Option<FreshnessValue> {
        match self {
            Self::Git { author_time } => Some(FreshnessValue::GitAuthorTime(author_time)),
            Self::WorkingTree { .. } => Some(FreshnessValue::Uncommitted),
            Self::Observed {
                snapshot_sequence,
                observed_at,
            } => Some(FreshnessValue::Observed {
                snapshot_sequence,
                observed_at,
            }),
            Self::Unknown => None,
        }
    }

    /// Whether `self` is strictly newer than `other` on a comparable clock.
    #[must_use]
    pub const fn is_strictly_newer_than(self, other: Self) -> bool {
        match (self, other) {
            (Self::WorkingTree { .. }, Self::Git { .. }) => true,
            (Self::Git { .. }, Self::WorkingTree { .. })
            | (Self::WorkingTree { .. }, Self::WorkingTree { .. }) => false,
            (Self::Git { author_time: lower }, Self::Git { author_time: upper }) => lower > upper,
            (
                Self::Observed {
                    snapshot_sequence: lower,
                    ..
                },
                Self::Observed {
                    snapshot_sequence: upper,
                    ..
                },
            ) => lower > upper,
            _ => false,
        }
    }
}

/// Basis-specific value exposed by retrieval surfaces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FreshnessValue {
    GitAuthorTime(i64),
    Uncommitted,
    Observed {
        snapshot_sequence: i64,
        observed_at: i64,
    },
}

/// Relevance and final rank metadata for one candidate in final order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RankMovement {
    /// One-based rank before freshness ordering.
    pub base_rank: usize,
    /// One-based rank after freshness ordering.
    pub final_rank: usize,
    /// `base_rank - final_rank`; positive means promotion.
    pub movement: i64,
}

/// Reorder candidates using the G24 bounded adjacent-swap rule.
///
/// The input order is the relevance order. A lower adjacent candidate swaps
/// upward only when it is strictly newer on a comparable clock and neither
/// candidate would move farther than `max_rank_movement` from its original
/// rank. Top-to-bottom passes repeat until a complete pass makes no swap.
/// Ties and incomparable bases retain relevance order.
#[must_use]
pub fn reorder<T>(
    candidates: &mut [T],
    max_rank_movement: usize,
    freshness_of: impl Fn(&T) -> FreshnessBasis,
) -> Vec<RankMovement> {
    let mut base_ranks = (1..=candidates.len()).collect::<Vec<_>>();

    if max_rank_movement > 0 && candidates.len() > 1 {
        loop {
            let mut swapped = false;

            for upper_index in 0..candidates.len() - 1 {
                let lower_index = upper_index + 1;
                let upper_new_rank = lower_index + 1;
                let lower_new_rank = upper_index + 1;
                let upper_stays_bounded =
                    base_ranks[upper_index].abs_diff(upper_new_rank) <= max_rank_movement;
                let lower_stays_bounded =
                    base_ranks[lower_index].abs_diff(lower_new_rank) <= max_rank_movement;

                if upper_stays_bounded
                    && lower_stays_bounded
                    && freshness_of(&candidates[lower_index])
                        .is_strictly_newer_than(freshness_of(&candidates[upper_index]))
                {
                    candidates.swap(upper_index, lower_index);
                    base_ranks.swap(upper_index, lower_index);
                    swapped = true;
                }
            }

            if !swapped {
                break;
            }
        }
    }

    base_ranks
        .into_iter()
        .enumerate()
        .map(|(final_index, base_rank)| {
            let final_rank = final_index + 1;
            RankMovement {
                base_rank,
                final_rank,
                movement: signed_movement(base_rank, final_rank),
            }
        })
        .collect()
}

fn signed_movement(base_rank: usize, final_rank: usize) -> i64 {
    if base_rank >= final_rank {
        i64::try_from(base_rank - final_rank)
            .expect("a documentation result set cannot exceed i64::MAX candidates")
    } else {
        -i64::try_from(final_rank - base_rank)
            .expect("a documentation result set cannot exceed i64::MAX candidates")
    }
}

#[cfg(test)]
mod tests {
    use super::{FreshnessBasis, FreshnessValue, RankMovement, reorder};

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct Candidate {
        id: usize,
        freshness: FreshnessBasis,
    }

    const fn candidate(id: usize, freshness: FreshnessBasis) -> Candidate {
        Candidate { id, freshness }
    }

    fn reorder_candidates(candidates: &mut [Candidate], bound: usize) -> Vec<RankMovement> {
        reorder(candidates, bound, |candidate| candidate.freshness)
    }

    #[test]
    fn wire_names_and_structured_values_are_stable() {
        let cases = [
            (
                FreshnessBasis::Git { author_time: 17 },
                "git",
                Some(FreshnessValue::GitAuthorTime(17)),
            ),
            (
                FreshnessBasis::WorkingTree {
                    latest_committed_author_time: Some(11),
                },
                "working_tree",
                Some(FreshnessValue::Uncommitted),
            ),
            (
                FreshnessBasis::Observed {
                    snapshot_sequence: 9,
                    observed_at: 23,
                },
                "observed",
                Some(FreshnessValue::Observed {
                    snapshot_sequence: 9,
                    observed_at: 23,
                }),
            ),
            (FreshnessBasis::Unknown, "unknown", None),
        ];

        for (basis, name, value) in cases {
            assert_eq!(basis.wire_name(), name);
            assert_eq!(basis.value(), value);
        }
    }

    #[test]
    fn repeated_top_down_passes_bubble_newest_git_candidate() {
        let mut candidates = [
            candidate(1, FreshnessBasis::Git { author_time: 10 }),
            candidate(2, FreshnessBasis::Git { author_time: 20 }),
            candidate(3, FreshnessBasis::Git { author_time: 30 }),
        ];

        let ranks = reorder_candidates(&mut candidates, 2);

        assert_eq!(candidates.map(|candidate| candidate.id), [3, 2, 1]);
        assert_eq!(
            ranks,
            [
                RankMovement {
                    base_rank: 3,
                    final_rank: 1,
                    movement: 2,
                },
                RankMovement {
                    base_rank: 2,
                    final_rank: 2,
                    movement: 0,
                },
                RankMovement {
                    base_rank: 1,
                    final_rank: 3,
                    movement: -2,
                },
            ]
        );
    }

    #[test]
    fn original_rank_guard_bounds_both_sides_of_every_swap() {
        let mut candidates = [
            candidate(1, FreshnessBasis::Git { author_time: 10 }),
            candidate(2, FreshnessBasis::Git { author_time: 20 }),
            candidate(3, FreshnessBasis::Git { author_time: 30 }),
            candidate(4, FreshnessBasis::Git { author_time: 40 }),
        ];

        let ranks = reorder_candidates(&mut candidates, 1);

        assert_eq!(candidates.map(|candidate| candidate.id), [2, 1, 4, 3]);
        assert!(ranks.iter().all(|rank| rank.movement.abs() <= 1));
    }

    #[test]
    fn zero_bound_preserves_relevance_order_and_reports_zero_movement() {
        let mut candidates = [
            candidate(1, FreshnessBasis::Git { author_time: 10 }),
            candidate(
                2,
                FreshnessBasis::WorkingTree {
                    latest_committed_author_time: None,
                },
            ),
        ];

        let ranks = reorder_candidates(&mut candidates, 0);

        assert_eq!(candidates.map(|candidate| candidate.id), [1, 2]);
        assert_eq!(
            ranks,
            [
                RankMovement {
                    base_rank: 1,
                    final_rank: 1,
                    movement: 0,
                },
                RankMovement {
                    base_rank: 2,
                    final_rank: 2,
                    movement: 0,
                },
            ]
        );
    }

    #[test]
    fn working_tree_is_newer_than_git_but_ties_with_working_tree() {
        let mut candidates = [
            candidate(
                1,
                FreshnessBasis::Git {
                    author_time: i64::MAX,
                },
            ),
            candidate(
                2,
                FreshnessBasis::WorkingTree {
                    latest_committed_author_time: Some(1),
                },
            ),
            candidate(
                3,
                FreshnessBasis::WorkingTree {
                    latest_committed_author_time: Some(999),
                },
            ),
        ];

        reorder_candidates(&mut candidates, 2);

        assert_eq!(candidates.map(|candidate| candidate.id), [2, 3, 1]);
    }

    #[test]
    fn observed_uses_sequence_and_not_timestamp() {
        let mut candidates = [
            candidate(
                1,
                FreshnessBasis::Observed {
                    snapshot_sequence: 4,
                    observed_at: 100,
                },
            ),
            candidate(
                2,
                FreshnessBasis::Observed {
                    snapshot_sequence: 4,
                    observed_at: 200,
                },
            ),
            candidate(
                3,
                FreshnessBasis::Observed {
                    snapshot_sequence: 5,
                    observed_at: 50,
                },
            ),
        ];

        reorder_candidates(&mut candidates, 2);

        assert_eq!(candidates.map(|candidate| candidate.id), [3, 1, 2]);
    }

    #[test]
    fn git_and_observed_are_incomparable_barriers() {
        let mut candidates = [
            candidate(1, FreshnessBasis::Git { author_time: 1 }),
            candidate(
                2,
                FreshnessBasis::Observed {
                    snapshot_sequence: i64::MAX,
                    observed_at: i64::MAX,
                },
            ),
            candidate(3, FreshnessBasis::Git { author_time: 3 }),
        ];

        let ranks = reorder_candidates(&mut candidates, usize::MAX);

        assert_eq!(candidates.map(|candidate| candidate.id), [1, 2, 3]);
        assert!(ranks.iter().all(|rank| rank.movement == 0));
    }

    #[test]
    fn unknown_never_moves_or_allows_another_candidate_to_cross() {
        let mut candidates = [
            candidate(1, FreshnessBasis::Git { author_time: 1 }),
            candidate(2, FreshnessBasis::Unknown),
            candidate(
                3,
                FreshnessBasis::WorkingTree {
                    latest_committed_author_time: Some(1),
                },
            ),
        ];

        let ranks = reorder_candidates(&mut candidates, usize::MAX);

        assert_eq!(candidates.map(|candidate| candidate.id), [1, 2, 3]);
        assert!(ranks.iter().all(|rank| rank.movement == 0));
    }

    #[test]
    fn equal_freshness_retains_relevance_order() {
        let mut candidates = [
            candidate(1, FreshnessBasis::Git { author_time: 7 }),
            candidate(2, FreshnessBasis::Git { author_time: 7 }),
            candidate(3, FreshnessBasis::Git { author_time: 7 }),
        ];

        reorder_candidates(&mut candidates, usize::MAX);

        assert_eq!(candidates.map(|candidate| candidate.id), [1, 2, 3]);
    }

    #[test]
    fn empty_and_singleton_inputs_are_total() {
        let mut empty: [Candidate; 0] = [];
        assert!(reorder_candidates(&mut empty, 3).is_empty());

        let mut singleton = [candidate(1, FreshnessBasis::Unknown)];
        assert_eq!(
            reorder_candidates(&mut singleton, 3),
            [RankMovement {
                base_rank: 1,
                final_rank: 1,
                movement: 0,
            }]
        );
    }

    #[test]
    fn exhaustive_small_inputs_preserve_permutation_bounds_and_local_stability() {
        const BASES: [FreshnessBasis; 6] = [
            FreshnessBasis::Unknown,
            FreshnessBasis::Git { author_time: 1 },
            FreshnessBasis::Git { author_time: 2 },
            FreshnessBasis::WorkingTree {
                latest_committed_author_time: Some(1),
            },
            FreshnessBasis::Observed {
                snapshot_sequence: 1,
                observed_at: 20,
            },
            FreshnessBasis::Observed {
                snapshot_sequence: 2,
                observed_at: 10,
            },
        ];

        for encoded in 0_usize..BASES.len().pow(5) {
            let mut remaining = encoded;
            let mut input = [candidate(0, FreshnessBasis::Unknown); 5];
            for (id, slot) in input.iter_mut().enumerate() {
                *slot = candidate(id, BASES[remaining % BASES.len()]);
                remaining /= BASES.len();
            }

            for bound in 0..=3 {
                let mut actual = input;
                let ranks = reorder_candidates(&mut actual, bound);
                let mut ids = actual.map(|candidate| candidate.id);
                ids.sort_unstable();
                assert_eq!(ids, [0, 1, 2, 3, 4]);

                for (final_index, (candidate, rank)) in actual.iter().zip(ranks.iter()).enumerate()
                {
                    let final_rank = final_index + 1;
                    assert_eq!(rank.base_rank, candidate.id + 1);
                    assert_eq!(rank.final_rank, final_rank);
                    assert_eq!(
                        rank.movement,
                        i64::try_from(rank.base_rank).unwrap() - i64::try_from(final_rank).unwrap()
                    );
                    assert!(rank.base_rank.abs_diff(final_rank) <= bound);
                }

                for upper_index in 0..actual.len() - 1 {
                    let lower_index = upper_index + 1;
                    let upper_new_rank = lower_index + 1;
                    let lower_new_rank = upper_index + 1;
                    let swap_would_be_bounded =
                        ranks[upper_index].base_rank.abs_diff(upper_new_rank) <= bound
                            && ranks[lower_index].base_rank.abs_diff(lower_new_rank) <= bound;
                    assert!(
                        !swap_would_be_bounded
                            || !actual[lower_index]
                                .freshness
                                .is_strictly_newer_than(actual[upper_index].freshness),
                        "eligible inversion remained for encoded={encoded} bound={bound}"
                    );
                }
            }
        }
    }
}
