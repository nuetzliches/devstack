<script setup lang="ts">
import { computed, ref } from 'vue';
import type { LaneDisk } from '@/api/types';
import { bytes, percent } from '@/lib/format';
import { useStack } from '@/stores/stack';
import Sparkline from '@/components/Sparkline.vue';
import Icon from '@/components/icons/Icon.vue';

const stack = useStack();
const u = computed(() => stack.usage);
type Kind = 'service' | 'container' | 'builds' | 'group';
const filter = ref<'all' | Kind>('all');

type Row = { key: string; name: string; kind: Kind; sub?: string; cpu: number; memory: number; processes?: number; to?: object };

const rows = computed<Row[]>(() => {
  const r = u.value;
  if (!r) return [];
  const out: Row[] = [];
  for (const s of r.services)
    out.push({ key: `svc:${s.name}`, name: s.name, kind: 'service', cpu: s.cpu, memory: s.memory, processes: s.processes, to: { name: 'service', params: { stack: stack.id ?? '', name: s.name } } });
  out.push({ key: 'builds', name: 'builds', kind: 'builds', sub: 'all build jobs', cpu: r.builds.cpu, memory: r.builds.memory, processes: r.builds.processes });
  for (const c of r.containers) out.push({ key: `ctr:${c.group}/${c.service}`, name: c.service, kind: 'container', sub: c.group, cpu: c.cpu, memory: c.memory });
  for (const g of r.groups) {
    const label = stack.view?.groups.find((x) => x.name === g.name)?.label;
    out.push({ key: `grp:${g.name}`, name: label ?? g.name, kind: 'group', sub: label ? g.name : undefined, cpu: g.cpu, memory: g.memory });
  }
  return out;
});
const shown = computed(() => (filter.value === 'all' ? rows.value : rows.value.filter((r) => r.kind === filter.value)));

function cpuSeries(key: string) {
  return (stack.history.get(key) ?? []).map((p) => p.cpu);
}
function memSeries(key: string) {
  return (stack.history.get(key) ?? []).map((p) => p.memory);
}

const diskParts = (d: LaneDisk) =>
  [
    { k: 'snapshot', label: 'Snapshots', v: d.snapshot, color: 'var(--accent)' },
    { k: 'artifacts', label: 'Artifacts', v: d.artifacts, color: 'var(--info)' },
    { k: 'logs', label: 'Logs', v: d.logs, color: 'var(--warn)' },
    { k: 'volumes', label: 'Volumes', v: d.volumes ?? 0, color: 'var(--ok)', na: d.volumes === null },
  ] as { k: string; label: string; v: number; color: string; na?: boolean }[];

function diskTotal(d: LaneDisk) {
  return d.snapshot + d.artifacts + d.logs + (d.volumes ?? 0);
}

const kindLabel: Record<Kind, string> = { service: 'service', container: 'container', builds: 'builds', group: 'group' };
const filters: { v: 'all' | Kind; label: string }[] = [
  { v: 'all', label: 'All' },
  { v: 'service', label: 'Services' },
  { v: 'container', label: 'Containers' },
  { v: 'group', label: 'Groups' },
  { v: 'builds', label: 'Builds' },
];
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <h1>Usage</h1>
        <p class="subtitle">CPU (% of one core), memory and disk, live every 2 s. Sparklines show the last 10 minutes.</p>
      </div>
    </header>

    <div v-if="u?.docker_error" class="banner warn" data-testid="docker-error">
      <Icon name="alert" :size="18" />
      <span class="grow"><strong>Container figures unavailable:</strong> {{ u.docker_error }}</span>
    </div>

    <template v-if="u">
      <div class="lanes stale-dim">
        <section v-for="l in u.lanes" :key="l.name" class="card lane" :data-testid="`lane-${l.name}`">
          <header class="card-header">
            <Icon name="gitBranch" :size="16" class="muted" />
            <h2>Lane {{ l.name }}</h2>
          </header>
          <div class="card-body lane-body">
            <div class="metric">
              <span class="ml">CPU</span>
              <span class="mv tabular">{{ percent(l.cpu) }}</span>
              <Sparkline :values="cpuSeries(`lane:${l.name}`)" :width="240" :height="40" :min-max="10" fluid :label="`Lane ${l.name} CPU`" />
            </div>
            <div class="metric">
              <span class="ml">Memory</span>
              <span class="mv tabular">{{ bytes(l.memory) }}</span>
              <Sparkline :values="memSeries(`lane:${l.name}`)" :width="240" :height="40" :min-max="16 * 1024 * 1024" :zero-based="false" fluid color="var(--ok)" :label="`Lane ${l.name} memory`" />
            </div>
            <div v-if="l.disk" class="metric disk">
              <span class="ml">Disk</span>
              <span class="mv tabular">{{ bytes(diskTotal(l.disk)) }}</span>
              <div class="bar" role="img" :aria-label="`Disk use of lane ${l.name}`">
                <span v-for="p in diskParts(l.disk)" :key="p.k" :style="{ width: `${(p.v / Math.max(1, diskTotal(l.disk))) * 100}%`, background: p.color }" />
              </div>
              <ul class="legend">
                <li v-for="p in diskParts(l.disk)" :key="p.k">
                  <span class="sw" :style="{ background: p.color }" />{{ p.label }}
                  <span class="tabular lv">{{ p.na ? 'n/a' : bytes(p.v) }}</span>
                </li>
              </ul>
            </div>
          </div>
        </section>
      </div>

      <section class="card">
        <header class="card-header wrap-head">
          <h2>Processes and containers</h2>
          <span class="grow" />
          <div class="btn-group" role="group" aria-label="Filter rows">
            <button v-for="f in filters" :key="f.v" type="button" class="chip-toggle" :aria-pressed="filter === f.v" @click="filter = f.v">{{ f.label }}</button>
          </div>
        </header>
        <div class="table-wrap">
          <table class="table" data-testid="usage-table">
            <thead>
              <tr>
                <th>Name</th>
                <th class="hide-sm">Type</th>
                <th class="num">CPU</th>
                <th class="hide-md">Last 10 min</th>
                <th class="num">Memory</th>
                <th class="hide-md">Last 10 min</th>
                <th class="num hide-sm">Procs</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="r in shown" :key="r.key" :data-row="r.key">
                <td>
                  <RouterLink v-if="r.to" :to="r.to" class="rname">{{ r.name }}</RouterLink>
                  <span v-else class="rname">{{ r.name }}</span>
                  <span v-if="r.sub" class="faint rsub">{{ r.sub }}</span>
                </td>
                <td class="hide-sm"><span class="pill sm" :class="{ accent: r.kind === 'service', info: r.kind === 'container', warn: r.kind === 'builds' }">{{ kindLabel[r.kind] }}</span></td>
                <td class="num">{{ percent(r.cpu) }}</td>
                <td class="hide-md spark-cell"><Sparkline :values="cpuSeries(r.key)" :width="120" :height="24" :min-max="r.kind === 'builds' ? 50 : 5" /></td>
                <td class="num">{{ bytes(r.memory) }}</td>
                <td class="hide-md spark-cell"><Sparkline :values="memSeries(r.key)" :width="120" :height="24" :min-max="4 * 1024 * 1024" :zero-based="false" color="var(--ok)" /></td>
                <td class="num hide-sm">{{ r.processes ?? '—' }}</td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>
    </template>
    <div v-else class="card">
      <div class="empty">
        <Icon name="gauge" :size="32" />
        <h2>No usage figures yet</h2>
        <p v-if="!stack.running">The stack is not running.</p>
        <p v-else>The first report arrives within a few seconds.</p>
      </div>
    </div>
  </div>
</template>

<style scoped>
.lanes {
  display: grid;
  gap: 16px;
}
.lane-body {
  display: grid;
  grid-template-columns: 1fr 1fr 1.3fr;
  gap: 24px;
}
@media (max-width: 860px) {
  .lane-body {
    grid-template-columns: 1fr 1fr;
  }
  .disk {
    grid-column: 1 / -1;
  }
}
@media (max-width: 480px) {
  .lane-body {
    grid-template-columns: 1fr;
  }
}
.metric {
  display: flex;
  flex-direction: column;
  gap: 4px;
  min-width: 0;
}
.ml {
  font-size: 11px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  color: var(--text-muted);
}
.mv {
  font-size: 22px;
  font-weight: 650;
  line-height: 1.2;
  margin-bottom: 4px;
}
.bar {
  display: flex;
  height: 10px;
  border-radius: 999px;
  overflow: hidden;
  background: var(--surface-3);
  gap: 2px;
}
.bar span {
  display: block;
  min-width: 2px;
}
.legend {
  list-style: none;
  padding: 0;
  margin: 8px 0 0;
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 4px 16px;
  font-size: 12.5px;
}
.legend li {
  display: flex;
  align-items: center;
  gap: 6px;
}
.sw {
  width: 9px;
  height: 9px;
  border-radius: 3px;
  flex: none;
}
.lv {
  margin-left: auto;
  color: var(--text-muted);
}
.wrap-head {
  flex-wrap: wrap;
}
.rname {
  font-weight: 600;
  color: var(--text);
}
.rsub {
  margin-left: 6px;
  font-size: 12px;
}
.spark-cell {
  width: 130px;
}
@media (max-width: 900px) {
  .hide-md {
    display: none;
  }
}
@media (max-width: 520px) {
  .hide-sm {
    display: none;
  }
}
</style>
