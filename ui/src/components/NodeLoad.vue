<script setup lang="ts">
// How busy a node of the pool is, to choose where a lane starts (ADR-0036):
// what it has free now, of what it offers and has, its lanes, and the last
// hour of what its lanes used, from the coordinator's history.
import { computed, onBeforeUnmount, ref, watch } from 'vue';
import { api, paths } from '@/api';
import type { CapacityPoint, PoolNode } from '@/api/types';
import { bytes } from '@/lib/format';
import Sparkline from './Sparkline.vue';

const props = defineProps<{ pool: string; node: PoolNode; lanes: number }>();
const points = ref<CapacityPoint[]>([]);

async function load() {
  try {
    const r = await api().request<CapacityPoint[]>('GET', paths.poolHistory(props.pool, props.node.id, '1h'));
    if (r.ok && Array.isArray(r.body)) points.value = r.body;
  } catch {
    // Without history the numbers of its last report still show.
  }
}
watch(() => [props.pool, props.node.id], () => void load(), { immediate: true });
const timer = setInterval(() => void load(), 30_000);
onBeforeUnmount(() => clearInterval(timer));

const r = computed(() => props.node.report);
/** Free now: what it offers a new lane (the report's `cpus`/`memory`). */
const cpu = computed(() => {
  const total = r.value?.total?.cpus ?? 0;
  const used = r.value?.used?.cpus ?? 0;
  return { free: r.value?.cpus ?? 0, used, total, share: total ? Math.min(1, used / total) : 0 };
});
const mem = computed(() => {
  const total = r.value?.total?.memory ?? 0;
  const used = r.value?.used?.memory ?? 0;
  return { free: r.value?.memory ?? 0, used, total, share: total ? Math.min(1, used / total) : 0 };
});
const gpus = computed(() => (r.value?.gpus ?? []).filter((g) => g.offered));
const tone = (share: number) => (share > 0.85 ? 'danger' : share > 0.6 ? 'warn' : 'ok');
</script>

<template>
  <div class="load" data-testid="node-load">
    <div class="row">
      <span class="k">CPU</span>
      <span class="bar" :class="`tone-${tone(cpu.share)}`"><span :style="{ width: `${100 * cpu.share}%` }" /></span>
      <span class="v tabular"><strong>{{ cpu.free.toFixed(1) }}</strong> free · {{ cpu.used.toFixed(1) }} of {{ cpu.total.toFixed(0) }} used</span>
      <Sparkline v-if="points.length > 1" :values="points.map((p) => p.used_cpus)" :max="Math.max(1, cpu.total)" :height="18" :width="90" label="CPU used, last hour" />
    </div>
    <div class="row">
      <span class="k">Memory</span>
      <span class="bar" :class="`tone-${tone(mem.share)}`"><span :style="{ width: `${100 * mem.share}%` }" /></span>
      <span class="v tabular"><strong>{{ bytes(mem.free) }}</strong> free · {{ bytes(mem.used) }} of {{ bytes(mem.total) }} used</span>
      <Sparkline v-if="points.length > 1" :values="points.map((p) => p.used_memory)" :max="Math.max(1, mem.total)" :height="18" :width="90" label="Memory used, last hour" />
    </div>
    <div class="row meta">
      <span>{{ lanes }} lane{{ lanes === 1 ? '' : 's' }} running</span>
      <span v-if="r?.kept?.lanes">· {{ r.kept.lanes }} kept ({{ bytes(r.kept.bytes) }})</span>
      <span v-if="gpus.length">· {{ gpus.length }} GPU{{ gpus.length === 1 ? '' : 's' }} ({{ gpus.map((g) => `${g.load.toFixed(0)} %`).join(', ') }})</span>
      <span v-if="r?.devstack?.version">· devstack {{ r.devstack.version }}</span>
    </div>
  </div>
</template>

<style scoped>
.load {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 12px;
  text-align: left;
}
.row {
  display: flex;
  align-items: center;
  gap: 8px;
}
.k {
  width: 52px;
  color: var(--text-muted);
  flex: none;
}
.bar {
  --tone: var(--ok);
  width: 80px;
  height: 6px;
  border-radius: 3px;
  background: var(--surface-hover);
  overflow: hidden;
  flex: none;
}
.bar > span {
  display: block;
  height: 100%;
  background: var(--tone);
}
.bar.tone-warn {
  --tone: var(--warn);
}
.bar.tone-danger {
  --tone: var(--danger);
}
.v {
  flex: 1;
  min-width: 0;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.meta {
  color: var(--text-muted);
  gap: 4px;
  flex-wrap: wrap;
}
</style>
