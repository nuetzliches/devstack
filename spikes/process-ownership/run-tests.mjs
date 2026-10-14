#!/usr/bin/env node
// Drives `procspike` through the scenarios of plan §7.1 and checks the outcome
// from the outside: markers written by the fixtures, process liveness, exit codes.
//
// Usage: node run-tests.mjs [scenario ...]   (default: all)
// Results: results/<timestamp>.json plus a table on stdout.

import { spawn, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const exe = path.join(here, 'target', 'release', process.platform === 'win32' ? 'procspike.exe' : 'procspike');
const resultsDir = path.join(here, 'results');
const runDir = path.join(resultsDir, new Date().toISOString().replace(/[:.]/g, '-'));
fs.mkdirSync(runDir, { recursive: true });

const SVC = {
  dotnet: 'dotnet run --project fixtures/dotnet-host --no-build',
  node: 'npm run start --prefix fixtures/node-app',
  stubborn: 'node fixtures/node-app/server.js stubborn',
  detached: 'node fixtures/node-app/server.js detached',
  watch: 'dotnet watch --project fixtures/dotnet-host --non-interactive run',
  breakaway: (state) => `"${exe}" breakaway --out "${path.join(state, 'breakaway.txt')}"`,
};

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const alive = (pid) => { try { process.kill(pid, 0); return true; } catch { return false; } };
const exists = (f) => fs.existsSync(f);
const read = (f) => (exists(f) ? fs.readFileSync(f, 'utf8').trim() : null);
const killTree = (pid) => spawnSync('taskkill', ['/PID', String(pid), '/T', '/F']);

async function waitFor(pred, ms, step = 100) {
  const end = Date.now() + ms;
  while (Date.now() < end) { if (pred()) return true; await sleep(step); }
  return pred();
}

function start(name, { method = 'break', hold = 6, stopTimeout = 15, services, flags = [], stateName = name, detached = false }) {
  const state = path.join(runDir, stateName);
  fs.mkdirSync(state, { recursive: true });
  const args = ['run', '--method', method, '--hold', String(hold), '--stop-timeout', String(stopTimeout),
    '--state', state, ...flags, ...services.map(([n, cmd]) => `${n}=${typeof cmd === 'function' ? cmd(state) : cmd}`)];
  // detached: no console for the supervisor, as for a daemon started in the background.
  const child = spawn(exe, args, { cwd: here, env: { ...process.env, MARKER_DIR: state }, stdio: ['ignore', 'pipe', 'pipe'], detached, windowsHide: true });
  let out = '';
  child.stdout.on('data', (d) => (out += d));
  child.stderr.on('data', (d) => (out += d));
  const exited = new Promise((r) => child.on('exit', (code) => r(code)));
  return { state, child, exited, output: () => out };
}

const pidsOf = (state) => Object.values(JSON.parse(read(path.join(state, 'pids.json')) ?? '{}')).flat();
const markerPid = (state, name) => Number(read(path.join(state, `${name}.started`)));

// ------------------------------------------------------------------ scenarios

const scenarios = {
  // a1 + a2: trees are captured per service; a graceful stop reaches exactly one service.
  async a1a2_conpty() { return graceful('conpty'); },
  async a1a2_break() { return graceful('break'); },
  // a2b: supervisor started without a console (detached daemon): without and with a hidden console of its own.
  async a2b_noconsole() { return graceful('break', { detached: true, name: 'a2b_noconsole' }); },
  async a2b_hiddenconsole() { return graceful('break', { detached: true, flags: ['--alloc-console'], name: 'a2b_hidden' }); },

  // a3: a process that ignores stop signals is killed after the timeout, tree gone.
  async a3() {
    const r = start('a3', { hold: 3, stopTimeout: 3, services: [['stubborn', SVC.stubborn]] });
    await r.exited;
    const pids = pidsOf(r.state);
    await sleep(500);
    const report = JSON.parse(read(path.join(r.state, 'report.json')));
    const s = report.stops[0];
    return {
      pass: s.killed && !s.stopped_gracefully && pids.every((p) => !alive(p)),
      detail: { killed: s.killed, stop_ms: s.stop_ms, survivors: pids.filter(alive) },
    };
  },

  // a4: supervisor killed hard -> nothing of any tree survives, detached child included.
  async a4() { return hardKill('a4', { services: [['dotnet', SVC.dotnet], ['node', SVC.node], ['detached', SVC.detached]] }, false); },

  // a5: negative control, inheritable job handle -> trees survive the supervisor.
  async a5_negative() {
    return hardKill('a5', { method: 'break', flags: ['--inheritable-job'], services: [['node', SVC.node]] }, true);
  },

  // a6: breakaway is refused; negative control with breakaway allowed.
  async a6() {
    const r = start('a6', { hold: 3, stopTimeout: 3, services: [['breakaway', SVC.breakaway]] });
    await r.exited;
    const outcome = read(path.join(r.state, 'breakaway.txt'));
    return { pass: outcome?.startsWith('refused') === true, detail: { outcome } };
  },
  async a6_negative() {
    const r = start('a6n', { hold: 3, stopTimeout: 3, flags: ['--allow-breakaway'], services: [['breakaway', SVC.breakaway]] });
    await r.exited;
    const outcome = read(path.join(r.state, 'breakaway.txt')) ?? '';
    const escaped = Number(outcome.split(' ')[1]);
    const survived = escaped > 0 && alive(escaped);
    if (survived) killTree(escaped);
    return { pass: survived, detail: { outcome, survivedSupervisor: survived } };
  },

  // a7: a tool with its own job objects (dotnet watch) inside ours; a4 still holds.
  async a7() { return hardKill('a7', { services: [['watch', SVC.watch]] }, false, 'dotnet'); },

  // a8: the OS lock: a second instance is refused, and after a hard kill of the
  // first the lock is free immediately.
  async a8() {
    const a = start('a8a', { hold: 0, services: [['node', SVC.stubborn]], stateName: 'a8' });
    await waitFor(() => exists(path.join(a.state, 'pids.json')), 20000);
    const b = start('a8b', { hold: 0, services: [['node', SVC.stubborn]], stateName: 'a8' });
    const bCode = await Promise.race([b.exited, sleep(8000).then(() => 'still running')]);
    if (bCode === 'still running') b.child.kill();
    spawnSync('taskkill', ['/PID', String(a.child.pid), '/F']);
    await a.exited;
    const c = start('a8c', { hold: 0, services: [['node', SVC.stubborn]], stateName: 'a8' });
    const cGotLock = await waitFor(() => read(path.join(a.state, 'supervisor.pid')) === String(c.child.pid), 20000);
    spawnSync('taskkill', ['/PID', String(c.child.pid), '/F']);
    await c.exited;
    return { pass: bCode === 3 && cGotLock, detail: { secondInstanceExit: bCode, lockFreeAfterHardKill: cGotLock } };
  },
};

async function graceful(method, { detached = false, flags = [], name = `a1a2_${method}` } = {}) {
  const r = start(name, { method, detached, flags, services: [['dotnet', SVC.dotnet], ['node', SVC.node]] });
  const code = await r.exited;
  const pids = JSON.parse(read(path.join(r.state, 'pids.json')) ?? '{}');
  const report = JSON.parse(read(path.join(r.state, 'report.json')) ?? '{"stops":[]}');
  const m = (n) => path.join(r.state, n);
  const mtime = (n) => (exists(m(n)) ? fs.statSync(m(n)).mtimeMs : null);
  const nodeStop = report.stops.find((s) => s.service === 'node');
  const independent = mtime('node.stopped') !== null && nodeStop && mtime('node.stopped') >= nodeStop.requested_at_ms - 50;
  const all = Object.values(pids).flat();
  await sleep(500);
  const detail = {
    exit: code,
    processes: Object.fromEntries(Object.entries(pids).map(([k, v]) => [k, v.length])),
    stops: report.stops.map((s) => ({ service: s.service, graceful: s.stopped_gracefully, ms: s.stop_ms, signalError: s.signal_error })),
    markers: { dotnet: exists(m('dotnet.stopped')), node: read(m('node.stopped')), grandchild: read(m('grandchild.stopped')) },
    independent,
    survivors: all.filter(alive),
  };
  const pass = Object.values(pids).every((v) => v.length >= 2)
    && report.stops.every((s) => s.stopped_gracefully)
    && detail.markers.dotnet && detail.markers.node && independent && detail.survivors.length === 0;
  return { pass, detail };
}

async function hardKill(name, opts, expectSurvivors, startedMarker) {
  const r = start(name, { hold: 0, ...opts });
  await waitFor(() => exists(path.join(r.state, 'pids.json')), 30000);
  if (startedMarker) await waitFor(() => exists(path.join(r.state, `${startedMarker}.started`)), 60000);
  await sleep(1000);
  const pids = pidsOf(r.state);
  const orphan = markerPid(r.state, 'orphan');
  if (orphan) pids.push(orphan);
  spawnSync('taskkill', ['/PID', String(r.child.pid), '/F']); // like a crash: no cleanup code runs
  await r.exited;
  await sleep(2000);
  const survivors = pids.filter(alive);
  survivors.forEach(killTree); // never leave anything behind
  const pass = expectSurvivors ? survivors.length > 0 : survivors.length === 0;
  return { pass, detail: { recorded: pids.length, survivors, started: startedMarker ? exists(path.join(r.state, `${startedMarker}.started`)) : undefined } };
}

// ------------------------------------------------------------------------ main

const wanted = process.argv.slice(2);
const results = {};
for (const [name, fn] of Object.entries(scenarios)) {
  if (wanted.length && !wanted.includes(name)) continue;
  process.stdout.write(`${name} ... `);
  try {
    results[name] = await fn();
  } catch (e) {
    results[name] = { pass: false, detail: { error: String(e) } };
  }
  console.log(results[name].pass ? 'PASS' : 'FAIL', JSON.stringify(results[name].detail));
}
fs.writeFileSync(path.join(runDir, 'results.json'), JSON.stringify(results, null, 2));
console.log(`\nresults: ${runDir}`);
