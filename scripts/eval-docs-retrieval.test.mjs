import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";

import {
  compareProfiles,
  docsSearchArguments,
  materializeRepositories,
  parseArguments,
  scoreRun,
  summarizeProfile,
  validateManifest,
  validatePhase2ProviderConfiguration,
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

    const escapedVariant = structuredClone(manifest);
    escapedVariant.variants[0].id = "../../outside";
    assert.throws(() => validateManifest(escapedVariant, manifestPath), /filename-safe slug/);

    const localDate = structuredClone(manifest);
    localDate.commits[0].date = "2020-01-01T00:00:00";
    assert.throws(() => validateManifest(localDate, manifestPath), /YYYY-MM-DDTHH:mm:ssZ/);
  } finally {
    rmSync(directory, { recursive: true });
    rmSync(externalDirectory, { recursive: true });
  }
});
