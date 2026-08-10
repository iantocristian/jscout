//! Semantic scouting: generative runs over deterministic candidates.
//!
//! Every model call is recorded in the run ledger before it can publish
//! anything; failed, incomplete, and canceled runs stay attributable and
//! never create semantic artifacts.

#[allow(dead_code)] // consumed by workflow scouting orchestration (G4)
pub mod ledger;
