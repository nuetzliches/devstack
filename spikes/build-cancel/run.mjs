#!/usr/bin/env node
// Spike (b): build cancellation (plan §7.2). Windows; builds run in a Job
// Object through `procspike exec`, the way the daemon will run them.
//
//   node run.mjs --scenario scenarios/<name>.local.json [--only b1,b5,b6]
//
// Two clones of the same commit: A is cancelled and resumed, B builds the same
// changes without interruption. Both builds map their source root to /src, so
// deterministic compilation makes A's and B's assemblies byte-identical when
// nothing went wrong.

import { spawnSync } from 'node:child_process';
import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const procspike = path.resolve(here, '../process-ownership/target/release/procspike.exe');
const args = Object.fromEntries(process.argv.slice(2).reduce((acc, a, i, all) => (a.startsWith('--') ? [...acc, [a.slice(2), all[i + 1]]] : acc), []));
const sc = JSON.parse(fs.readFileSync(args.scenario, 'utf8'));
const only = args.only ? args.only.split(',') : null;
const runDir = path.join(here, 'results', new Date().toISOString().replace(/[:.]/g, '-'));
fs.mkdirSync(runDir, { recursive: true });
const A = path.resolve(sc.root, 'a');
const B = path.resolve(sc.root, 'b');
const log = (...m) => console.log(`[${new Date().toISOString().slice(11, 19)}]`, ...m);
const rand = (lo, hi) => Math.round(lo + Math.random() * (hi - lo));
let logN = 0;

function sh(cmd, cwd) {
  const r = spawnSync(cmd, { cwd, shell: true, encoding: 'utf8', maxBuffer: 1 << 28 });
  if (r.status !== 0) throw new Error(`${cmd} failed:\n${(r.stdout + r.stderr).slice(-2000)}`);
  return r.stdout;
}

// Run a build in a job via procspike. opts: stopAfterMs, graceful, servers ('job' | 'off' | 'outside').
function build(root, cmd, cwd, opts = {}) {
  const full = opts.servers === 'off' ? `${cmd} -nodeReuse:false -p:UseSharedCompilation=false` : cmd;
  const logFile = path.join(runDir, `build-${++logN}.log`);
  if (opts.servers === 'outside') {
    // Plain child process, not in a job: build servers outlive the build.
    const t0 = Date.now();
    const r = spawnSync(full, { cwd, shell: true, encoding: 'utf8', maxBuffer: 1 << 28 });
    fs.writeFileSync(logFile, r.stdout + r.stderr);
    return { exit_code: r.status, stopped: false, total_ms: Date.now() - t0 };
  }
  const a = ['exec', '--log', logFile];
  if (opts.stopAfterMs) a.push('--stop-after-ms', String(opts.stopAfterMs));
  if (opts.graceful) a.push('--graceful');
  a.push(full);
  const r = spawnSync(procspike, a, { cwd, encoding: 'utf8' });
  if (r.status !== 0) throw new Error(`procspike exec failed: ${r.stderr}`);
  return { ...JSON.parse(r.stdout.trim().split('\n').pop()), log: logFile };
}

const dotnetCmd = (root) => `dotnet build ${sc.project} -c Debug -nologo -v q -p:BootstrapDevAppsettings=false "-p:PathMap=${root}=/src"`;

function hashDir(dir, filter) {
  const out = {};
  const walk = (d) => {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const p = path.join(d, e.name);
      if (e.isDirectory()) walk(p);
      else if (!filter || filter.test(e.name)) out[path.relative(dir, p).replaceAll('\\', '/')] = crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex').slice(0, 16);
    }
  };
  walk(dir);
  return out;
}
const diff = (x, y) => {
  const keys = new Set([...Object.keys(x), ...Object.keys(y)]);
  return [...keys].filter((k) => x[k] !== y[k]);
};

// Time until the output directory can be renamed, i.e. no file in it is locked.
function unlockedAfterMs(dir) {
  const t0 = Date.now();
  while (Date.now() - t0 < 30000) {
    try { fs.renameSync(dir, `${dir}.probe`); fs.renameSync(`${dir}.probe`, dir); return Date.now() - t0; } catch { /* locked */ }
  }
  return null;
}

function probe(root, i) {
  // A new public type changes the reference assembly, so every dependant
  // recompiles: a long build with restore, compile and copy phases to cancel
  // into. It lives in a file of its own next to `touch`, so the file's own
  // namespace style does not matter.
  const file = path.join(root, path.dirname(sc.touch), 'DevstackBenchProbe.cs');
  fs.writeFileSync(file, `namespace DevstackBenchProbe;

public static class Probe${i} { }
`);
}

function mustSucceed(r, what) {
  if (r.exit_code !== 0) throw new Error(`${what} failed (exit ${r.exit_code}), see ${r.log ?? 'log'}`);
  return r;
}

const results = { scenario: path.basename(args.scenario), started: new Date().toISOString() };
const save = () => fs.writeFileSync(path.join(runDir, 'results.json'), JSON.stringify(results, null, 2));

// ---------------------------------------------------------------------- setup
if (!fs.existsSync(A) || args.fresh === 'true') {
  fs.mkdirSync(sc.root, { recursive: true });
  for (const d of [A, B]) {
    fs.rmSync(d, { recursive: true, force: true });
    log(`clone ${d}`);
    sh(`git clone -q --no-checkout "${sc.repo}" "${d}"`, path.dirname(d));
    sh(`git -c advice.detachedHead=false checkout -q ${sc.ref}`, d);
  }
  if (sc.ng) sh('npm install --no-audit --no-fund', path.join(A, sc.ng.cwd));
}
for (const d of [A, B]) sh('dotnet build-server shutdown', d);
log('baseline builds');
for (const d of [A, B]) {
  const r = build(d, dotnetCmd(d), d);
  if (r.exit_code !== 0) throw new Error(`baseline build failed in ${d}: ${r.log}`);
}
let iter = 0;

// --------------------------------------------- b1-b4: cancel, resume, compare
if (!only || only.includes('b1')) {
  // Calibrate: duration of one uninterrupted probe build.
  probe(A, ++iter); probe(B, iter);
  const cal = mustSucceed(build(A, dotnetCmd(A), A), 'calibration A'); mustSucceed(build(B, dotnetCmd(B), B), 'calibration B');
  log(`probe build takes ${cal.total_ms} ms`);
  results.calibrationMs = cal.total_ms;
  results.b1 = [];
  for (let k = 0; k < sc.iterations; k++) {
    const graceful = k >= sc.iterations - sc.gracefulIterations;
    probe(A, ++iter); probe(B, iter);
    const stopAt = rand(300, Math.round(cal.total_ms * 0.9));
    const cancelled = build(A, dotnetCmd(A), A, { stopAfterMs: stopAt, graceful });
    const unlock = unlockedAfterMs(path.join(A, sc.output));
    const resumed = build(A, dotnetCmd(A), A);
    const normal = mustSucceed(build(B, dotnetCmd(B), B), 'uninterrupted build B');
    const d = diff(hashDir(path.join(A, sc.output), /\.dll$/), hashDir(path.join(B, sc.output), /\.dll$/));
    const row = {
      graceful, stopAtMs: stopAt, wasStopped: cancelled.stopped, goneMs: cancelled.gone_after_ms, escalated: cancelled.escalated,
      unlockedMs: unlock, resumedOk: resumed.exit_code === 0, resumedMs: resumed.total_ms, normalMs: normal.total_ms,
      identical: d.length === 0, differing: d.slice(0, 5),
    };
    results.b1.push(row);
    log(`b1 #${k} ${JSON.stringify(row)}`);
    save();
  }
}

// --------------------------------------------------------- b5: build servers
if (!only || only.includes('b5')) {
  results.b5 = {};
  for (const servers of ['job', 'outside', 'off']) {
    sh('dotnet build-server shutdown', A);
    const times = [];
    for (let k = 0; k < sc.serverIterations; k++) {
      probe(A, ++iter);
      const r = build(A, dotnetCmd(A), A, { servers });
      if (r.exit_code !== 0) throw new Error(`b5 build failed (${servers})`);
      times.push({ ms: r.total_ms, stragglersKilled: r.stragglers_killed ?? null });
    }
    results.b5[servers] = times;
    log(`b5 ${servers}: ${JSON.stringify(times)}`);
    save();
  }
  sh('dotnet build-server shutdown', A);
}

// ------------------------------------------------------- b6: SPA build cancel
if (sc.ng && (!only || only.includes('b6'))) {
  const cwd = path.join(A, sc.ng.cwd);
  const dist = path.join(cwd, sc.ng.output);
  fs.rmSync(dist, { recursive: true, force: true });
  const ref = build(A, sc.ng.cmd, cwd);
  if (ref.exit_code !== 0) throw new Error('reference SPA build failed');
  const refHash = hashDir(dist);
  results.b6 = [];
  for (let k = 0; k < sc.ngIterations; k++) {
    fs.rmSync(dist, { recursive: true, force: true });
    const stopAt = rand(2000, Math.round(ref.total_ms * 0.9));
    const cancelled = build(A, sc.ng.cmd, cwd, { stopAfterMs: stopAt });
    fs.rmSync(dist, { recursive: true, force: true });
    const resumed = build(A, sc.ng.cmd, cwd);
    const d = diff(hashDir(dist), refHash);
    const row = { stopAtMs: stopAt, wasStopped: cancelled.stopped, goneMs: cancelled.gone_after_ms, resumedOk: resumed.exit_code === 0, resumedMs: resumed.total_ms, referenceMs: ref.total_ms, identical: d.length === 0, differing: d.slice(0, 5) };
    results.b6.push(row);
    log(`b6 #${k} ${JSON.stringify(row)}`);
    save();
  }
}

results.finished = new Date().toISOString();
save();
log(`results: ${runDir}`);
