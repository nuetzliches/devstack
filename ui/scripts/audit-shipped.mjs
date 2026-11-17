// The dashboard's advisory gate (ADR-0111): an npm advisory counts when the
// package it is for has code in what ships, the built bundle. The build
// lists those packages in `bundle-packages.json` (vite.config.ts). One for a
// build tool alone (vite, a bundler plugin, also where a runtime package
// lists it as a dependency for its build plugin) is listed here, not
// counted. No list, or no readable answer from npm, fails the check.
//
//   npm run build && node scripts/audit-shipped.mjs   (from ui/; exit 1 when
//   a shipped package has a known advisory, 2 when it cannot tell)

import { spawnSync } from 'node:child_process'
import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const ui = join(dirname(fileURLToPath(import.meta.url)), '..')

let shipped
try {
  shipped = new Set(JSON.parse(readFileSync(join(ui, 'bundle-packages.json'), 'utf8')))
} catch (e) {
  console.error(`no bundle-packages.json (run \`npm run build\` first): ${e.message}`)
  process.exit(2)
}

// npm is a script on Windows (npm.cmd), started only through a shell; the
// arguments are this file's own. npm exits 1 on a finding, so the exit code
// says nothing, only the JSON does.
const r = spawnSync('npm audit --omit=dev --json', {
  cwd: ui,
  encoding: 'utf8',
  maxBuffer: 64 * 1024 * 1024,
  shell: true,
})
let audit
try {
  audit = JSON.parse(r.stdout)
} catch {
  console.error(`npm audit gave no JSON (exit ${r.status}):\n${r.stderr}`)
  process.exit(2)
}
if (typeof audit.vulnerabilities !== 'object' || audit.vulnerabilities === null) {
  console.error('npm audit --json has no `vulnerabilities`: cannot tell what is affected')
  process.exit(2)
}

// `via` holds the advisories for the package itself (objects) and the names
// of the packages it is marked through (strings).
const own = (v) => v.via.filter((x) => typeof x === 'object')

let failed = 0
for (const v of Object.values(audit.vulnerabilities)) {
  const line = `${v.name} ${v.range}: ${
    own(v)
      .map((x) => `${x.url ?? x.title} (${x.severity})`)
      .join(', ') || `through ${v.via.join(', ')}`
  }`
  if (own(v).length > 0 && shipped.has(v.name)) {
    console.error(`shipped:     ${line}`)
    failed++
  } else {
    console.log(`not shipped: ${line}`)
  }
}
if (failed) {
  console.error(`${failed} package(s) in the dashboard's bundle have a known advisory`)
  process.exit(1)
}
console.log(`no known advisory in the dashboard's bundle (${shipped.size} packages: ${[...shipped].join(', ')})`)
