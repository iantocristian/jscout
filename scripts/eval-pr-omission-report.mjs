#!/usr/bin/env node

// Summarize blind PR-replay omission adjudications.
//
// Reports, per reference (gold) file, how often an arm's failure to touch it
// was judged a real defect versus covered by the arm's own design, and lists
// any arm judged a genuine alternative implementation. Confirmed-omission
// counts and alternative verdicts are reported separately from the arm's
// oracle result; they never combine into one score.

import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

import { jsonLines } from "./eval-contamination-probe.mjs";

export function summarize(rows) {
  const byFile = new Map();
  for (const row of rows) {
    for (const entry of row.gold_files) {
      if (!byFile.has(entry.gold_file)) {
        byFile.set(entry.gold_file, {
          gold_file: entry.gold_file,
          pending_in_arms: 0,
          omission: 0,
          alternative_covered: 0,
          not_required: 0,
          ever_legitimately_omitted: false,
        });
      }
      const stats = byFile.get(entry.gold_file);
      stats.pending_in_arms += 1;
      stats[entry.verdict] += 1;
      if (entry.verdict !== "omission") stats.ever_legitimately_omitted = true;
    }
  }
  const overall = { genuine_alternative: 0, partial_alternative: 0, not_alternative: 0 };
  for (const row of rows) overall[row.overall_verdict] += 1;
  return {
    arms: rows.length,
    files: [...byFile.values()].sort((left, right) => left.gold_file.localeCompare(right.gold_file)),
    overall,
    genuine_alternatives: rows
      .filter((row) => row.overall_verdict === "genuine_alternative")
      .map((row) => ({
        session: row.session,
        trial: row.trial,
        profile: row.profile,
        treatment: row.treatment,
        reason: row.overall_reason,
      })),
  };
}

export function renderMarkdown(summary, rows) {
  const short = (file) => file.replace("packages/next/src/client/", "");
  const lines = [
    `# Blind omission adjudication (${summary.arms} arms)`,
    "",
    "Verdicts are per arm, for reference code files that arm did not patch.",
    "",
    "| Reference file | Pending in arms | Confirmed omission | Alternative covered | Not required | Ever legitimately omitted |",
    "|---|---:|---:|---:|---:|---|",
    ...summary.files.map((file) => [
      "",
      `\`${short(file.gold_file)}\``,
      file.pending_in_arms,
      file.omission,
      file.alternative_covered,
      file.not_required,
      file.ever_legitimately_omitted ? "yes" : "no",
      "",
    ].join(" | ").trim()),
    "",
    "## Overall arm verdicts",
    "",
    "| Verdict | Arms |",
    "|---|---:|",
    ...Object.entries(summary.overall).map(([verdict, count]) => `| ${verdict} | ${count} |`),
    "",
  ];
  if (summary.genuine_alternatives.length > 0) {
    lines.push("## Arms judged genuine alternative implementations", "");
    for (const arm of summary.genuine_alternatives) {
      lines.push(`- \`${arm.session}\` (${arm.profile}/${arm.treatment}, ${arm.trial}): ${arm.reason}`);
    }
    lines.push("");
  } else {
    lines.push("No arm was judged a genuine alternative implementation.", "");
  }
  lines.push("## Per-arm confirmed omissions", "", "| Arm | Trial | Profile | Treatment | Omission | Alt covered | Not required | Overall |", "|---|---|---|---|---:|---:|---:|---|");
  for (const row of [...rows].sort((left, right) => left.session.localeCompare(right.session))) {
    const count = (verdict) => row.gold_files.filter((entry) => entry.verdict === verdict).length;
    lines.push(`| \`${row.session}\` | ${row.trial} | ${row.profile} | ${row.treatment} | ${count("omission")} | ${count("alternative_covered")} | ${count("not_required")} | ${row.overall_verdict} |`);
  }
  lines.push("");
  return lines.join("\n");
}

function main() {
  const input = path.resolve(process.argv[2] ?? "");
  if (!input || !fs.existsSync(input)) throw new Error("usage: eval-pr-omission-report.mjs <adjudications.jsonl> [output.md]");
  const rows = jsonLines(fs.readFileSync(input, "utf8"));
  const markdown = renderMarkdown(summarize(rows), rows);
  const output = process.argv[3];
  if (output) fs.writeFileSync(path.resolve(output), markdown);
  else process.stdout.write(markdown);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main();
}
