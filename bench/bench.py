"""Benchmark v2: warms each model, validates the vector stage actually fired,
and reports per-stage numbers. One model exercised at a time."""
import json
import re
import statistics
import subprocess
import sys
import time
import urllib.request

JSRAG = "/Users/cristian/git/js-rag/target/release/js-rag"
REPO = "/Users/cristian/git/bvb"
LMS = "http://localhost:1234/v1/embeddings"
RERANK = "http://127.0.0.1:8792/rerank"

MODELS = [
    "text-embedding-bge-m3",
    "text-embedding-qwen3-embedding-4b",
    "text-embedding-nomic-embed-code",
]

EVALS = [
    ("where are api credentials persisted on disk", r"JsonCredentialStore"),
    ("stream events to the client over http as they arrive", r"startSse|parseSseBlock|handleStreamEvent"),
    ("retry agent startup after transient network errors", r"startupRetry|StartupRetry"),
    ("compute text embeddings with a local child process", r"LocalEmbeddingClient|embeddings"),
    ("block sql write statements in metrics queries", r"queryMetrics|FORBIDDEN"),
    ("send a json http response with a status code", r"sendJson"),
    ("build the system prompt for the agent", r"systemPrompt|createSystemPrompt"),
    ("terminate child processes when the parent gets a signal", r"dev\.mjs|stop"),
]


def run(cmd, env=None, timeout=3600):
    import os
    full_env = dict(os.environ)
    if env:
        full_env.update(env)
    t0 = time.time()
    r = subprocess.run(cmd, capture_output=True, text=True, env=full_env, timeout=timeout)
    return r, time.time() - t0


def raw_embed(model, text, timeout=180):
    body = json.dumps({"model": model, "input": [text]}).encode()
    req = urllib.request.Request(LMS, data=body, headers={"Content-Type": "application/json"})
    t0 = time.time()
    urllib.request.urlopen(req, timeout=timeout).read()
    return time.time() - t0


def warm_up(model, budget=180):
    """Force-load the model; returns load time or None."""
    t0 = time.time()
    while time.time() - t0 < budget:
        try:
            raw_embed(model, "warm up")
            return time.time() - t0
        except Exception as e:
            print(f"  warm-up retry ({type(e).__name__})", flush=True)
            time.sleep(5)
    return None


def eval_model(model, use_vector=True, use_rerank=False):
    env = {}
    label = "bm25-only"
    if use_vector:
        env = {"JSRAG_EMBED_URL": LMS, "JSRAG_EMBED_MODEL": model}
        label = model.replace("text-embedding-", "")
    if use_rerank:
        env["JSRAG_RERANK_URL"] = RERANK
        label += "+rerank"
    ranks, times = [], []
    vector_failures = 0
    rerank_failures = 0
    for query, pattern in EVALS:
        cmd = [JSRAG, "search", REPO, query, "-k", "20", "--json"]
        if not use_vector:
            cmd.append("--no-vector")
        r, dt = run(cmd, env=env)
        times.append(dt)
        if "vector search unavailable" in r.stderr:
            vector_failures += 1
        if "rerank unavailable" in r.stderr:
            rerank_failures += 1
        try:
            hits = json.loads(r.stdout)
        except json.JSONDecodeError:
            ranks.append(None)
            continue
        rank = None
        for i, h in enumerate(hits):
            hay = f"{h['file']}:{h.get('name') or ''} {' '.join(h.get('used_by', []))}"
            if re.search(pattern, hay):
                rank = i + 1
                break
        ranks.append(rank)
    hit1 = sum(1 for r in ranks if r == 1)
    hit5 = sum(1 for r in ranks if r and r <= 5)
    mrr = sum(1.0 / r for r in ranks if r) / len(ranks)
    res = {
        "label": label,
        "hit@1": f"{hit1}/{len(ranks)}",
        "hit@5": f"{hit5}/{len(ranks)}",
        "mrr": round(mrr, 3),
        "search_ms_median": int(statistics.median(times) * 1000),
        "ranks": ranks,
    }
    if use_vector and vector_failures:
        res["INVALID_vector_failures"] = vector_failures
    if use_rerank and rerank_failures:
        res["INVALID_rerank_failures"] = rerank_failures
    return res


def ensure_embedded(model):
    q = f"SELECT COUNT(*) FROM embeddings WHERE model='{model}'"
    existing = int(subprocess.run(["sqlite3", f"{REPO}/.jsrag.db", q],
                                  capture_output=True, text=True).stdout.strip() or 0)
    if existing > 0:
        print(f"  embeddings cached: {existing} vectors", flush=True)
        return "cached"
    for attempt in range(2):
        r, dt = run([JSRAG, "embed", REPO, "--batch", "16"],
                    env={"JSRAG_EMBED_URL": LMS, "JSRAG_EMBED_MODEL": model})
        m = re.search(r"embedded (\d+)/\d+ chunks", r.stdout)
        n = int(m.group(1)) if m else 0
        if r.returncode == 0 and n > 0:
            tput = round(n / dt, 2)
            print(f"  embed: {n} chunks in {int(dt)}s = {tput} chunks/s", flush=True)
            return tput
        print(f"  embed attempt {attempt+1} failed (rc={r.returncode}): {r.stderr.strip()[-300:]}", flush=True)
        time.sleep(10)
    return "FAILED"


def main():
    results = []
    print("== baseline: BM25 only ==", flush=True)
    results.append(eval_model(None, use_vector=False))
    print(results[-1], flush=True)

    for model in MODELS:
        short = model.replace("text-embedding-", "")
        print(f"\n== {short} ==", flush=True)
        load = warm_up(model)
        if load is None:
            print("  COULD NOT LOAD MODEL — skipping", flush=True)
            continue
        print(f"  model ready in {load:.1f}s", flush=True)
        tput = ensure_embedded(model)
        if tput == "FAILED":
            continue
        qlat = statistics.median(raw_embed(model, "latency probe") for _ in range(3))
        print(f"  query embed latency: {int(qlat*1000)}ms", flush=True)
        for use_rerank in (False, True):
            res = eval_model(model, use_rerank=use_rerank)
            res["embed_chunks_per_s"] = tput
            res["query_embed_ms"] = int(qlat * 1000)
            results.append(res)
            print(res, flush=True)

    print("\n===== SUMMARY =====")
    for r in results:
        print(json.dumps(r))


if __name__ == "__main__":
    main()
