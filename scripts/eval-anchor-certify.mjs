#!/usr/bin/env node

// Anchor certification for eval tasks.
//
// The post-cutoff mining pipeline selects tasks around newly introduced
// symbols, which guarantees a greppable lexical handle: grep the prompt,
// hit the gold files. A suite built that way cannot measure what structural
// retrieval adds beyond grep. This tool classifies each task by whether its
// prompt leaks lexical anchors into the gold files:
//
// - `anchored`:    an identifier-like prompt token (camelCase, snake_case,
//                  dotted/path-like, or quoted) appears in a gold file.
// - `weak`:        only plain prose words from the prompt appear in gold.
// - `anchor-free`: no prompt content-word appears in any gold file.
//
// An anchor-free suite is the discriminating instrument for graph retrieval;
// `--require anchor-free` gates admission in CI.

import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

const STOPWORDS = new Set([
  "about", "after", "again", "against", "all", "and", "answer", "any", "are",
  "back", "because", "been", "before", "being", "between", "both", "but",
  "call", "called", "calls", "can", "class", "classes", "code", "codebase",
  "concrete", "could", "define", "defined", "defines", "direct", "directly",
  "does", "each", "entry", "exact", "explain", "file", "files", "final",
  "find", "first", "from", "function", "functions", "give", "gold", "handler",
  "handlers", "has", "have", "how", "implement", "implementation",
  "implementations", "implemented", "into", "its", "list", "locate", "method",
  "methods", "module", "modules", "must", "name", "named", "names", "not",
  "only", "other", "path", "paths", "point", "process", "production",
  "provide", "repository", "return", "returns", "runtime", "should", "source",
  "starting", "such", "symbol", "symbols", "than", "that", "the", "their",
  "them", "then", "there", "these", "they", "this", "those", "through",
  "trace", "used", "uses", "using", "什么", "were", "what", "when", "where",
  "which", "while", "with", "would", "your",
]);

function isIdentifierLike(token) {
  if (/[_$./:]/.test(token)) return true; // snake_case, dotted, path-like
  if (/^[a-z]+[A-Z]/.test(token)) return true; // camelCase
  if (/^[A-Z][a-z]+[A-Z]/.test(token)) return true; // PascalCase
  if (/[a-zA-Z]\d|\d[a-zA-Z]/.test(token)) return true; // mixed alnum
  return false;
}

export function extractAnchors(prompt) {
  const quoted = [...prompt.matchAll(/[`'"]([^`'"]{3,80})[`'"]/g)].map((m) => m[1]);
  const tokens = prompt.split(/[^\w$./:-]+/).filter((t) => t.length >= 4);
  const identifiers = new Set(quoted.filter((q) => !q.includes(" ")));
  const words = new Set();
  for (const token of tokens) {
    if (isIdentifierLike(token)) {
      identifiers.add(token);
    } else if (token.length >= 5 && !STOPWORDS.has(token.toLowerCase())) {
      words.add(token.toLowerCase());
    }
  }
  return { identifiers: [...identifiers], words: [...words] };
}

function wholeTokenRegex(anchor) {
  const escaped = anchor.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`(^|[^\\w$])${escaped}([^\\w$]|$)`, "i");
}

export function certifyTask(prompt, goldContents) {
  const { identifiers, words } = extractAnchors(prompt);
  const identifierHits = [];
  const wordHits = [];
  for (const [file, content] of goldContents) {
    for (const anchor of identifiers) {
      if (wholeTokenRegex(anchor).test(content)) {
        identifierHits.push({ anchor, file });
      }
    }
    for (const anchor of words) {
      if (wholeTokenRegex(anchor).test(content)) {
        wordHits.push({ anchor, file });
      }
    }
  }
  const status =
    identifierHits.length > 0 ? "anchored" : wordHits.length > 0 ? "weak" : "anchor-free";
  return { status, identifierHits, wordHits };
}

function parseArgs(argv) {
  const options = {};
  for (let index = 0; index < argv.length; index += 2) {
    const flag = argv[index];
    const value = argv[index + 1];
    if (!flag?.startsWith("--") || value === undefined) {
      throw new Error("arguments must be --name value pairs");
    }
    options[flag.slice(2)] = value;
  }
  for (const required of ["tasks", "repository"]) {
    if (!options[required]) throw new Error(`--${required} is required`);
  }
  return options;
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  const repository = path.resolve(options.repository);
  const taskSets = options.tasks
    .split(",")
    .map((file) => JSON.parse(fs.readFileSync(path.resolve(file.trim()), "utf8")));

  const results = [];
  for (const taskSet of taskSets) {
    for (const task of taskSet.tasks) {
      const goldContents = [];
      for (const file of task.gold?.files ?? []) {
        const absolute = path.join(repository, file);
        if (!fs.existsSync(absolute)) {
          throw new Error(`task ${task.id}: gold file missing from repository: ${file}`);
        }
        goldContents.push([file, fs.readFileSync(absolute, "utf8")]);
      }
      const certificate = certifyTask(task.prompt, goldContents);
      results.push({
        task_id: task.id,
        status: certificate.status,
        identifier_anchors: certificate.identifierHits,
        word_anchors: certificate.wordHits.slice(0, 20),
      });
    }
  }

  const counts = {};
  for (const result of results) {
    counts[result.status] = (counts[result.status] ?? 0) + 1;
  }
  process.stdout.write(`${JSON.stringify({ counts, results }, null, 2)}\n`);

  if (options.require) {
    const failing = results.filter((result) => result.status !== options.require);
    if (failing.length > 0) {
      process.stderr.write(
        `${failing.length} task(s) are not ${options.require}: ${failing
          .map((result) => `${result.task_id} (${result.status})`)
          .join(", ")}\n`,
      );
      process.exitCode = 1;
    }
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main();
}
