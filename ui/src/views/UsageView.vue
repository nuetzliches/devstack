<script setup lang="ts">
import { computed, ref } from 'vue';
import { bytes, percent } from '@/lib/format';
import { useSort } from '@/lib/sort';
import { useStack } from '@/stores/stack';
import Sparkline from '@/components/Sparkline.vue';
import LaneUsage from '@/components/LaneUsage.vue';
import StoreLine from '@/components/StoreLine.vue';
import SortTh from '@/components/SortTh.vue';
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
const filtered = computed(() => (filter.value === 'all' ? rows.value : rows.value.filter((r) => r.kind === filter.value)));
// The sparkline columns sort by the peak of the last 10 minutes.
const peak = (xs: number[]) => (xs.length ? Math.max(...xs) : null);
const sort = useSort(
  filtered,
  {
    name: (r) => r.name,
    kind: (r) => kindLabel[r.kind],
    cpu: (r) => r.cpu,
    cpuPeak: (r) => peak(cpuSeries(r.key)),
    memory: (r) => r.memory,
    memoryPeak: (r) => peak(memSeries(r.key)),
    processes: (r) => r.processes,
  },
  { id: 'usage' },
);
const shown = sort.sorted;

function cpuSeries(key: string) {
  return (stack.history.get(key) ?? []).map((p) => p.cpu);
}
function memSeries(key: string) {
  return (stack.history.get(key) ?? []).map((p) => p.memory);
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
        <LaneUsage v-for="l in u.lanes" :key="l.name" :lane="l" />
      </div>

      <StoreLine class="stale-dim" />

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
                <SortTh label="Name" :dir="sort.dirOf('name')" @toggle="sort.toggle('name')" />
                <SortTh label="Type" class="hide-sm" :dir="sort.dirOf('kind')" @toggle="sort.toggle('kind')" />
                <SortTh label="CPU" class="num" :dir="sort.dirOf('cpu')" @toggle="sort.toggle('cpu')" />
                <SortTh label="Last 10 min" class="hide-md" :dir="sort.dirOf('cpuPeak')" @toggle="sort.toggle('cpuPeak')" />
                <SortTh label="Memory" class="num" :dir="sort.dirOf('memory')" @toggle="sort.toggle('memory')" />
                <SortTh label="Last 10 min" class="hide-md" :dir="sort.dirOf('memoryPeak')" @toggle="sort.toggle('memoryPeak')" />
                <SortTh label="Procs" class="num hide-sm" :dir="sort.dirOf('processes')" @toggle="sort.toggle('processes')" />
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
