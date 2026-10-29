<script setup lang="ts">
// How busy a node of the pool is, to choose where a lane starts (ADR-0036):
// per resource one chart on the scale of what the machine has: the last hour
// of what its lanes used against what it offered (the coordinator's
// history), and now, with what the lane would take on top (`need`). The
// history is read again when the pool's nodes changed (ADR-0073), at most
// every 15 s.
import { computed, onBeforeUnmount, ref, watch } from 'vue';
import { api, paths } from '@/api';
import type { CapacityPoint, PoolNode } from '@/api/types';
import { bytes } from '@/lib/format';
import { throttle } from '@/lib/throttle';
import { usePool } from '@/stores/pool';
import LoadChart from './LoadChart.vue';

const props = defineProps<{ pool: string; node: PoolNode; lanes: number; need?: { cpus: number; memory: number } }>();
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
const again = throttle(() => void load(), 15_000);
const pool = usePool();
watch(() => pool.feeds[props.pool]?.versions.nodes, again);
onBeforeUnmount(again.cancel);

const r = computed(() => props.node.report);
/** Free now: what it offers a new lane (the report's `cpus`/`memory`); its
 * lanes may use up to `used + free` (the offer), of `total`. */
const cpu = computed(() => {
  const total = r.value?.total?.cpus ?? 0;
  const used = r.value?.used?.cpus ?? 0;
  const free = r.value?.cpus ?? 0;
  return { free, used, total, offered: used + free };
});
const mem = computed(() => {
  const total = r.value?.total?.memory ?? 0;
  const used = r.value?.used?.memory ?? 0;
  const free = r.value?.memory ?? 0;
  return { free, used, total, offered: used + free };
});
const gpus = computed(() => (r.value?.gpus ?? []).filter((g) => g.offered));
</script>

<template>
  <div class="load" data-testid="node-load">
    <div class="row">
      <span class="k">CPU</span>
      <LoadChart
        :total="cpu.total"
        :used="cpu.used"
        :limit="cpu.offered"
        :need="need?.cpus"
        :history="points.map((p) => p.used_cpus)"
        :ceiling="points.map((p) => p.used_cpus + p.offered_cpus)"
        label="CPU: what its lanes used in the last hour, what it offered (dashed), and now, with the lane on top"
      />
      <span class="v tabular" :title="`${cpu.used.toFixed(1)} used by its lanes, ${cpu.offered.toFixed(1)} offered to the pool, ${cpu.total.toFixed(0)} cores in all`">
        <strong>{{ cpu.free.toFixed(1) }}</strong> free · {{ cpu.offered.toFixed(1) }} offered<template v-if="cpu.total"> of {{ cpu.total.toFixed(0) }}</template>
      </span>
    </div>
    <div class="row">
      <span class="k">Memory</span>
      <LoadChart
        :total="mem.total"
        :used="mem.used"
        :limit="mem.offered"
        :need="need?.memory"
        :history="points.map((p) => p.used_memory)"
        :ceiling="points.map((p) => p.used_memory + p.offered_memory)"
        label="Memory: what its lanes used in the last hour, what it offered (dashed), and now, with the lane on top"
      />
      <span class="v tabular" :title="`${bytes(mem.used)} used by its lanes, ${bytes(mem.offered)} offered to the pool, ${bytes(mem.total)} in all`">
        <strong>{{ bytes(mem.free) }}</strong> free · {{ bytes(mem.offered) }} offered<template v-if="mem.total"> of {{ bytes(mem.total) }}</template>
      </span>
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
