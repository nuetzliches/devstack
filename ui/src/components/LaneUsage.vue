<script setup lang="ts">
// What one lane costs now: CPU and memory with the last 10 minutes, and its
// disk. The usage page shows one per lane, the overview the one it shows.
import type { LaneDisk, LaneUsage as Lane } from '@/api/types';
import { CPU_HINT, bytes, percent } from '@/lib/format';
import { useStack } from '@/stores/stack';
import Sparkline from '@/components/Sparkline.vue';
import Icon from '@/components/icons/Icon.vue';

// `title`: the lane as the page names it. A lane on a node reports from its
// own daemon there, to which it is the main tree (`main`).
defineProps<{ lane: Lane; link?: boolean; title?: string }>();

const stack = useStack();

function cpuSeries(key: string) {
  return (stack.history.get(key) ?? []).map((p) => p.cpu);
}
function memSeries(key: string) {
  return (stack.history.get(key) ?? []).map((p) => p.memory);
}

const diskParts = (d: LaneDisk) =>
  [
    {
      k: 'snapshot',
      label: 'Snapshots',
      v: d.snapshot,
      color: 'var(--accent)',
      tip: 'The copy of the source that builds and `prepare` run in, with what they leave there: obj/, node_modules, caches.',
    },
    {
      k: 'artifacts',
      label: 'Artifacts',
      v: d.artifacts,
      color: 'var(--info)',
      tip: "Published build outputs that services run from, every kept build. A lane shares the main tree's store, which counts there.",
    },
    {
      k: 'logs',
      label: 'Logs',
      v: d.logs,
      color: 'var(--warn)',
      tip: 'Service, build and command logs.',
    },
    {
      k: 'volumes',
      label: 'Volumes',
      v: d.volumes ?? 0,
      color: 'var(--ok)',
      na: d.volumes === null,
      tip:
        "Named Docker volumes of the stack's Compose projects (data of databases, caches). Images and bind mounts are not counted." +
        (d.volumes === null ? ' Not measured: Docker is not reachable, or the infra is external or not there.' : ''),
    },
  ] as { k: string; label: string; v: number; color: string; tip: string; na?: boolean }[];

function diskTotal(d: LaneDisk) {
  return d.snapshot + d.artifacts + d.logs + (d.volumes ?? 0);
}
</script>

<template>
  <section class="card lane" :data-testid="`lane-${lane.name}`">
    <header class="card-header">
      <Icon name="gitBranch" :size="16" class="muted" />
      <h2>Lane {{ title ?? lane.name }}</h2>
      <template v-if="link">
        <span class="grow" />
        <RouterLink class="more" :to="{ name: 'usage', params: { stack: stack.id ?? '' } }" title="Every service, container and build job" data-testid="usage-link">
          Usage <Icon name="chevronRight" :size="14" />
        </RouterLink>
      </template>
    </header>
    <div class="card-body lane-body">
      <div class="metric">
        <span class="ml" :title="CPU_HINT"><Icon name="cpu" :size="13" /> CPU</span>
        <span class="mv tabular">{{ percent(lane.cpu) }}</span>
        <Sparkline :values="cpuSeries(`lane:${lane.name}`)" :width="240" :height="40" :min-max="10" fluid :label="`Lane ${lane.name} CPU`" />
      </div>
      <div class="metric">
        <span class="ml"><Icon name="memoryStick" :size="13" /> Memory</span>
        <span class="mv tabular">{{ bytes(lane.memory) }}</span>
        <Sparkline :values="memSeries(`lane:${lane.name}`)" :width="240" :height="40" :min-max="16 * 1024 * 1024" :zero-based="false" fluid color="var(--ok)" :label="`Lane ${lane.name} memory`" />
      </div>
      <div v-if="lane.disk" class="metric disk">
        <span class="ml"><Icon name="hardDrive" :size="13" /> Disk</span>
        <span class="mv tabular">{{ bytes(diskTotal(lane.disk)) }}</span>
        <div class="bar" role="img" :aria-label="`Disk use of lane ${lane.name}`">
          <span v-for="p in diskParts(lane.disk)" :key="p.k" :title="`${p.label}: ${p.na ? 'n/a' : bytes(p.v)}. ${p.tip}`" :style="{ width: `${(p.v / Math.max(1, diskTotal(lane.disk))) * 100}%`, background: p.color }" />
        </div>
        <ul class="legend">
          <li v-for="p in diskParts(lane.disk)" :key="p.k" :title="p.tip">
            <span class="sw" :style="{ background: p.color }" />{{ p.label }}
            <span class="tabular lv">{{ p.na ? 'n/a' : bytes(p.v) }}</span>
          </li>
        </ul>
      </div>
    </div>
  </section>
</template>

<style scoped>
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
.more {
  display: inline-flex;
  align-items: center;
  gap: 2px;
  font-size: 13px;
}
.metric {
  display: flex;
  flex-direction: column;
  gap: 4px;
  min-width: 0;
}
.ml {
  display: inline-flex;
  align-items: center;
  gap: 5px;
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
  /* two columns, one where they no longer fit */
  grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
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
  white-space: nowrap;
  color: var(--text-muted);
}
</style>
