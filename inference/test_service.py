from __future__ import annotations

import math
import os
import unittest
from unittest.mock import patch

import service


class FakeProvider:
    embed_model_id = service.DEFAULT_EMBED_MODEL
    rerank_model_id = service.DEFAULT_RERANK_MODEL

    def embed(self, texts: list[str], deadline_ms: int = 120_000) -> dict:
        return {
            "provider": "local",
            "model": self.embed_model_id,
            "dimensions": 2,
            "configuration": {"normalized": True},
            "vectors": [[float(index), 1.0] for index, _text in enumerate(texts)],
            "deadline_ms": deadline_ms,
        }

    def rerank(self, query: str, candidates: list[dict[str, str]], deadline_ms: int = 120_000) -> dict:
        return {
            "provider": "local",
            "model": self.rerank_model_id,
            "scores": [
                {"id": candidate["id"], "score": float(len(query) + len(candidate["text"]))}
                for candidate in candidates
            ],
            "deadline_ms": deadline_ms,
        }


class RequestTests(unittest.TestCase):
    def setUp(self) -> None:
        self.provider = FakeProvider()

    def test_embed_returns_one_vector_per_input(self) -> None:
        result = service.embed_request(
            {"model": service.DEFAULT_EMBED_MODEL, "texts": ["one", "two"], "deadline_ms": 900},
            self.provider,
        )
        self.assertEqual(result["vectors"], [[0.0, 1.0], [1.0, 1.0]])
        self.assertEqual(result["deadline_ms"], 900)

    def test_embed_rejects_empty_or_wrong_model(self) -> None:
        for body in [
            {"texts": []},
            {"texts": [""]},
            {"model": "wrong", "texts": ["ok"]},
        ]:
            with self.subTest(body=body), self.assertRaises(service.RequestError):
                service.embed_request(body, self.provider)

    def test_rerank_preserves_candidate_ids(self) -> None:
        result = service.rerank_request(
            {
                "query": "find",
                "candidates": [{"id": "a", "text": "alpha"}, {"id": "b", "text": "beta"}],
            },
            self.provider,
        )
        self.assertEqual([score["id"] for score in result["scores"]], ["a", "b"])
        self.assertTrue(all(math.isfinite(score["score"]) for score in result["scores"]))

    def test_rerank_rejects_duplicate_ids_and_invalid_candidates(self) -> None:
        invalid = [
            {"query": "q", "candidates": []},
            {"query": "", "candidates": [{"id": "a", "text": "x"}]},
            {
                "query": "q",
                "candidates": [{"id": "a", "text": "x"}, {"id": "a", "text": "y"}],
            },
        ]
        for body in invalid:
            with self.subTest(body=body), self.assertRaises(service.RequestError):
                service.rerank_request(body, self.provider)

    def test_deadline_is_bounded(self) -> None:
        for deadline in [0, 99, 600_001, True, "1000"]:
            with self.subTest(deadline=deadline), self.assertRaises(service.RequestError):
                service.embed_request({"texts": ["x"], "deadline_ms": deadline}, self.provider)

    def test_port_is_bounded(self) -> None:
        with patch.dict(os.environ, {"JSCOUT_INFERENCE_PORT": "65536"}):
            with self.assertRaises(ValueError):
                service.inference_port()


if __name__ == "__main__":
    unittest.main()
