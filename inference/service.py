"""Optional local embedding and reranking service for jscout.

Rust owns retrieval, storage, and degradation policy. This process owns the
Python ML runtime and exposes one bounded loopback HTTP boundary:

    GET  /health
    GET  /configuration
    POST /embed
    POST /rerank

Models load lazily. BM25-only jscout installs never import PyTorch.
"""

from __future__ import annotations

import json
import math
import os
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any


DEFAULT_EMBED_MODEL = "BAAI/bge-m3"
DEFAULT_RERANK_MODEL = "BAAI/bge-reranker-v2-m3"
# Pin the bundled defaults so a mutable Hugging Face `main` cannot silently
# change vector or reranker semantics under an existing cache fingerprint.
DEFAULT_EMBED_REVISION = "5617a9f61b028005a4858fdac845db406aefb181"
DEFAULT_RERANK_REVISION = "953dc6f6f85a1b2dbfca4c34a2796e7dde08d41e"
MAX_BODY_BYTES = 4 * 1024 * 1024
MAX_EMBED_INPUTS = 128
MAX_RERANK_CANDIDATES = 100
MAX_INPUT_CHARS = 500_000


def _text_env(name: str, fallback: str = "") -> str:
    return os.environ.get(name, "").strip() or fallback


def _positive_int(name: str, fallback: int) -> int:
    raw = _text_env(name)
    if not raw:
        return fallback
    value = int(raw)
    if value <= 0:
        raise ValueError(f"{name} must be positive")
    return value


def inference_host() -> str:
    return _text_env("JSCOUT_INFERENCE_HOST", "127.0.0.1")


def inference_port() -> int:
    port = _positive_int("JSCOUT_INFERENCE_PORT", 8792)
    if port > 65_535:
        raise ValueError("JSCOUT_INFERENCE_PORT must be at most 65535")
    return port


def model_cache_root() -> str:
    configured = _text_env("JSCOUT_MODEL_CACHE_ROOT")
    if configured:
        return str(Path(configured).expanduser().resolve())
    return str((Path.home() / ".cache" / "jscout" / "models").resolve())


os.environ.setdefault("HF_HOME", model_cache_root())


class RequestError(Exception):
    def __init__(self, status: int, code: str):
        super().__init__(code)
        self.status = status
        self.code = code


def _text_list(value: Any, *, maximum: int) -> list[str]:
    if not isinstance(value, list) or not value or len(value) > maximum:
        raise RequestError(400, "invalid_inputs")
    if any(not isinstance(item, str) or not item.strip() for item in value):
        raise RequestError(400, "invalid_inputs")
    if sum(len(item) for item in value) > MAX_INPUT_CHARS:
        raise RequestError(413, "inputs_too_large")
    return value


def _requested_model(body: dict[str, Any], expected: str) -> str:
    model = body.get("model", expected)
    if not isinstance(model, str) or model != expected:
        raise RequestError(400, "unsupported_model")
    return model


def _deadline_ms(body: dict[str, Any]) -> int:
    value = body.get("deadline_ms", 120_000)
    if not isinstance(value, int) or isinstance(value, bool) or value < 100 or value > 600_000:
        raise RequestError(400, "invalid_deadline")
    return value


class LocalBgeProvider:
    """Lazy, serialized PyTorch workers for BGE-M3 and its reranker."""

    def __init__(self) -> None:
        self.embed_model_id = _text_env("JSCOUT_EMBED_MODEL", DEFAULT_EMBED_MODEL)
        if self.embed_model_id != DEFAULT_EMBED_MODEL:
            raise ValueError(
                f"local embedding model must be {DEFAULT_EMBED_MODEL}; "
                "use the OpenAI-compatible provider for other models"
            )
        self.rerank_model_id = _text_env("JSCOUT_RERANK_MODEL", DEFAULT_RERANK_MODEL)
        self.embed_revision = _text_env("JSCOUT_EMBED_REVISION", DEFAULT_EMBED_REVISION)
        self.rerank_revision = _text_env("JSCOUT_RERANK_REVISION", DEFAULT_RERANK_REVISION)
        self.batch_size = _positive_int("JSCOUT_INFERENCE_BATCH_SIZE", 16)
        self.max_length = _positive_int("JSCOUT_INFERENCE_MAX_LENGTH", 4096)
        self._runtime_state: tuple[Any, Any, str, str] | None = None
        self._embed_worker: tuple[Any, Any, Any, str, str] | None = None
        self._rerank_worker: tuple[Any, Any, Any, str, str] | None = None
        self._load_lock = threading.Lock()
        # MPS and model loading are intentionally serialized. A predictable
        # queue is safer than concurrent requests exhausting unified memory.
        self._run_lock = threading.Lock()

    def _runtime(self) -> tuple[Any, Any, str, str]:
        if self._runtime_state is not None:
            return self._runtime_state
        import torch
        import torch.nn.functional as functional

        if torch.backends.mps.is_available():
            device, dtype = "mps", "float16"
        elif torch.cuda.is_available():
            device, dtype = "cuda", "float16"
        else:
            device, dtype = "cpu", "float32"
        self._runtime_state = (torch, functional, device, dtype)
        return self._runtime_state

    @staticmethod
    def _model_kwargs(torch: Any, dtype: str, revision: str | None) -> dict[str, Any]:
        options: dict[str, Any] = {
            "dtype": torch.float16 if dtype == "float16" else torch.float32,
        }
        if revision is not None:
            options["revision"] = revision
        return options

    def _load_embed(self) -> tuple[Any, Any, Any, str, str]:
        with self._load_lock:
            if self._embed_worker is not None:
                return self._embed_worker
            torch, functional, device, dtype = self._runtime()
            from transformers import AutoModel, AutoTokenizer

            tokenizer = AutoTokenizer.from_pretrained(
                self.embed_model_id,
                **({"revision": self.embed_revision} if self.embed_revision else {}),
            )
            model = AutoModel.from_pretrained(
                self.embed_model_id,
                **self._model_kwargs(torch, dtype, self.embed_revision),
            ).to(device)
            model.eval()
            self._embed_worker = (tokenizer, model, functional, device, dtype)
            return self._embed_worker

    def _load_rerank(self) -> tuple[Any, Any, Any, str, str]:
        with self._load_lock:
            if self._rerank_worker is not None:
                return self._rerank_worker
            torch, _functional, device, dtype = self._runtime()
            from transformers import AutoModelForSequenceClassification, AutoTokenizer

            tokenizer = AutoTokenizer.from_pretrained(
                self.rerank_model_id,
                **({"revision": self.rerank_revision} if self.rerank_revision else {}),
            )
            model = AutoModelForSequenceClassification.from_pretrained(
                self.rerank_model_id,
                **self._model_kwargs(torch, dtype, self.rerank_revision),
            ).to(device)
            model.eval()
            self._rerank_worker = (tokenizer, model, torch, device, dtype)
            return self._rerank_worker

    def configuration(self) -> dict[str, Any]:
        _torch, _functional, device, dtype = self._runtime()
        return {
            "available": True,
            "provider": "local",
            "device": device,
            "embedding": {
                "model": self.embed_model_id,
                "dimensions": 1024,
                "revision": self.embed_revision,
                "configuration": {
                    "pooling": "cls",
                    "normalized": True,
                    "max_length": self.max_length,
                    "dtype": dtype,
                },
            },
            "reranker": {
                "model": self.rerank_model_id,
                "revision": self.rerank_revision,
                "configuration": {"max_length": self.max_length, "dtype": dtype},
            },
        }

    def status(self) -> dict[str, Any]:
        status: dict[str, Any] = {
            "provider": "local",
            "embedding": {"model": self.embed_model_id, "loaded": self._embed_worker is not None},
            "reranker": {"model": self.rerank_model_id, "loaded": self._rerank_worker is not None},
        }
        try:
            status["runtime"] = self.configuration()
        except Exception as error:  # Health remains readable without ML dependencies.
            status["runtime"] = {"available": False, "error": str(error)[:300]}
        return status

    def _acquire_run(self, started: float, deadline_ms: int) -> None:
        remaining = deadline_ms / 1000 - (time.monotonic() - started)
        if remaining <= 0 or not self._run_lock.acquire(timeout=remaining):
            raise RuntimeError("inference_deadline_exceeded")

    @staticmethod
    def _check_deadline(started: float, deadline_ms: int) -> None:
        if (time.monotonic() - started) * 1000 >= deadline_ms:
            raise RuntimeError("inference_deadline_exceeded")

    def embed(self, texts: list[str], deadline_ms: int = 120_000) -> dict[str, Any]:
        started = time.monotonic()
        tokenizer, model, functional, device, dtype = self._load_embed()
        vectors: list[list[float]] = []
        self._acquire_run(started, deadline_ms)
        try:
            import torch

            with torch.inference_mode():
                for offset in range(0, len(texts), self.batch_size):
                    self._check_deadline(started, deadline_ms)
                    batch = texts[offset : offset + self.batch_size]
                    encoded = tokenizer(
                        batch,
                        padding=True,
                        truncation=True,
                        max_length=self.max_length,
                        return_tensors="pt",
                    ).to(device)
                    # BGE-M3's dense representation is the normalized CLS token.
                    output = model(**encoded).last_hidden_state[:, 0]
                    output = functional.normalize(output.float(), p=2, dim=1).cpu()
                    self._check_deadline(started, deadline_ms)
                    vectors.extend(output.tolist())
        finally:
            self._run_lock.release()
        if not vectors or any(any(not math.isfinite(value) for value in vector) for vector in vectors):
            raise RuntimeError("embedding_model_returned_non_finite_values")
        return {
            "provider": "local",
            "model": self.embed_model_id,
            "revision": self.embed_revision,
            "device": device,
            "dtype": dtype,
            "dimensions": len(vectors[0]),
            "configuration": self.configuration()["embedding"]["configuration"],
            "vectors": vectors,
            "usage": {"inputs": len(texts), "characters": sum(map(len, texts))},
        }

    def rerank(
        self, query: str, candidates: list[dict[str, str]], deadline_ms: int = 120_000
    ) -> dict[str, Any]:
        started = time.monotonic()
        tokenizer, model, torch, device, dtype = self._load_rerank()
        scores: list[dict[str, Any]] = []
        self._acquire_run(started, deadline_ms)
        try:
            with torch.inference_mode():
                for offset in range(0, len(candidates), self.batch_size):
                    self._check_deadline(started, deadline_ms)
                    batch = candidates[offset : offset + self.batch_size]
                    pairs = [[query, candidate["text"]] for candidate in batch]
                    encoded = tokenizer(
                        pairs,
                        padding=True,
                        truncation=True,
                        max_length=self.max_length,
                        return_tensors="pt",
                    ).to(device)
                    logits = model(**encoded, return_dict=True).logits.view(-1).float().cpu().tolist()
                    self._check_deadline(started, deadline_ms)
                    for candidate, score in zip(batch, logits, strict=True):
                        if not math.isfinite(score):
                            raise RuntimeError("reranker_returned_non_finite_score")
                        scores.append({"id": candidate["id"], "score": score})
        finally:
            self._run_lock.release()
        return {
            "provider": "local",
            "model": self.rerank_model_id,
            "revision": self.rerank_revision,
            "device": device,
            "dtype": dtype,
            "scores": scores,
            "usage": {
                "inputs": len(candidates),
                "characters": len(query) * len(candidates)
                + sum(len(candidate["text"]) for candidate in candidates),
            },
        }


PROVIDER = LocalBgeProvider()


def embed_request(body: Any, provider: LocalBgeProvider = PROVIDER) -> dict[str, Any]:
    if not isinstance(body, dict):
        raise RequestError(400, "invalid_json")
    _requested_model(body, provider.embed_model_id)
    return provider.embed(
        _text_list(body.get("texts"), maximum=MAX_EMBED_INPUTS),
        deadline_ms=_deadline_ms(body),
    )


def rerank_request(body: Any, provider: LocalBgeProvider = PROVIDER) -> dict[str, Any]:
    if not isinstance(body, dict):
        raise RequestError(400, "invalid_json")
    _requested_model(body, provider.rerank_model_id)
    query = body.get("query")
    raw_candidates = body.get("candidates")
    if not isinstance(query, str) or not query.strip():
        raise RequestError(400, "invalid_query")
    if (
        not isinstance(raw_candidates, list)
        or not raw_candidates
        or len(raw_candidates) > MAX_RERANK_CANDIDATES
    ):
        raise RequestError(400, "invalid_candidates")
    candidates: list[dict[str, str]] = []
    for candidate in raw_candidates:
        if not isinstance(candidate, dict):
            raise RequestError(400, "invalid_candidates")
        identifier, text = candidate.get("id"), candidate.get("text")
        if (
            not isinstance(identifier, str)
            or not identifier
            or not isinstance(text, str)
            or not text.strip()
        ):
            raise RequestError(400, "invalid_candidates")
        candidates.append({"id": identifier, "text": text})
    if len({candidate["id"] for candidate in candidates}) != len(candidates):
        raise RequestError(400, "duplicate_candidate")
    if len(query) * len(candidates) + sum(len(item["text"]) for item in candidates) > MAX_INPUT_CHARS:
        raise RequestError(413, "inputs_too_large")
    return provider.rerank(query, candidates, deadline_ms=_deadline_ms(body))


class Handler(BaseHTTPRequestHandler):
    server_version = "jscout-inference/0.1.0"
    provider = PROVIDER

    def do_GET(self) -> None:  # noqa: N802
        if self.path == "/health":
            self._json(200, {"status": "ok", "service": "jscout-inference", **self.provider.status()})
        elif self.path == "/configuration":
            try:
                self._json(200, self.provider.configuration())
            except Exception as error:
                self._json(503, {"error": "inference_unavailable", "detail": str(error)[:500]})
        else:
            self._json(404, {"error": "not_found", "path": self.path})

    def do_POST(self) -> None:  # noqa: N802
        try:
            body = self._read_json()
            if self.path == "/embed":
                self._json(200, embed_request(body, self.provider))
            elif self.path == "/rerank":
                self._json(200, rerank_request(body, self.provider))
            else:
                self._json(404, {"error": "not_found", "path": self.path})
        except RequestError as error:
            self._json(error.status, {"error": error.code})
        except (BrokenPipeError, ConnectionResetError):
            return
        except Exception as error:
            self._json(503, {"error": "inference_unavailable", "detail": str(error)[:500]})

    def _read_json(self) -> Any:
        try:
            length = int(self.headers.get("Content-Length", ""))
        except ValueError as error:
            raise RequestError(400, "invalid_content_length") from error
        if length <= 0:
            raise RequestError(400, "empty_body")
        if length > MAX_BODY_BYTES:
            raise RequestError(413, "payload_too_large")
        try:
            return json.loads(self.rfile.read(length))
        except (json.JSONDecodeError, UnicodeDecodeError) as error:
            raise RequestError(400, "invalid_json") from error

    def _json(self, status: int, body: dict[str, Any]) -> None:
        payload = json.dumps(body, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(payload)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, fmt: str, *args: object) -> None:
        print(f"inference: {fmt % args}")


class InferenceServer(ThreadingHTTPServer):
    daemon_threads = True


def main() -> None:
    port = inference_port()
    host = inference_host()
    if host not in {"127.0.0.1", "localhost", "::1"} and _text_env(
        "JSCOUT_INFERENCE_ALLOW_REMOTE"
    ).lower() not in {"1", "true", "yes"}:
        raise RuntimeError(
            "refusing a non-loopback inference bind; set "
            "JSCOUT_INFERENCE_ALLOW_REMOTE=1 only on a trusted network"
        )
    server = InferenceServer((host, port), Handler)
    print(f"jscout inference listening on http://{host}:{port}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("jscout inference stopped", flush=True)
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
