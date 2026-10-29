<script setup lang="ts">
// The pool's capacity over time, machine by machine (ADR-0036): per resource
// what each node's lanes used, stacked, one colour per machine, against what
// the pool offered (dashed) on the scale of all it has. Read from the
// coordinator's history of the pool (the time axis, what it has and offers)
// and of each node, again when the pool's nodes changed (ADR-0073), at most
// every 15 s.
import { computed, onBeforeUnmount, ref, watch } from 'vue';
import { api, paths } from '@/api';
import type { CapacityPoint } from '@/api/types';
import { bytes, dateTime } from '@/lib/format';
import { throttle } from '@/lib/throttle';
import { usePool } from '@/stores/pool';

// With `node`: that machine alone, the same chart with one band.
const props = defineProps<{ pool: string; node?: string }>();
const pool = usePool();
const range = ref<'1h' | '24h'>('1h');
const total = ref<CapacityPoint[]>([]);
const perNode = ref<Record<string, CapacityPoint[]>>({});
const error = ref<string | null>(null);

/** The nodes that report, in a fixed order: a machine keeps its colour while others come and go. */
const nodes = computed<{ id: string; name: string }[]>(() => {
  const o = pool.overviews[props.pool];
  if (props.node) return [{ id: props.node, name: o?.members?.find((m) => m.id === props.node)?.name ?? o?.nodes?.find((n) => n.id === props.node)?.name ?? props.node }];
  return (o?.nodes ?? []).filter((n) => n.report?.total).sort((a, b) => a.id.localeCompare(b.id));
});
// ponytail: one request per node; a history endpoint per pool with every node in it, if pools grow past a few dozen nodes.
async function load() {
  const get = (node?: string) => api().request<CapacityPoint[]>('GET', paths.poolHistory(props.pool, node, range.value));
  try {
    // One machine: its own history is the time axis too.
    const [t, ...each] = props.node ? [await get(props.node)].flatMap((r) => [r, r]) : await Promise.all([get(), ...nodes.value.map((n) => get(n.id))]);
    if (t.ok && Array.isArray(t.body)) {
      total.value = t.body;
      error.value = null;
    } else error.value = 'no history from the coordinator';
    perNode.value = Object.fromEntries(nodes.value.map((n, i) => [n.id, each[i].ok && Array.isArray(each[i].body) ? each[i].body! : []]));
  } catch {
    error.value = 'the hub is not reachable';
  }
}
watch(() => [props.pool, props.node, range.value, nodes.value.map((n) => n.id).join()], () => void load(), { immediate: true });
const again = throttle(() => void load(), 15_000);
watch(() => pool.feeds[props.pool]?.versions.nodes, again);
onBeforeUnmount(again.cancel);

/** A node's value at time `at`: its last point at or before it (0 before its first). */
function at(points: CapacityPoint[], t: number, f: (p: CapacityPoint) => number) {
  let v = 0;
  for (const p of points) {
    if (p.at > t) break;
    v = f(p);
  }
  return v;
}

const W = 1000;
const H = 80;
type Res = { key: string; label: string; used: (p: CapacityPoint) => number; offered: (p: CapacityPoint) => number; has: (p: CapacityPoint) => number; fmt: (v: number) => string };
const base: Res[] = [
  { key: 'cpu', label: 'CPU', used: (p) => p.used_cpus, offered: (p) => p.offered_cpus, has: (p) => p.total_cpus, fmt: (v) => `${v.toFixed(1)} CPU` },
  { key: 'mem', label: 'Memory', used: (p) => p.used_memory, offered: (p) => p.offered_memory, has: (p) => p.total_memory, fmt: (v) => bytes(v) },
];
// GPU memory where a GPU reports: what lanes use of it, all of it offered when a GPU is.
const resources = computed<Res[]>(() =>
  total.value.some((p) => p.gpus > 0)
    ? [
        ...base,
        {
          key: 'gpu',
          label: 'GPU memory',
          used: (p) => p.gpu_memory_used,
          offered: (p) => (p.gpus_offered > 0 ? Math.max(0, p.gpu_memory - p.gpu_memory_used) : 0),
          has: (p) => p.gpu_memory,
          fmt: (v) => bytes(v),
        },
      ]
    : base,
);

const rows = computed(() => {
  const t = total.value;
  if (t.length < 2) return [];
  const x = (i: number) => (i / (t.length - 1)) * W;
  // Every number from the machines' own histories on the pool's time axis:
  // the bands, what is offered and what there is add up by construction.
  const sumAt = (p: CapacityPoint, f: (q: CapacityPoint) => number) => nodes.value.reduce((a, n) => a + at(perNode.value[n.id] ?? [], p.at, f), 0);
  return resources.value.map((r) => {
    const has = t.map((p) => sumAt(p, r.has));
    const ceiling = t.map((p) => sumAt(p, (q) => r.used(q) + r.offered(q)));
    const max = Math.max(1, ...has);
    const y = (v: number) => H - (v / max) * H;
    // Per machine its values, stacked from the bottom.
    const series = nodes.value.map((n) => ({ id: n.id, name: n.name, values: t.map((p) => at(perNode.value[n.id] ?? [], p.at, r.used)) }));
    const base = t.map(() => 0);
    const bands = series.map((s, k) => {
      const lower = [...base];
      s.values.forEach((v, i) => (base[i] += v));
      const top = base.map((v, i) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`);
      const bottom = lower.map((v, i) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`).reverse();
      return { ...s, slot: (k % 6) + 1, points: [...top, ...bottom].join(' ') };
    });
    const offeredLine = ceiling.map((v, i) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`).join(' ');
    return { ...r, max, bands, offeredLine, has, ceiling };
  });
});

// The hover: the point under the pointer, and each machine's value there.
const hover = ref<number | null>(null);
function move(e: PointerEvent) {
  const box = (e.currentTarget as SVGElement).getBoundingClientRect();
  const n = total.value.length;
  hover.value = Math.max(0, Math.min(n - 1, Math.round(((e.clientX - box.left) / box.width) * (n - 1))));
}
const now = computed(() => total.value.length - 1);
/** What the tooltip says at the point under the pointer. */
const tip = computed(() => {
  const i = hover.value;
  if (i === null || !rows.value.length) return null;
  return {
    when: dateTime(total.value[i].at * 1000),
    machines: rows.value[0].bands.map((b) => ({ id: b.id, name: b.name, slot: b.slot, text: rows.value.map((r) => r.fmt(r.bands.find((x) => x.id === b.id)?.values[i] ?? 0)).join(' · ') })),
    sums: rows.value.map((r) => ({ key: r.key, text: `${r.label} ${r.fmt(sumOf(r, i))} used · ${r.fmt(r.ceiling[i])} offered of ${r.fmt(r.has[i])}` })),
  };
});
const sumOf = (r: (typeof rows.value)[number], i: number) => r.bands.reduce((a, b) => a + (b.values[i] ?? 0), 0);
const hoverX = computed(() => (hover.value === null ? null : (hover.value / Math.max(1, total.value.length - 1)) * 100));
</script>

<template>
  <div class="pool-capacity" data-testid="pool-capacity-chart">
    <div class="range">
      <button type="button" class="btn btn-xs" :class="{ 'btn-primary': range === '1h' }" @click="range = '1h'">1 h</button>
      <button type="button" class="btn btn-xs" :class="{ 'btn-primary': range === '24h' }" @click="range = '24h'">24 h</button>
    </div>
    <p v-if="error && !total.length" class="faint small">{{ error }}</p>
    <p v-else-if="!nodes.length" class="faint small">No node of the pool reports what it has yet.</p>
    <p v-else-if="total.length < 2" class="faint small">Collecting: the coordinator keeps a point per report (every 15 s), since it started.</p>
    <template v-else>
      <div v-for="r in rows" :key="r.key" class="row">
        <span class="label">{{ r.label }}</span>
        <div class="plot">
          <svg
            :viewBox="`0 0 ${W} ${H}`"
            preserveAspectRatio="none"
            role="img"
            :aria-label="`${r.label} the pool's lanes used, by machine, against what it offered`"
            @pointermove="move"
            @pointerleave="hover = null"
          >
            <polygon v-for="b in r.bands" :key="b.id" :points="b.points" class="band" :style="{ fill: `var(--series-${b.slot})` }" />
            <polyline :points="r.offeredLine" class="offered" />
          </svg>
          <span v-if="hoverX !== null" class="cross" :style="{ left: `${hoverX}%` }" />
          <div
            v-if="tip && hoverX !== null && r === rows[0]"
            class="tip small tabular"
            :class="{ flip: hoverX > 60 }"
            :style="{ left: `${hoverX}%` }"
            role="tooltip"
            data-testid="pool-capacity-tip"
          >
            <strong>{{ tip.when }}</strong>
            <span v-for="m in tip.machines" :key="m.id" class="tip-row">
              <span class="swatch" :style="{ background: `var(--series-${m.slot})` }" aria-hidden="true" /> {{ m.name }} <span class="faint">{{ m.text }}</span>
            </span>
            <span v-for="x in tip.sums" :key="x.key" class="faint">{{ x.text }}</span>
          </div>
        </div>
      </div>
      <ul class="legend" data-testid="pool-capacity-legend">
        <li v-for="b in rows[0]?.bands ?? []" :key="b.id">
          <span class="swatch" :style="{ background: `var(--series-${b.slot})` }" aria-hidden="true" />
          <span>{{ b.name }}</span>
          <span class="faint tabular">
            <template v-for="(r, i) in rows" :key="r.key">{{ i ? ' · ' : '' }}{{ r.fmt(r.bands.find((x) => x.id === b.id)?.values[now] ?? 0) }}</template>
          </span>
        </li>
        <li class="faint"><span class="dash" aria-hidden="true" /> offered</li>
      </ul>
      <p class="sums faint small tabular" data-testid="pool-capacity-now">
        Now: <template v-for="(r, i) in rows" :key="r.key">{{ i ? ' — ' : '' }}{{ r.label }} {{ r.fmt(sumOf(r, now)) }} used · {{ r.fmt(r.ceiling[now]) }} offered of {{ r.fmt(r.has[now]) }}</template>
      </p>
    </template>
  </div>
</template>

<style scoped>
/* The categorical slots, in fixed order, validated against the surface of
   each theme (dataviz palette; light needs the legend's labels beside it). */
.pool-capacity {
  --series-1: #2a78d6;
  --series-2: #eb6834;
  --series-3: #1baf7a;
  --series-4: #eda100;
  --series-5: #e87ba4;
  --series-6: #6250d6;
  display: grid;
  grid-template-columns: 90px minmax(160px, 1fr);
  align-items: center;
  gap: 8px 12px;
}
@media (prefers-color-scheme: dark) {
  :root:not([data-theme='light']) .pool-capacity {
    --series-1: #3987e5;
    --series-2: #d95926;
    --series-3: #199e70;
    --series-4: #c98500;
    --series-5: #d55181;
    --series-6: #9085e9;
  }
}
:root[data-theme='dark'] .pool-capacity {
  --series-1: #3987e5;
  --series-2: #d95926;
  --series-3: #199e70;
  --series-4: #c98500;
  --series-5: #d55181;
  --series-6: #9085e9;
}
.range,
.pool-capacity > p,
.legend,
.sums {
  grid-column: 1 / -1;
}
.range {
  display: flex;
  gap: 4px;
  justify-content: flex-end;
}
.row {
  display: contents;
}
.label {
  font-size: 12px;
  color: var(--text-muted);
}
.plot {
  position: relative;
  height: 72px;
}
.plot svg {
  display: block;
  width: 100%;
  height: 100%;
  overflow: visible;
}
.band {
  stroke: var(--surface);
  stroke-width: 1;
  vector-effect: non-scaling-stroke;
}
.offered {
  fill: none;
  stroke: var(--text-muted);
  stroke-width: 1.5;
  stroke-dasharray: 4 3;
  vector-effect: non-scaling-stroke;
}
.cross {
  position: absolute;
  top: 0;
  bottom: 0;
  width: 1px;
  background: var(--text-muted);
  pointer-events: none;
}
.tip {
  position: absolute;
  top: 0;
  z-index: 2;
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 220px;
  margin-left: 12px;
  padding: 8px 10px;
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--surface);
  box-shadow: 0 4px 14px rgb(0 0 0 / 0.25);
  pointer-events: none;
  white-space: nowrap;
}
.tip.flip {
  transform: translateX(-100%);
  margin-left: -12px;
}
.tip-row {
  display: flex;
  align-items: center;
  gap: 6px;
}
.sums {
  margin: 0;
}
.legend {
  list-style: none;
  margin: 4px 0 0;
  padding: 0;
  display: flex;
  flex-wrap: wrap;
  gap: 6px 16px;
  font-size: 12px;
}
.legend li {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}
.swatch {
  width: 10px;
  height: 10px;
  border-radius: 2px;
}
.dash {
  width: 14px;
  border-top: 1.5px dashed var(--text-muted);
}
</style>
