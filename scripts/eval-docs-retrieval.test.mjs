import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";

import {
  compareFreshnessTreatments,
  comparePhase2RankedIdentities,
  compareProfiles,
  configWithFreshness,
  docsSearchArguments,
  hasConflictTreatmentOpportunity,
  materializeRepositories,
  parseArguments,
  phase2ValidityForReport,
  scoreRun,
  selectPhase3Default,
  summarizeProfile,
  validateManifest,
  validatePhase2ProviderConfiguration,
  validatePhase2Report,
  validatePhase2ServiceConfiguration,
  validateQrelsAgainstRepositories,
} from "./eval-docs-retrieval.mjs";

const qrel = (answerId, path, heading) => ({ answer_id: answerId, path, heading });
const hit = (path, heading, rank, suffix = rank) => ({
  rank,
  path,
  heading,
  source_bytes: [rank * 10, rank * 10 + 5],
  file_hash: `hash-${suffix}`,
});

test("scorer separates current recall, older visibility, and evergreen inversion", () => {
  const conflict = {
    id: "deploy",
    category: "conflict",
    current: [qrel("deploy-current", "docs/current.md", "Deploy > Rollout")],
    older_conflicts: [qrel("deploy-old", "docs/old.md", "Deploy > Rollout")],
  };
  const conflictScore = scoreRun(conflict, [
    hit("docs/old.md", "Deploy > Rollout", 1),
    hit("docs/current.md", "Deploy > Rollout", 2),
  ]);
  assert.equal(conflictScore.current_rank, 2);
  assert.equal(conflictScore.current_recall[1], 0);
  assert.equal(conflictScore.current_recall[3], 1);
  assert.equal(conflictScore.older_conflict_visible_at_5, true);
  assert.equal(conflictScore.current_ahead_of_older, false);
  assert.equal(hasConflictTreatmentOpportunity([{
    query: conflict,
    score: conflictScore,
  }]), true);
  assert.equal(hasConflictTreatmentOpportunity([{
    query: conflict,
    score: scoreRun(conflict, [
      hit("docs/current.md", "Deploy > Rollout", 1),
      hit("docs/old.md", "Deploy > Rollout", 2),
    ]),
  }]), false);

  const evergreen = {
    id: "identity",
    category: "evergreen",
    current: [qrel("identity-spec", "docs/spec.md", "Identity > Contract")],
    recent_irrelevant: [qrel("identity-note", "CHANGELOG.md", "Changelog > Identity")],
  };
  const evergreenScore = scoreRun(evergreen, [
    hit("CHANGELOG.md", "Changelog > Identity", 1),
    hit("docs/spec.md", "Identity > Contract", 2),
  ]);
  assert.equal(evergreenScore.recent_irrelevant_inversion, true);
});

test("profile summary and comparison report recall lift and exact-order parity", () => {
  const query = {
    id: "deploy",
    category: "conflict",
    current: [qrel("current", "current.md", "Deploy")],
    older_conflicts: [qrel("old", "old.md", "Deploy")],
  };
  const lexicalHits = [hit("old.md", "Deploy", 1), hit("current.md", "Deploy", 2)];
  const hybridHits = [hit("current.md", "Deploy", 1), hit("old.md", "Deploy", 2)];
  const runs = [
    { variant: "clean", profile: "lexical", query, hits: lexicalHits, score: scoreRun(query, lexicalHits) },
    { variant: "clean", profile: "fallback", query, hits: lexicalHits, score: scoreRun(query, lexicalHits) },
    { variant: "clean", profile: "hybrid", query, hits: hybridHits, score: scoreRun(query, hybridHits) },
  ];
  const summary = summarizeProfile(runs.filter((run) => run.profile === "hybrid"));
  assert.equal(summary.current_answer_recall[1], 1);
  assert.deepEqual(summary.conflict_current_ahead, { numerator: 1, denominator: 1 });
  assert.equal(compareProfiles(runs, "lexical", "fallback").exact_order_parity, true);
  const hybrid = compareProfiles(runs, "lexical", "hybrid");
  assert.equal(hybrid.changed_orders, 1);
  assert.equal(hybrid.current_answer_recall_delta[1], 1);
});

test("fixture manifest builds full, shallow, non-Git, dirty, staged, and untracked repositories", () => {
  const manifestPath = resolve("eval/fixtures/docs-retrieval/manifest.json");
  const manifest = validateManifest(JSON.parse(readFileSync(manifestPath, "utf8")), manifestPath);
  const workspace = mkdtempSync(join(tmpdir(), "jscout-docs-eval-test-"));
  const env = {
    PATH: process.env.PATH,
    LANG: process.env.LANG ?? "C",
    GIT_CONFIG_GLOBAL: "/dev/null",
    GIT_CONFIG_NOSYSTEM: "1",
  };
  try {
    const repositories = materializeRepositories(manifest, manifestPath, workspace, env);
    validateQrelsAgainstRepositories(manifest, repositories);
    assert.equal(repositories.get("clean").status.length, 0);
    assert.equal(repositories.get("clean").shallow, false);
    assert.equal(repositories.get("shallow").shallow, true);
    assert.equal(repositories.get("non_git").head, null);
    assert.ok(repositories.get("dirty").status.some((line) => line.startsWith(" M ")));
    assert.ok(repositories.get("staged").status.some((line) => /^[AMDRC]/.test(line)));
    assert.ok(repositories.get("untracked").status.some((line) => line.startsWith("?? ")));
  } finally {
    rmSync(workspace, { recursive: true });
  }
});

test("runner fixes the response budget and requires complete pinned Phase 2 treatments", () => {
  const args = docsSearchArguments("hybrid", "/repo", "query", "/index.db");
  const responseBytes = args.indexOf("--response-bytes");
  assert.notEqual(responseBytes, -1);
  assert.equal(args[responseBytes + 1], "1048576");
  assert.ok(args.includes("--debug-json"));
  const disabledArgs = docsSearchArguments(
    "lexical",
    "/repo",
    "query",
    "/index.db",
    { id: "disabled", enabled: false, bound: 1 },
  );
  assert.ok(disabledArgs.includes("--no-freshness"));

  assert.throws(
    () => parseArguments([
      "--output", "/result.json",
      "--run-kind", "phase2-baseline",
      "--profiles", "lexical,fallback",
    ]),
    /hybrid profiles|requires exactly/,
  );
  assert.throws(
    () => parseArguments(["--output", "/result.json", "--profiles", "lexical,lexical"]),
    /duplicates/,
  );
  const full = parseArguments([
    "--output", "/result.json",
    "--run-kind", "phase2-baseline",
    "--profiles", "lexical,fallback,hybrid,hybrid-rerank",
    "--provider-config", "/provider.toml",
  ]);
  assert.equal(full.runKind, "phase2-baseline");
  assert.throws(
    () => parseArguments([
      "--output", "/result.json",
      "--run-kind", "phase3-candidate",
      "--profiles", "lexical,hybrid,hybrid-rerank",
      "--provider-config", "/provider.toml",
    ]),
    /requires --phase2-report/,
  );
  const phase3 = parseArguments([
    "--output", "/result.json",
    "--run-kind", "phase3-candidate",
    "--profiles", "lexical,hybrid,hybrid-rerank",
    "--provider-config", "/provider.toml",
    "--phase2-report", "/phase2.json",
  ]);
  assert.equal(phase3.runKind, "phase3-candidate");

  const pinned = {
    embedding: {
      provider: "local",
      model: "BAAI/bge-m3",
      revision: "5617a9f61b028005a4858fdac845db406aefb181",
    },
    inference: { url: "http://127.0.0.1:8792/" },
    reranker: {
      url: "http://127.0.0.1:8792/rerank",
      model: "BAAI/bge-reranker-v2-m3",
      revision: "953dc6f6f85a1b2dbfca4c34a2796e7dde08d41e",
    },
  };
  assert.doesNotThrow(() => validatePhase2ProviderConfiguration(pinned));
  assert.throws(
    () => validatePhase2ProviderConfiguration({
      ...pinned,
      embedding: { ...pinned.embedding, revision: "main" },
    }),
    /embedding revision/,
  );
  const service = {
    available: true,
    provider: "local",
    embedding: {
      model: "BAAI/bge-m3",
      revision: "5617a9f61b028005a4858fdac845db406aefb181",
    },
    reranker: {
      model: "BAAI/bge-reranker-v2-m3",
      revision: "953dc6f6f85a1b2dbfca4c34a2796e7dde08d41e",
    },
  };
  assert.doesNotThrow(() => validatePhase2ServiceConfiguration(service));
  assert.throws(
    () => validatePhase2ServiceConfiguration({
      ...service,
      reranker: { ...service.reranker, revision: "main" },
    }),
    /reranker revision/,
  );
});

test("manifest validation rejects duplicate query ids before execution", () => {
  const directory = mkdtempSync(join(tmpdir(), "jscout-docs-manifest-test-"));
  const source = join(directory, "source.md");
  writeFileSync(source, "# Source\n");
  const query = {
    id: "duplicate",
    query: "source",
    category: "canonical",
    variants: ["clean"],
    current: [qrel("source", "source.md", "Source")],
  };
  const variants = ["clean", "shallow", "non_git", "dirty", "staged", "untracked"].map((kind) => ({
    id: kind,
    kind,
    ...(kind === "shallow" ? { depth: 1 } : {}),
  }));
  const manifest = {
    schema_version: 1,
    suite: "validation",
    commits: [
      { id: "one", date: "2020-01-01T00:00:00Z", message: "one", overlay: { "source.md": "source.md" } },
      { id: "two", date: "2021-01-01T00:00:00Z", message: "two", overlay: { "source.md": "source.md" } },
    ],
    variants,
    queries: [query, query],
  };
  try {
    assert.throws(() => validateManifest(manifest, join(directory, "manifest.json")), /duplicate query id/);
  } finally {
    rmSync(directory, { recursive: true });
  }
});

test("manifest validation contains fixture inputs and generated repository paths", () => {
  const directory = mkdtempSync(join(tmpdir(), "jscout-docs-manifest-safety-"));
  const externalDirectory = mkdtempSync(join(tmpdir(), "jscout-docs-manifest-external-"));
  writeFileSync(join(directory, "source.md"), "# Source\n");
  const externalSource = join(externalDirectory, "external.md");
  writeFileSync(externalSource, "# External\n");
  const variants = ["clean", "shallow", "non_git", "dirty", "staged", "untracked"].map((kind) => ({
    id: kind,
    kind,
    ...(kind === "shallow" ? { depth: 1 } : {}),
  }));
  const manifest = {
    schema_version: 1,
    suite: "safety",
    commits: [
      { id: "one", date: "2020-01-01T00:00:00Z", message: "one", overlay: { "source.md": "source.md" } },
      { id: "two", date: "2021-01-01T00:00:00Z", message: "two", overlay: { "source.md": "source.md" } },
    ],
    variants,
    queries: [{
      id: "source",
      query: "source",
      category: "canonical",
      variants: ["clean"],
      current: [qrel("source", "source.md", "Source")],
    }],
  };
  const manifestPath = join(directory, "manifest.json");
  try {
    const external = structuredClone(manifest);
    external.commits[0].overlay["source.md"] = externalSource;
    assert.throws(() => validateManifest(external, manifestPath), /escapes the fixture root/);

    const hook = structuredClone(manifest);
    hook.commits[0].overlay = { ".git/hooks/pre-commit": "source.md" };
    assert.throws(() => validateManifest(hook, manifestPath), /safe repository-relative path/);

    const caseFoldedHook = structuredClone(manifest);
    caseFoldedHook.commits[0].delete = [".GIT/config"];
    assert.throws(() => validateManifest(caseFoldedHook, manifestPath), /safe repository-relative path/);

    const variantHook = structuredClone(manifest);
    variantHook.variants[0].overlay = { ".git/config": "source.md" };
    assert.throws(() => validateManifest(variantHook, manifestPath), /safe repository-relative path/);

    const escapedVariant = structuredClone(manifest);
    escapedVariant.variants[0].id = "../../outside";
    assert.throws(() => validateManifest(escapedVariant, manifestPath), /filename-safe slug/);

    const localDate = structuredClone(manifest);
    localDate.commits[0].date = "2020-01-01T00:00:00";
    assert.throws(() => validateManifest(localDate, manifestPath), /explicit timezone/);

    const offsetDate = structuredClone(manifest);
    offsetDate.commits[0].date = "2020-01-01T01:00:00+01:00";
    assert.doesNotThrow(() => validateManifest(offsetDate, manifestPath));

    const invalidDate = structuredClone(manifest);
    invalidDate.commits[0].date = "2020-02-30T00:00:00Z";
    assert.throws(() => validateManifest(invalidDate, manifestPath), /valid RFC3339/);
  } finally {
    rmSync(directory, { recursive: true });
    rmSync(externalDirectory, { recursive: true });
  }
});

test("freshness config projection replaces only the registered search controls", () => {
  const source = `version = 1

[docs.search]
vector = true
freshness = false
max_rank_movement = 3

[embedding]
model = "fixed"
`;
  const projected = configWithFreshness(source, { id: "bound-2", enabled: true, bound: 2 });
  assert.match(projected, /freshness = true/);
  assert.match(projected, /max_rank_movement = 2/);
  assert.equal(projected.match(/freshness\s*=/g).length, 1);
  assert.equal(projected.match(/max_rank_movement\s*=/g).length, 1);
  assert.match(projected, /\[embedding\]\nmodel = "fixed"/);
});

const freshnessHit = ({
  id,
  path,
  rank,
  baseRank,
  movement,
  basis = "git",
  value = "2026-01-01T00:00:00Z",
}) => ({
  rank,
  base_rank: baseRank,
  movement,
  path,
  heading: "Authentication",
  source_bytes: [id * 10, id * 10 + 5],
  file_hash: `hash-${id}`,
  freshness_basis: basis,
  freshness_value: value,
  source_state: "current",
});

const profileDiagnostics = (profile, treatment) => ({
  vector_status: profile === "lexical" ? "disabled" : "active",
  reranker_status: profile === "hybrid-rerank" ? "active" : "disabled",
  freshness_status: treatment === "disabled" ? "disabled" : "active",
  max_rank_movement: treatment === "disabled" ? 1 : Number(treatment.replace("bound-", "")),
  total_candidates: 2,
});

function freshnessRun(profile, treatment, query) {
  const disabled = treatment === "disabled";
  const hits = disabled
    ? [
      freshnessHit({ id: 1, path: "old.md", rank: 1, baseRank: 1, movement: 0, value: "2024-01-01T00:00:00Z" }),
      freshnessHit({ id: 2, path: "current.md", rank: 2, baseRank: 2, movement: 0 }),
    ]
    : [
      freshnessHit({ id: 2, path: "current.md", rank: 1, baseRank: 2, movement: 1 }),
      freshnessHit({ id: 1, path: "old.md", rank: 2, baseRank: 1, movement: -1, value: "2024-01-01T00:00:00Z" }),
    ];
  return {
    variant: "clean",
    profile,
    treatment,
    query,
    diagnostics: profileDiagnostics(profile, treatment),
    repeated_exact_order: true,
    hits,
    score: scoreRun(query, hits),
  };
}

test("Phase 3 scorer reports bounded deltas and selects the smallest passing bound", () => {
  const query = {
    id: "ambiguous-auth",
    query: "Which header?",
    category: "conflict",
    current: [qrel("current", "current.md", "Authentication")],
    older_conflicts: [qrel("old", "old.md", "Authentication")],
  };
  const profiles = ["lexical", "hybrid", "hybrid-rerank"];
  const treatments = ["disabled", "bound-1", "bound-2", "bound-3"];
  const runs = profiles.flatMap((profile) => treatments.map((treatment) => freshnessRun(profile, treatment, query)));
  const comparisons = profiles.flatMap((profile) => [1, 2, 3].map(
    (bound) => compareFreshnessTreatments(runs, profile, `bound-${bound}`),
  ));
  const lexicalBoundOne = comparisons.find(
    (comparison) => comparison.profile === "lexical" && comparison.candidate_treatment === "bound-1",
  );
  assert.equal(lexicalBoundOne.changed_orders, 1);
  assert.equal(lexicalBoundOne.maximum_absolute_movement, 1);
  assert.deepEqual(lexicalBoundOne.movement_histogram, { "-1": 1, "1": 1 });
  assert.equal(lexicalBoundOne.validity.movement_within_bound, true);
  assert.equal(lexicalBoundOne.validity.candidate_bases_match_disabled, true);

  const phase2Report = {
    runs: runs.filter((run) => run.treatment === "disabled").map((run) => ({
      ...run,
      hits: run.hits.map(({ base_rank: _baseRank, movement: _movement, freshness_basis: _basis,
        freshness_value: _value, ...hitValue }) => hitValue),
    })),
  };
  const parity = comparePhase2RankedIdentities(runs, phase2Report);
  assert.equal(parity.exact_ranked_identities, true);
  const selection = selectPhase3Default(runs, comparisons, parity);
  assert.deepEqual(selection.selected_default, { freshness: true, max_rank_movement: 1 });
  assert.ok(selection.candidates.every((candidate) => candidate.passes));
});

test("Phase 3 hard gates expose unknown movement and Git/observed crossings", () => {
  const query = {
    id: "invalid",
    category: "conflict",
    current: [qrel("current", "current.md", "Authentication")],
    older_conflicts: [qrel("old", "old.md", "Authentication")],
  };
  const disabled = freshnessRun("lexical", "disabled", query);
  const candidate = freshnessRun("lexical", "bound-1", query);
  candidate.hits[0].freshness_basis = "observed";
  candidate.hits[1].freshness_basis = "git";
  candidate.hits[1].freshness_value = null;
  const crossing = compareFreshnessTreatments([disabled, candidate], "lexical", "bound-1");
  assert.equal(crossing.validity.git_observed_do_not_cross, false);

  candidate.hits[0].freshness_basis = "unknown";
  const unknown = compareFreshnessTreatments([disabled, candidate], "lexical", "bound-1");
  assert.equal(unknown.validity.unknown_basis_stationary, false);
});

test("Phase 2 report validation pins corpus hashes, validity, and run cardinality", () => {
  const manifest = {
    suite: "phase2",
    queries: [{ id: "q", variants: ["clean"] }],
  };
  const baseRun = (profile) => ({ profile, variant: "clean", query: { id: "q" }, hits: [] });
  const report = {
    schema: "jscout.docs-retrieval-eval.v1",
    schema_version: 1,
    run_kind: "phase2-baseline",
    decision: "phase2-baseline-recorded",
    suite: "phase2",
    inputs: {
      manifest_sha256: "manifest",
      fixture_sha256: "fixture",
      profiles: ["lexical", "fallback", "hybrid", "hybrid-rerank"],
    },
    validity: {
      bm25_fallback_exact_order: true,
      repeated_orders_stable: true,
      phase2_complete: true,
      required_profiles_present: true,
      hybrid_measured: true,
      hybrid_rerank_measured: true,
    },
    runs: ["lexical", "fallback", "hybrid", "hybrid-rerank"].map(baseRun),
  };
  assert.doesNotThrow(() => validatePhase2Report(report, manifest, {
    manifestSha256: "manifest",
    fixtureSha256: "fixture",
  }));
  const wrongCorpus = structuredClone(report);
  wrongCorpus.inputs.fixture_sha256 = "different";
  assert.throws(() => validatePhase2Report(wrongCorpus, manifest, {
    manifestSha256: "manifest",
    fixtureSha256: "fixture",
  }), /corpus fingerprints/);

  assert.deepEqual(
    phase2ValidityForReport(
      true,
      ["lexical", "hybrid", "hybrid-rerank"],
      null,
      report,
    ),
    { bm25_fallback_exact_order: true, phase2_complete: true },
  );
  assert.deepEqual(
    phase2ValidityForReport(
      false,
      ["lexical", "fallback"],
      { exact_order_parity: true },
      null,
    ),
    { bm25_fallback_exact_order: true, phase2_complete: false },
  );
});
