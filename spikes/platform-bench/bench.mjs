#!/usr/bin/env node
// Platform benchmark for ADR-0011: runs the same scenario on different file
// systems / platforms and records wall-clock timings as JSON.
//
// One script for every platform on purpose: the harness must not be the
// variable being measured. Requires Node >= 20 and git; the scenario decides
// which other toolchains are needed.
//
// Usage:
//   node bench.mjs --scenario <file.json> --root <work dir> --label <name>
//                  [--repo <source repo>] [--ref <commit>] [--repeat N]
//                  [--only step1,step2] [--skip-setup] [--dry]

import { spawn, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { performance } from 'node:perf_hooks';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const isWin = process.platform === 'win32';

// ------------------------------------------------------------------ arguments

function parseArgs(argv) {
  const out = { repeat: 3 };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (!a.startsWith('--')) throw new Error(`unexpected argument ${a}`);
    const key = a.slice(2);
    if (key === 'dry' || key === 'skip-setup') { out[key] = true; continue; }
    out[key] = argv[++i];
  }
  for (const k of ['scenario', 'root', 'label']) if (!out[k]) throw new Error(`--${k} is required`);
  out.repeat = Number(out.repeat);
  out.only = out.only ? out.only.split(',') : null;
  return out;
}

const args = parseArgs(process.argv.slice(2));
const scenario = JSON.parse(fs.readFileSync(args.scenario, 'utf8'));
const root = path.resolve(args.root);
const repo = args.repo ?? scenario.source?.repo;
const ref = args.ref ?? scenario.source?.ref;

const env = { ...process.env, ...(scenario.env ?? {}) };
const resultsDir = path.join(here, 'results');
const stamp = new Date().toISOString().replace(/[:.]/g, '-');
const logDir = path.join(resultsDir, `${args.label}-${stamp}-logs`);
fs.mkdirSync(logDir, { recursive: true });

// -------------------------------------------------------------------- helpers

const log = (...m) => console.log(`[${new Date().toISOString().slice(11, 19)}]`, ...m);

function run(cmd, { cwd = root, logName, allowFail = false } = {}) {
  if (args.dry) { log(`(dry) ${cmd}  @ ${cwd}`); return { ms: 0, code: 0 }; }
  const t0 = performance.now();
  const r = spawnSync(cmd, { cwd, env, shell: true, encoding: 'utf8', maxBuffer: 1 << 28 });
  const ms = performance.now() - t0;
  if (logName) fs.writeFileSync(path.join(logDir, `${logName}.log`), `${cmd}\n\n${r.stdout ?? ''}\n${r.stderr ?? ''}`);
  if (r.status !== 0 && !allowFail) {
    const tail = `${r.stdout ?? ''}${r.stderr ?? ''}`.split('\n').slice(-30).join('\n');
    throw new Error(`command failed (${r.status}): ${cmd}\n${tail}`);
  }
  return { ms, code: r.status };
}

function killTree(child) {
  if (!child || child.exitCode !== null) return;
  if (isWin) spawnSync('taskkill', ['/pid', String(child.pid), '/T', '/F']);
  else try { process.kill(-child.pid, 'SIGKILL'); } catch { /* already gone */ }
}

const rel = (p) => path.join(root, p);

// Prepare operations are not timed. They run in JS so that no shell syntax
// differs between platforms.
function prepare(ops = []) {
  for (const op of ops) {
    if (args.dry) { log('(dry) prepare', JSON.stringify(op)); continue; }
    if (op.clean) {
      // -x ignores .gitignore, the -e patterns still protect what we keep.
      const keep = (op.keep ?? []).map((k) => `-e ${k}`).join(' ');
      run(`git clean -xfdq ${keep} -- ${op.clean.join(' ')}`);
    } else if (op.rm) {
      for (const p of op.rm) fs.rmSync(rel(p), { recursive: true, force: true });
    } else if (op.append) {
      fs.appendFileSync(rel(op.append), op.text ?? `\n// bench ${Date.now()}\n`);
    } else if (op.restore) {
      run(`git checkout -- ${op.restore}`);
      const now = new Date();
      fs.utimesSync(rel(op.restore), now, now); // checkout may keep an old mtime
    } else if (op.run) {
      run(op.run, { cwd: op.cwd ? rel(op.cwd) : root });
    } else {
      throw new Error(`unknown prepare op ${JSON.stringify(op)}`);
    }
  }
}

// Many small files: isolates the file system from the toolchains.
function fsMicro(step) {
  const n = step.files ?? 20000;
  const dir = path.join(root, '.bench-fsmicro');
  fs.rmSync(dir, { recursive: true, force: true });
  const payload = Buffer.alloc(step.bytes ?? 1024, 'x');
  const t = {};
  let t0 = performance.now();
  for (let i = 0; i < n; i++) {
    const d = path.join(dir, String(i % 100), String(Math.floor(i / 100) % 10));
    if (i < 1000) fs.mkdirSync(d, { recursive: true });
    fs.writeFileSync(path.join(d, `f${i}.txt`), payload);
  }
  t.write = performance.now() - t0;
  t0 = performance.now();
  const walk = (d) => { for (const e of fs.readdirSync(d, { withFileTypes: true })) e.isDirectory() ? walk(path.join(d, e.name)) : fs.statSync(path.join(d, e.name)); };
  walk(dir);
  t.stat = performance.now() - t0;
  t0 = performance.now();
  const read = (d) => { for (const e of fs.readdirSync(d, { withFileTypes: true })) e.isDirectory() ? read(path.join(d, e.name)) : fs.readFileSync(path.join(d, e.name)); };
  read(dir);
  t.read = performance.now() - t0;
  t0 = performance.now();
  fs.rmSync(dir, { recursive: true, force: true });
  t.delete = performance.now() - t0;
  return t;
}

// Start a watching process, wait until it is ready, then measure how long it
// takes to report a rebuild after a file change.
async function watchLatency(step, logName) {
  const out = fs.createWriteStream(path.join(logDir, `${logName}.log`));
  const child = spawn(step.run, { cwd: step.cwd ? rel(step.cwd) : root, env, shell: true, detached: !isWin });
  let buffer = '';
  let matches = 0;
  const done = new RegExp(step.doneRegex, 'gi');
  const onData = (d) => {
    const s = d.toString();
    out.write(s);
    buffer += s;
    const m = buffer.match(done);
    if (m) { matches += m.length; buffer = ''; }
    else buffer = buffer.slice(-4096);
  };
  child.stdout.on('data', onData);
  child.stderr.on('data', onData);

  const waitFor = (count, timeoutMs) => new Promise((resolve, reject) => {
    const t0 = performance.now();
    const iv = setInterval(() => {
      if (matches >= count) { clearInterval(iv); resolve(performance.now() - t0); }
      else if (child.exitCode !== null) { clearInterval(iv); reject(new Error(`watcher exited (${child.exitCode})`)); }
      else if (performance.now() - t0 > timeoutMs) { clearInterval(iv); reject(new Error('watcher timeout')); }
    }, 25);
  });

  // The first rebuild after start-up is much slower than the following ones
  // (caches are filled lazily), so it is reported on its own.
  const timings = { ready: null, firstRebuild: null, rebuilds: [] };
  try {
    timings.ready = await waitFor(1, (step.readyTimeoutS ?? 600) * 1000);
    await new Promise((r) => setTimeout(r, (step.settleS ?? 5) * 1000));
    for (let i = 0; i < (step.changes ?? 3); i++) {
      const before = matches;
      fs.appendFileSync(rel(step.file), `\n// bench ${Date.now()}\n`);
      const ms = await waitFor(before + 1, (step.changeTimeoutS ?? 300) * 1000);
      if (i === 0) timings.firstRebuild = ms;
      else timings.rebuilds.push(ms);
      await new Promise((r) => setTimeout(r, (step.settleS ?? 5) * 1000));
    }
  } finally {
    killTree(child);
    out.end();
    run(`git checkout -- ${step.file}`, { allowFail: true });
  }
  return timings;
}

function describeEnvironment() {
  const info = {
    label: args.label,
    platform: process.platform,
    release: os.release(),
    cpu: os.cpus()[0]?.model,
    logicalCpus: os.cpus().length,
    memGb: Math.round(os.totalmem() / 2 ** 30),
    node: process.version,
    root,
    ref,
    startedAt: new Date().toISOString(),
  };
  const probe = (cmd) => spawnSync(cmd, { shell: true, encoding: 'utf8' }).stdout?.trim() || null;
  info.dotnet = probe('dotnet --version');
  info.git = probe('git --version');
  if (isWin) {
    const drive = root.slice(0, 1);
    info.fileSystem = probe(`powershell -NoProfile -Command "(Get-Volume -DriveLetter ${drive}).FileSystem"`);
  } else {
    info.fileSystem = probe(`df -T "${fs.existsSync(root) ? root : path.dirname(root)}" | tail -1 | awk '{print $2}'`);
  }
  return info;
}

// ---------------------------------------------------------------------- main

const stats = (xs) => {
  const s = [...xs].sort((a, b) => a - b);
  const med = s.length % 2 ? s[(s.length - 1) / 2] : (s[s.length / 2 - 1] + s[s.length / 2]) / 2;
  return { n: s.length, medianMs: Math.round(med), minMs: Math.round(s[0]), maxMs: Math.round(s[s.length - 1]) };
};

const result = { environment: describeEnvironment(), steps: {} };
const resultFile = path.join(resultsDir, `${args.label}-${stamp}.json`);
const save = () => fs.writeFileSync(resultFile, JSON.stringify(result, null, 2));
log('environment', JSON.stringify(result.environment));

if (!args['skip-setup']) {
  if (!repo || !ref) throw new Error('--repo and --ref (or scenario.source) are required for setup');
  log(`setup: clone ${repo} @ ${ref} -> ${root}`);
  fs.rmSync(root, { recursive: true, force: true });
  fs.mkdirSync(path.dirname(root), { recursive: true });
  run(`git clone -q --no-checkout "${repo}" "${root}"`, { cwd: path.dirname(root) });
  run(`git -c advice.detachedHead=false checkout -q ${ref}`);
  for (const s of scenario.setup ?? []) prepare([s]);
}
// The file system can only be probed once the root exists.
result.environment = { ...describeEnvironment(), startedAt: result.environment.startedAt };
save();

for (const step of scenario.steps) {
  if (args.only && !args.only.includes(step.name)) continue;
  const repeat = step.repeat ?? args.repeat;
  log(`step ${step.name} (warmup ${step.warmup ?? 0}, repeat ${repeat})`);
  const samples = [];
  const failures = [];
  try {
    for (let i = 0; i < (step.warmup ?? 0) + repeat; i++) {
      const warm = i < (step.warmup ?? 0);
      prepare(step.prepare);
      const logName = `${step.name}-${warm ? 'warmup' : 'run'}${i}`;
      let sample;
      if (step.kind === 'fs-micro') sample = fsMicro(step);
      else if (step.kind === 'watch') sample = await watchLatency(step, logName);
      else {
        // A failed attempt is data, not noise: on Windows, transient file locks
        // (EBUSY from scanners or indexers) are one of the things measured.
        // Retry up to step.retries times and count the failures.
        for (let attempt = 0; ; attempt++) {
          const r = run(step.run, { cwd: step.cwd ? rel(step.cwd) : root, logName: `${logName}-a${attempt}`, allowFail: true });
          if (r.code === 0) { sample = { totalMs: r.ms }; break; }
          failures.push({ iteration: i, attempt, code: r.code });
          log(`  attempt ${attempt} failed (exit ${r.code})`);
          if (attempt >= (step.retries ?? 1)) throw new Error(`command failed (${r.code}) after ${attempt + 1} attempts: ${step.run}`);
          prepare(step.prepare);
        }
      }
      prepare(step.after);
      if (!warm) samples.push(sample);
      log(`  ${warm ? 'warmup' : 'sample'} ${JSON.stringify(sample, (k, v) => (typeof v === 'number' ? Math.round(v) : v))}`);
    }
    const keys = Object.keys(samples[0] ?? {});
    const summary = {};
    for (const k of keys) {
      const vals = samples.flatMap((s) => (Array.isArray(s[k]) ? s[k] : [s[k]])).filter((v) => typeof v === 'number');
      if (vals.length) summary[k] = stats(vals);
    }
    result.steps[step.name] = { summary, samples, failures };
  } catch (e) {
    log(`  FAILED: ${e.message.split('\n')[0]}`);
    result.steps[step.name] = { error: e.message, samples, failures };
  }
  save();
}

result.environment.finishedAt = new Date().toISOString();
save();
log(`results: ${resultFile}`);
