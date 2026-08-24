//! Harness for empirically testing the assumptions in the G24 Markdown
//! retrieval proposal (jscout PR #96).
//!
//! This crate is deliberately a *prototype of the specified mechanisms*, not a
//! reimplementation of jscout. Each module implements exactly what the plan
//! specifies so the integration tests can check whether the specified rule
//! actually produces the outcome the plan claims.
//!
//! Layout:
//! - [`md`]   — front matter, block parsing, chunking, embedding identity
//! - [`git`]  — real-git laboratory + line-porcelain blame parsing
//! - [`proc`] — process runner used to drive the real `jscout` binary

pub mod git;
pub mod md;
pub mod proc;
