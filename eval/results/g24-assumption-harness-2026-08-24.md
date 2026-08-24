# G24 assumption harness: first full run

- Date: 2026-08-24
- Corpus: synthetic fixtures, disposable git repositories, and a temp-directory jscout index
- Change under test: the G24 Markdown retrieval proposal at `4d41cac` (documentation only; no runtime code)
- Harness: [../g24-harness](../g24-harness) — 115 tests, git 2.49.0, jscout 0.4.0 release binary

## What was run

94 assumption records were extracted from the proposal and the `Proposed G24`
entry in `PLAN.md`, then tested against real `git` and the real `jscout` binary.
Every record that did not hold was sent to an independent adjudicator instructed
to default to blaming the harness rather than the plan.

| Outcome | Count |
|---|---:|
| Held outright | 65 |
| Flagged, adjudicated **harness bug** | 20 |
| Flagged, historical confirmation of the superseded decay model | 4 |
| Flagged, **plan defect confirmed** | 3 |
| Flagged, **spec ambiguity that forks implementations** | 2 |

The strawman rate is the headline methodological result: two thirds of the
apparent contradictions were the harness attributing a claim the plan never
made. Raw test output would have been mostly noise.

## Confirmed defects

### 1. The blame hardening is unimplementable as written

The proposal specifies that provenance commands disable replace objects "with
`--no-replace-objects`" and that blame clears ignore-revs "with
`-c blame.ignoreRevsFile=`". Both are written as blame *subcommand* options.
git 2.49 rejects that:

```
$ git blame --line-porcelain --no-replace-objects -- f
error: unknown option `no-replace-objects'
$ echo $?
129
```

Both are git-*level* options and must precede the subcommand:
`git --no-replace-objects blame …`, `git -c blame.ignoreRevsFile= blame …`.
Verified working once hoisted.

The rule is worth keeping in the corrected syntax: both halves of the
ignore-revs concern reproduce. A configured `blame.ignoreRevsFile` really does
change attribution, and clearing it really does restore it, so ambient
repository config can silently rewrite freshness if it is not neutralized.

### 2. The `moved` flag's "changed path" clause is unreachable

`moved` is defined as "the matched block changed path or reordered relative to
matched neighboring blocks". The first half cannot occur. `moved` applies only
to a *matched* block; every rule that can produce a match is path-scoped
("within each unchanged path", "repeated exact hashes within one path",
"between the same immediately adjacent matched neighbors in one document"); and
rule 3 bans the remaining route outright. A rename produces `removed` plus
`added`, never `moved`.

### 3. "Document-edge edits" is filed under a predicate that is false for it

The ambiguity list covers cases that leave "more than one predecessor or
successor possible", and names document-edge edits among them. Enumerating the
pairings shows a singleton edge edit leaves exactly *one* possible pairing:
after rules 1–2 the unmatched sets are `old=[0]`, `new=[0]`, so the only
candidate pairing is `(0,0)`. The exclusion is still correct, for a different
reason — a document boundary is not a matched neighbor, so there is no evidence
of correspondence — and rule 4's own "exactly one … and one" count test already
excludes the genuinely ambiguous multi-block edge.

## Ambiguities that would fork implementations

**`moved` is defined twice, incompatibly.** One sentence says "relative to
matched **neighboring** blocks" (adjacent pairs); another says "against **other
matched** blocks" (all pairs). For one block relocated past three others, the
readings disagree: 4 rows versus 1. Neither is a minimal-inversion reading,
which is probably what is intended.

**The glob dialect is unnamed.** With `exclude = ["drafts/"]` the outcome
depends on unstated choices. Under gitignore semantics
`matched("drafts", true)` ignores the directory, `matched("drafts/wip.md", false)`
does not, and `matched_path_or_any_parents(…)` does — three different corpora,
all defensible readings of "exclude globs … matching files".

## The superseded decay model, measured

An earlier revision applied a multiplicative time decay to fused scores and
claimed a 30% cap "prevents a recent but weakly relevant passage from defeating
a substantially better older passage". Review refuted that analytically; this
run measured it, and re-derived it in exact rational arithmetic so no
floating-point artifact is load-bearing.

| Candidate pool | Worst-case promotion |
|---:|---:|
| 30 | 26 ranks |
| 80 | 41 ranks |
| 200 | 77 ranks |
| 500 | 167 ranks |

Displacement grows linearly with pool depth, with slope exactly `max_penalty`;
there is no pool-independent bound. On jscout's **real** score ladder, taken
from `search --debug-json`, a 10% penalty moves a hit 6 ranks, 25% moves 20, and
50% moves 29 — past every retrieved candidate. Removing the model was correct.

The replacement rule passed its property tests: over 600 random corpora no
candidate ever moved more than `max_rank_movement` from its base rank, unknown
provenance never moved, and git and observed candidates never reordered against
each other.

## The existing system, confirmed against the binary

Each claim behind the separate-database decision reproduces, several more
sharply than review stated:

- a committed `[docs]` section breaks **11 commands** — `config show`,
  `config validate`, `index`, `search`, `stats`, `chunks`, `events`,
  `who-uses`, `overview`, `memory`, `neighborhood`. Only `--version`, `--help`,
  and `agent-guide` survive, because they never read the config file;
- the single global `schema_version` gate is real, and a refused `index` does
  **not** repair the file — the foreign value stays on disk, so the only
  recovery is a new database;
- **new**: a *second* independent global gate exists, `projection_version`
  (v12). A shared-file docs plane would be subject to two, not one;
- the sharpest evidence for separation: deleting the `snapshot` key breaks
  `jscout memory`, so the *semantic* plane is gated by the *structural*
  snapshot key;
- a SIGKILL sweep across the index write phase landed in the unpublished window
  in 5–6 of 10 samples. Content was complete (3000 files, 9000 chunks) while
  `snapshot`, `projection_version`, and `resolution_hash` were all absent and
  every reader refused;
- positive control for the chosen design: overwriting `.jscout-docs.db`'s header
  with `GARBAGE-NOT-SQLITE` and bumping its version left main-plane `search`,
  `who-uses`, and `index` all exiting 0, and the reverse held too.

RRF `k = 60` is now pinned empirically (1/61, 1/62, … 1/80 over 20 ranks). The
proposal never names `k`; the docs plane should reuse this value.

One refinement to the review's wording: a failure raised *before* the write
transaction — a read-only database, for instance — preserves publication
entirely, and a per-file read rejection is not a publication failure at all
(`index` succeeds and republishes). "Failed index" is more precisely "index
killed or failed during the write phase". The architectural conclusion is
unchanged.

## Hazards no review round caught

**A UTF-8 BOM defeats the corpus rules.** `\u{feff}# Heading` parses as a
paragraph rather than an H1, and `\u{feff}---` is not recognized as front
matter, so a BOM-prefixed document silently falls back to its file stem for a
title. The proposal never says whether a BOM is stripped.

**git's porcelain `boundary` marker does not mean "shallow".** Git marks
root-commit lines as `boundary` in ordinary complete repositories. Reading the
rule "a chunk whose contributing lines all blame to a boundary commit has
unknown git age" off that marker would erase the age of every line still
attributed to a repository's first commit. `--root` does not disambiguate — it
also clears the marker on a shallow graft, because a grafted commit has no
parents. The only reliable discriminator is membership in `.git/shallow`.

## Underspecification

The implementation pass recorded 70 points where the proposal did not decide
something the implementation had to, each marked `INVENTED:` at its point of use
in `src/md.rs`. The consequential ones:

- the exact bytes of the embedding wire format, which are part of the identity
  while only its three inputs are named;
- front-matter delimiter precision (`----`, `--- x`, an indented `  ---`, YAML's
  `...` terminator);
- whether a thematic break ends a chunk;
- `normal_max` versus the split threshold — read literally, a single 5,000-byte
  paragraph is one chunk above `normal_max` but below the hard bound;
- traversal and corpus order, while `doc_snapshots` carries an order-sensitive
  corpus fingerprint;
- symlink policy;
- whether HTML-comment stripping scans code content.

## Harness bugs found

Recorded for calibration, since a validation report that hides its own error
rate is not worth much: `embedding_input` ignored its bounds argument and used
defaults, which produced one spurious "violation"; and several tests asserted
claims stitched from plan text across an ellipsis that removed the operative
conditional. Both classes were caught by adjudication, not by the tests passing.
