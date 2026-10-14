#!/usr/bin/env node
// Linux half of spike (a). Runs inside a container (see Dockerfile.linux):
//   docker run --rm --privileged --cgroupns=private --init -v <out>:/out procspike-linux
// Without --privileged the cgroup scenarios are reported as skipped.
//
// Matrix: {shim, no shim} x {cgroup, process group only}, each with a hard
// kill of the supervisor, followed by `procspike cleanup` (ADR-0016).

import { spawn, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const exe = path.join(here, 'target', 'release', 'procspike');
const out = process.env.OUT_DIR ?? path.join(here, 'results');
const runDir = path.join(out, `linux-${new Date().toISOString().replace(/[:.]/g, '-')}`);
fs.mkdirSync(runDir, { recursive: true });

const cgroupBase = '/sys/fs/cgroup';
const cgroupsWritable = (() => {
  try { fs.mkdirSync(path.join(cgroupBase, 'procspike-probe')); fs.rmdirSync(path.join(cgroupBase, 'procspike-probe')); return true; } catch { return false; }
})();

const SVC = {
  dotnet: 'dotnet run --project fixtures/dotnet-host --no-build',
  node: 'npm run start --prefix fixtures/node-app',
  stubborn: 'node fixtures/node-app/server.js stubborn',
  detached: 'node fixtures/node-app/server.js detached',
};

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const alive = (pid) => {
  try {
    const stat = fs.readFileSync(`/proc/${pid}/stat`, 'utf8');
    return stat.slice(stat.lastIndexOf(')') + 2, stat.lastIndexOf(')') + 3) !== 'Z';
  } catch { return false; }
};
const read = (f) => (fs.existsSync(f) ? fs.readFileSync(f, 'utf8').trim() : null);
const waitFor = async (pred, ms) => { const end = Date.now() + ms; while (Date.now() < end) { if (pred()) return true; await sleep(100); } return pred(); };

function start(name, { hold = 6, stopTimeout = 15, services, flags = [], stateName = name }) {
  const state = path.join(runDir, stateName);
  fs.mkdirSync(state, { recursive: true });
  const args = ['run', '--method', 'signal', '--hold', String(hold), '--stop-timeout', String(stopTimeout), '--state', state, ...flags,
    ...services.map(([n, c]) => `${n}=${c}`)];
  const child = spawn(exe, args, { cwd: here, env: { ...process.env, MARKER_DIR: state }, stdio: ['ignore', 'pipe', 'pipe'] });
  let log = '';
  child.stdout.on('data', (d) => (log += d));
  child.stderr.on('data', (d) => (log += d));
  return { state, child, exited: new Promise((r) => child.on('exit', (c) => r(c))), log: () => log };
}

const cg = (name) => (cgroupsWritable ? ['--cgroup-root', path.join(cgroupBase, `procspike-${name}`)] : null);

async function graceful(name, flags) {
  const r = start(name, { flags, services: [['dotnet', SVC.dotnet], ['node', SVC.node]] });
  const code = await r.exited;
  const m = (n) => path.join(r.state, n);
  const pids = JSON.parse(read(m('pids.json')) ?? '{}');
  const report = JSON.parse(read(m('report.json')) ?? '{"stops":[]}');
  const nodeStop = report.stops.find((s) => s.service === 'node');
  const nodeStoppedAt = fs.existsSync(m('node.stopped')) ? fs.statSync(m('node.stopped')).mtimeMs : null;
  const detail = {
    exit: code,
    processes: Object.fromEntries(Object.entries(pids).map(([k, v]) => [k, v.length])),
    stops: report.stops.map((s) => ({ service: s.service, graceful: s.stopped_gracefully, ms: s.stop_ms })),
    markers: { dotnet: fs.existsSync(m('dotnet.stopped')), node: read(m('node.stopped')), grandchild: read(m('grandchild.stopped')) },
    independent: nodeStoppedAt !== null && nodeStop && nodeStoppedAt >= nodeStop.requested_at_ms - 50,
    survivors: Object.values(pids).flat().filter(alive),
  };
  const pass = report.stops.length === 2 && report.stops.every((s) => s.stopped_gracefully)
    && detail.markers.dotnet && detail.markers.node && detail.independent && detail.survivors.length === 0;
  return { pass, detail: { ...detail, log: code ? r.log().slice(-400) : undefined } };
}

// Hard kill of the supervisor, then cleanup. `expect` = survivors expected before / after cleanup.
async function crash(name, flags, expect) {
  const r = start(name, { hold: 0, flags, services: [['dotnet', SVC.dotnet], ['node', SVC.node], ['detached', SVC.detached]] });
  await waitFor(() => fs.existsSync(path.join(r.state, 'pids.json')), 30000);
  await waitFor(() => fs.existsSync(path.join(r.state, 'orphan.started')), 10000);
  await sleep(1000);
  const pids = Object.values(JSON.parse(read(path.join(r.state, 'pids.json')))).flat();
  const orphan = Number(read(path.join(r.state, 'orphan.started')));
  if (orphan && !pids.includes(orphan)) pids.push(orphan);
  r.child.kill('SIGKILL');
  await r.exited;
  await sleep(1500);
  const before = pids.filter(alive);
  const c = spawnSync(exe, ['cleanup', '--state', r.state], { encoding: 'utf8' });
  await sleep(500);
  const after = pids.filter(alive);
  after.forEach((p) => { try { process.kill(p, 'SIGKILL'); } catch {} });
  const pass = (before.length > 0) === expect.before && (after.length > 0) === expect.after
    && (expect.orphanAfter === undefined || after.includes(orphan) === expect.orphanAfter);
  return { pass, detail: { recorded: pids.length, survivorsBeforeCleanup: before.length, survivorsAfterCleanup: after.length, orphanSurvived: after.includes(orphan), cleanup: c.stdout.trim() } };
}

const scenarios = {
  l_a1a2_pgroup: () => graceful('l_a1a2_pgroup', []),
  l_a1a2_shim_cgroup: () => (cg('g') ? graceful('l_a1a2_shim_cgroup', ['--shim', ...cg('g')]) : null),
  async l_a3() {
    const r = start('l_a3', { hold: 3, stopTimeout: 3, flags: ['--shim'], services: [['stubborn', SVC.stubborn]] });
    await r.exited;
    const pids = Object.values(JSON.parse(read(path.join(r.state, 'pids.json')))).flat();
    const s = JSON.parse(read(path.join(r.state, 'report.json'))).stops[0];
    await sleep(300);
    return { pass: s.killed && pids.every((p) => !alive(p)), detail: { killed: s.killed, ms: s.stop_ms } };
  },
  // Process group only, no shim: the leader dies with the supervisor (PDEATHSIG), its children do not.
  l_a4_pgroup: () => crash('l_a4_pgroup', [], { before: true, after: true, orphanAfter: true }),
  // cgroup, no shim: nothing dies with the supervisor, cleanup by cgroup.kill gets everything.
  l_a4_cgroup: () => (cg('c') ? crash('l_a4_cgroup', cg('c'), { before: true, after: false }) : null),
  // Shim, process group only: the shim kills its group; a setsid child has left the group.
  l_a4_shim_pgroup: () => crash('l_a4_shim_pgroup', ['--shim'], { before: true, after: true, orphanAfter: true }),
  // Shim and cgroup: kill-on-close semantics, nothing survives even before cleanup.
  l_a4_shim_cgroup: () => (cg('s') ? crash('l_a4_shim_cgroup', ['--shim', ...cg('s')], { before: false, after: false }) : null),
  async l_a8() {
    const a = start('l_a8a', { hold: 0, services: [['s', SVC.stubborn]], stateName: 'l_a8' });
    await waitFor(() => fs.existsSync(path.join(a.state, 'pids.json')), 20000);
    const b = start('l_a8b', { hold: 0, services: [['s', SVC.stubborn]], stateName: 'l_a8' });
    const bCode = await Promise.race([b.exited, sleep(8000).then(() => 'running')]);
    if (bCode === 'running') b.child.kill('SIGKILL');
    a.child.kill('SIGKILL');
    await a.exited;
    const c = start('l_a8c', { hold: 0, services: [['s', SVC.stubborn]], stateName: 'l_a8' });
    const got = await waitFor(() => read(path.join(a.state, 'supervisor.pid')) === String(c.child.pid), 20000);
    c.child.kill('SIGKILL');
    await c.exited;
    spawnSync(exe, ['cleanup', '--state', a.state]);
    return { pass: bCode === 3 && got, detail: { secondInstanceExit: bCode, lockFreeAfterHardKill: got } };
  },
  // Cleanup must not hit a process that only shares a number with a recorded group.
  async l_cleanup_decoy() {
    const decoy = spawn('sleep', ['60'], { detached: true, stdio: 'ignore' }); // own process group
    await sleep(200);
    const recFile = path.join(runDir, 'decoy.json');
    fs.writeFileSync(recFile, JSON.stringify([{ name: 'old', pgid: decoy.pid, leader_start: Number.MAX_SAFE_INTEGER, cgroup: null }]));
    spawnSync(exe, ['cleanup', '--state', runDir, '--records', recFile]);
    await sleep(200);
    const spared = alive(decoy.pid);
    decoy.kill('SIGKILL');
    return { pass: spared, detail: { decoySpared: spared } };
  },
};

const wanted = process.argv.slice(2);
const results = { cgroupsWritable };
for (const [name, fn] of Object.entries(scenarios)) {
  if (wanted.length && !wanted.includes(name)) continue;
  process.stdout.write(`${name} ... `);
  let r;
  try { r = await fn(); } catch (e) { r = { pass: false, detail: { error: String(e) } }; }
  if (r === null) { console.log('SKIP (cgroups not writable)'); results[name] = 'skipped'; continue; }
  results[name] = r;
  console.log(r.pass ? 'PASS' : 'FAIL', JSON.stringify(r.detail));
}
fs.writeFileSync(path.join(runDir, 'results.json'), JSON.stringify(results, null, 2));
console.log(`\nresults: ${runDir}`);
