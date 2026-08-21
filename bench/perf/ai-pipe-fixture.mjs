export const AI_PIPE_REVISION = 'ea13166c59cfc52574e96959413f5c54be20e8c8';

export const SEARCH_QUERIES = Object.freeze([
  'decide whether an order should be blocked because the account is taking on too much exposure',
  'is the stock exchange open right now given the current time in new york',
  'split daily returns into the overnight gap versus regular trading hours',
  'clean up runs that were left half finished after the process crashed',
  'decrypt stored credentials and make them available to executing jobs',
  'should this failed delivery be attempted again or is the error permanent',
  'get the ticker symbols that belong to a saved list',
  'cap how many option strikes near the current price we keep',
  "build the link to a document in the regulator's filing archive",
  'which status changes are allowed for a marketing campaign',
  'queue social media posting events so workflows can process them',
  'give up on deliveries that have been stuck in flight for too long',
  'how long to wait before a failed event delivery is queued again',
  'ask a model to act as judge and compare answers from different models',
  'page that shows the details of a single executed trade',
  'hook managing the state of the visual workflow graph editor',
  'which tickers currently have positions that are not closed yet',
  'check a proposed trade against its contract before accepting it',
  'classify the market regime before the opening bell',
  'populate a fresh database with example trading workflows',
  'write detected chart patterns into a persistent journal',
  'fetch daily index bars preferring the interactive brokers feed',
  'http endpoints that receive callbacks from external services',
  'normalize price quote rows coming from different data vendors',
]);

export const SEARCH_LIMIT = 20;

export const NEIGHBORHOOD_ANCHORS = Object.freeze({
  low: Object.freeze({
    id: 'sym:src/RunNodeInspectorModal.tsx#::inspectorTabs@1',
    degree: 1,
  }),
  medium: Object.freeze({
    id: 'sym:tradebook/api/src/domain/lifecycle-timeline.ts#::compareEvents@1',
    degree: 4,
  }),
  high: Object.freeze({
    id: 'sym:server/db.mjs#::openDatabase@1',
    degree: 476,
  }),
  extreme: Object.freeze({
    id: 'pkg:node:assert',
    degree: 7_877,
  }),
});

const neighborhoodCase = (id, anchor, nodeLimit, edgeLimit, responseBytes, expected) =>
  Object.freeze({
    id,
    anchor: NEIGHBORHOOD_ANCHORS[anchor].id,
    anchor_class: anchor,
    depth: 1,
    direction: 'both',
    node_limit: nodeLimit,
    edge_limit: edgeLimit,
    min_confidence: 'likely',
    origins: Object.freeze(['repository', 'workspace']),
    response_bytes: responseBytes,
    debug: false,
    expected: Object.freeze(expected),
  });

export const NEIGHBORHOOD_CASES = Object.freeze([
  neighborhoodCase('anchor-low-default', 'low', 50, 200, 24_000,
    { nodes: 2, edges: 1, omitted_nodes: 0, omitted_edges: 0, truncated: false }),
  neighborhoodCase('anchor-medium-default', 'medium', 50, 200, 24_000,
    { nodes: 3, edges: 4, omitted_nodes: 0, omitted_edges: 0, truncated: false }),
  neighborhoodCase('anchor-high-default', 'high', 50, 200, 24_000,
    { nodes: 50, edges: 49, omitted_nodes: 0, omitted_edges: 0, truncated: true }),
  neighborhoodCase('anchor-extreme-default', 'extreme', 50, 200, 24_000,
    { nodes: 43, edges: 192, omitted_nodes: 0, omitted_edges: 8, truncated: true }),
  neighborhoodCase('budget-high-2k', 'high', 500, 1_000, 2_000,
    { nodes: 8, edges: 7, omitted_nodes: 140, omitted_edges: 469, truncated: true }),
  neighborhoodCase('budget-high-24k', 'high', 500, 1_000, 24_000,
    { nodes: 90, edges: 126, omitted_nodes: 58, omitted_edges: 350, truncated: true }),
  neighborhoodCase('budget-high-100k', 'high', 500, 1_000, 100_000,
    { nodes: 148, edges: 476, omitted_nodes: 0, omitted_edges: 0, truncated: false }),
]);

export const WATCH_TOUCH_PATH = 'server/db.mjs';
export const SCOUT_WORKFLOW_SEED = 'evaluateBrokerRiskPolicy';
export const SCOUT_CARD_ANCHOR =
  'sym:server/brokers/riskPolicy.mjs#::evaluateBrokerRiskPolicy@1';

export const SCOUT_PLAN_INVARIANTS = Object.freeze({
  workflows_auto: Object.freeze({
    calls_planned: 16,
    plan_items: 17,
    skipped_items: 1,
    over_context_bytes_items: 1,
  }),
  cards_auto: Object.freeze({
    calls_planned: 512,
    plan_items: 1_024,
    skipped_items: 0,
    over_context_bytes_items: 0,
  }),
  workflow_explicit: Object.freeze({
    calls_planned: 1,
    plan_items: 1,
    skipped_items: 0,
    over_context_bytes_items: 0,
  }),
  card_explicit: Object.freeze({
    calls_planned: 1,
    plan_items: 1,
    skipped_items: 0,
    over_context_bytes_items: 0,
  }),
});

export const EMBEDDING_FIXTURE = Object.freeze({
  model: 'jscout-bench-embed',
  revision: 'dense-unit-vector-v1',
  dimensions: 1_024,
  batch: 64,
  unique_embeddings: 5_065,
  occurrence_entries: 5_483,
  embed_requests: 317,
  configuration_requests: 1,
  sync_markers: 1,
});

export const ENRICHMENT_INVARIANTS = Object.freeze({
  projects: 458,
  occurrences_selected: 5_158,
  request_batches: 466,
  facts_published: 1_412,
  unknown_answers: 1_689,
  occurrences_resumed: 5_158,
});

// These values identify fixture drift. They are properties of the pinned
// ai-pipe revision, not performance thresholds.
export const CORPUS_INVARIANTS = Object.freeze({
  tracked_files: 930,
  indexed_files: 690,
  rejected_files: 0,
  chunks: 5_483,
  symbols: 4_538,
  references: 28_527,
  member_calls: 25_340,
  graph_nodes: 5_677,
  graph_edges: 37_202,
  search_queries: SEARCH_QUERIES.length,
});
