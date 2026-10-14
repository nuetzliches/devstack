#!/usr/bin/env node
// Compares bench.mjs result files: one column per run, one row per metric,
// median in seconds. Usage: node report.mjs results/a.json results/b.json ...

import fs from 'node:fs';
import path from 'node:path';

const files = process.argv.slice(2);
if (!files.length) {
  console.error('usage: node report.mjs <result.json> [...]');
  process.exit(1);
}

const runs = files.map((f) => ({ name: path.basename(f, '.json').replace(/-\d{4}-\d\d-\d\dT.*$/, ''), data: JSON.parse(fs.readFileSync(f, 'utf8')) }));

const rows = new Map();
for (const [i, run] of runs.entries()) {
  for (const [step, r] of Object.entries(run.data.steps)) {
    if (r.failures?.length) {
      const key = `${step} · failed attempts`;
      if (!rows.has(key)) rows.set(key, Array(runs.length).fill(''));
      rows.get(key)[i] = String(r.failures.length);
    }
    if (r.error) {
      const key = `${step}`;
      if (!rows.has(key)) rows.set(key, Array(runs.length).fill(''));
      rows.get(key)[i] = 'failed';
      continue;
    }
    for (const [metric, s] of Object.entries(r.summary)) {
      const key = metric === 'totalMs' ? step : `${step} · ${metric.replace(/Ms$/, '')}`;
      if (!rows.has(key)) rows.set(key, Array(runs.length).fill(''));
      const spread = s.n > 1 ? ` (${(s.minMs / 1000).toFixed(1)}–${(s.maxMs / 1000).toFixed(1)})` : '';
      rows.get(key)[i] = `${(s.medianMs / 1000).toFixed(2)}${spread}`;
    }
  }
}

const env = (k) => runs.map((r) => String(r.data.environment[k] ?? ''));
const line = (cells) => `| ${cells.join(' | ')} |`;
console.log(line(['metric (median s, min–max)', ...runs.map((r) => r.name)]));
console.log(line(['---', ...runs.map(() => '---:')]));
console.log(line(['file system', ...env('fileSystem')]));
console.log(line(['logical CPUs', ...env('logicalCpus')]));
for (const [k, v] of rows) console.log(line([k, ...v]));
