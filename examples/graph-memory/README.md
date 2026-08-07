# Minimal graph memory for agents

This runnable example distills the playbook into five small pieces:

1. Each specialist worker emits schema-shaped entities and relations.
2. An alias map resolves surface forms into canonical nodes.
3. A directed graph stores relations with document and worker provenance.
4. A two-hop breadth-first traversal selects the evidence subgraph.
5. Grounded synthesis and claim checks use only explicit graph edges.

The extraction and alias groups are deterministic fixtures so the demo runs
without an API key:

```bash
npm run demo
npm test
```

The important boundary is `RAW_WORKER_OUTPUTS` in `demo.mjs`. A production
version replaces those fixtures with schema-constrained LLM extraction, then
replaces `ALIAS_GROUPS` with a resolver call that must assign every input alias
to exactly one canonical cluster.

## What the demo deliberately omits

- Live model calls and model-specific SDK code
- Long-document chunking
- Scalable candidate blocking before entity resolution
- Persistent graph storage
- A hand-labeled evaluation set and prompt-tuning loop

Those omissions keep the example inspectable. The graph mechanics, provenance,
two-hop query, and evaluator behavior are fully executable and tested.
