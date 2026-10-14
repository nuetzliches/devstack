// Fixture for spike (a). Roles, chosen by argv[2]:
//   (none)      server: writes node.started, starts a grandchild, records stop
//   grandchild  records its own start and stop
//   stubborn    ignores every stop signal
//   detached    starts a detached, unref'd child (tries to outlive its parent)
const fs = require('node:fs');
const path = require('node:path');
const { spawn } = require('node:child_process');

const markers = process.env.MARKER_DIR ?? '.';
const role = process.argv[2] ?? 'server';
const mark = (name, text) => fs.writeFileSync(path.join(markers, `${role === 'server' ? 'node' : role}.${name}`), String(text));

mark('started', process.pid);

const onSignal = (sig) => {
  if (role === 'stubborn') return; // ignore
  mark('stopped', sig);
  process.exit(0);
};
for (const sig of ['SIGINT', 'SIGBREAK', 'SIGTERM']) process.on(sig, onSignal);

if (role === 'server') {
  spawn(process.execPath, [__filename, 'grandchild'], { stdio: 'inherit' });
}
if (role === 'detached') {
  const c = spawn(process.execPath, [__filename, 'orphan'], { detached: true, stdio: 'ignore' });
  c.unref();
}

setInterval(() => {}, 1 << 30);
