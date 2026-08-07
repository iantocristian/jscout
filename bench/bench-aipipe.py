"""Hard eval on ai-pipe: 24 paraphrased queries, identifier words avoided,
so semantic retrieval has to do real work. Same harness as bench.py otherwise."""
import json
import os
import re
import statistics
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

JSCOUT = os.environ.get(
    "JSCOUT_BIN", str(Path(__file__).resolve().parents[1] / "target/release/jscout")
)
REPO = os.environ.get("JSCOUT_BENCH_REPO", "/Users/cristian/git/ai-pipe")
LMS = "http://localhost:1234/v1/embeddings"
RERANK = "http://127.0.0.1:8792/rerank"

MODELS = [
    "text-embedding-bge-m3",
    "text-embedding-qwen3-embedding-4b",
    "text-embedding-nomic-embed-code",
]
# Optional filter: bench-aipipe.py <substring> runs only matching models
# (and skips the BM25 baseline).
ONLY = sys.argv[1] if len(sys.argv) > 1 else None
if ONLY:
    MODELS = [m for m in MODELS if ONLY in m]

EVALS = [
    ("decide whether an order should be blocked because the account is taking on too much exposure", r"evaluateBrokerRiskPolicy|riskPolicy"),
    ("is the stock exchange open right now given the current time in new york", r"marketSessionAt|marketSession"),
    ("split daily returns into the overnight gap versus regular trading hours", r"overnightIntradaySplit"),
    ("clean up runs that were left half finished after the process crashed", r"reconcileOrphanedRuns"),
    ("decrypt stored credentials and make them available to executing jobs", r"runtimeEnvWithSecrets|secrets"),
    ("should this failed delivery be attempted again or is the error permanent", r"isTransientDeliveryFailure"),
    ("get the ticker symbols that belong to a saved list", r"resolveWatchlistSymbols"),
    ("cap how many option strikes near the current price we keep", r"filterOptionRowsByStrikeLimit"),
    ("build the link to a document in the regulator's filing archive", r"buildArchiveDocumentUrl|edgarClient"),
    ("which status changes are allowed for a marketing campaign", r"CAMPAIGN_TRANSITIONS|campaigns"),
    ("queue social media posting events so workflows can process them", r"enqueueXPostEventDeliveries"),
    ("give up on deliveries that have been stuck in flight for too long", r"markStaleWorkflowEventDeliveriesFailed"),
    ("how long to wait before a failed event delivery is queued again", r"REQUEUE_BACKOFF|markWorkflowEventDeliveryRequeued"),
    ("ask a model to act as judge and compare answers from different models", r"messagesForJudgeComparison|judgeAiResponses"),
    ("page that shows the details of a single executed trade", r"TradePage"),
    ("hook managing the state of the visual workflow graph editor", r"useFlowGraphState"),
    ("which tickers currently have positions that are not closed yet", r"openTradeTickers"),
    ("check a proposed trade against its contract before accepting it", r"validateTradeDecisionCandidate|tradeDecisionValidation"),
    ("classify the market regime before the opening bell", r"preopenRegime|executePreopenRegime"),
    ("populate a fresh database with example trading workflows", r"tradingDemoFlows|seedDefaults"),
    ("write detected chart patterns into a persistent journal", r"journalPatternEvents|patternJournal"),
    ("fetch daily index bars preferring the interactive brokers feed", r"fetchIbkrIndexDailyBars|ibkrPreferred"),
    ("http endpoints that receive callbacks from external services", r"handleWebhookRoutes|webhooks"),
    ("normalize price quote rows coming from different data vendors", r"normalizeQuoteRow|normalizeQuotes"),
]


def run(cmd, env=None, timeout=7200):
    import os
    full_env = dict(os.environ)
    if env:
        full_env.update(env)
    t0 = time.time()
    r = subprocess.run(cmd, capture_output=True, text=True, env=full_env, timeout=timeout)
    return r, time.time() - t0


def raw_embed(model, text, timeout=300):
    body = json.dumps({"model": model, "input": [text]}).encode()
    req = urllib.request.Request(LMS, data=body, headers={"Content-Type": "application/json"})
    t0 = time.time()
    urllib.request.urlopen(req, timeout=timeout).read()
    return time.time() - t0


def warm_up(model, budget=300):
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
        env = {"JSCOUT_EMBED_URL": LMS, "JSCOUT_EMBED_MODEL": model}
        label = model.replace("text-embedding-", "")
    if use_rerank:
        env["JSCOUT_RERANK_URL"] = RERANK
        label += "+rerank"
    ranks, times = [], []
    vector_failures = 0
    for query, pattern in EVALS:
        cmd = [JSCOUT, "search", REPO, query, "-k", "20", "--json"]
        if not use_vector:
            cmd.append("--no-vector")
        r, dt = run(cmd, env=env)
        times.append(dt)
        if "vector search unavailable" in r.stderr:
            vector_failures += 1
        try:
            payload = json.loads(r.stdout)
            hits = payload["hits"] if isinstance(payload, dict) else payload
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
    n = len(ranks)
    res = {
        "label": label,
        "hit@1": f"{sum(1 for r in ranks if r == 1)}/{n}",
        "hit@5": f"{sum(1 for r in ranks if r and r <= 5)}/{n}",
        "hit@20": f"{sum(1 for r in ranks if r)}/{n}",
        "mrr": round(sum(1.0 / r for r in ranks if r) / n, 3),
        "search_ms_median": int(statistics.median(times) * 1000),
        "ranks": ranks,
    }
    if use_vector and vector_failures:
        res["INVALID_vector_failures"] = vector_failures
    return res


def ensure_embedded(model):
    q = f"SELECT COUNT(*) FROM embeddings WHERE model='{model}'"
    existing = int(subprocess.run(["sqlite3", f"{REPO}/.jscout.db", q],
                                  capture_output=True, text=True).stdout.strip() or 0)
    total = int(subprocess.run(["sqlite3", f"{REPO}/.jscout.db", "SELECT COUNT(DISTINCT hash) FROM chunks"],
                               capture_output=True, text=True).stdout.strip() or 0)
    if existing >= total:
        print(f"  embeddings cached: {existing}/{total} vectors", flush=True)
        return "cached"
    for attempt in range(2):
        r, dt = run([JSCOUT, "embed", REPO, "--batch", "32"],
                    env={"JSCOUT_EMBED_URL": LMS, "JSCOUT_EMBED_MODEL": model})
        m = re.search(r"embedded (\d+)/\d+ chunks", r.stdout)
        n = int(m.group(1)) if m else 0
        if r.returncode == 0 and n > 0:
            tput = round(n / dt, 2)
            print(f"  embed: {n} chunks in {int(dt)}s = {tput} chunks/s", flush=True)
            return tput
        print(f"  embed attempt {attempt+1} failed (rc={r.returncode}): {r.stderr.strip()[-300:]}", flush=True)
        time.sleep(15)
    return "FAILED"


def main():
    results = []
    if not ONLY:
        print("== baseline: BM25 only ==", flush=True)
        results.append(eval_model(None, use_vector=False))
        print(results[-1], flush=True)

    for model in MODELS:
        short = model.replace("text-embedding-", "")
        print(f"\n== {short} ==", flush=True)
        if warm_up(model) is None:
            print("  COULD NOT LOAD MODEL — skipping", flush=True)
            continue
        tput = ensure_embedded(model)
        if tput == "FAILED":
            continue
        qlat = statistics.median(raw_embed(model, "latency probe") for _ in range(3))
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
