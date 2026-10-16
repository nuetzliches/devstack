<script setup lang="ts">
// A node's or the pool's capacity over time (ADR-0036): what its lanes use,
// against what it offers and has, for CPU, memory and GPUs. Read from the
// coordinator's history; `node` unset: the whole pool.
import { computed, onBeforeUnmount, ref, watch } from 'vue';
import { api, paths } from '@/api';
import type { CapacityPoint } from '@/api/types';
import { bytes } from '@/lib/format';
import Sparkline from './Sparkline.vue';

const props = defineProps<{ pool: string; node?: string }>();
const range = ref<'1h' | '24h'>('1h');
const points = ref<CapacityPoint[]>([]);
const error = ref<string | null>(null);

async function load() {
  try {
    const r = await api().request<CapacityPoint[]>('GET', paths.poolHistory(props.pool, props.node, range.value));
    if (r.ok && Array.isArray(r.body)) {
      points.value = r.body;
      error.value = null;
    } else error.value = 'no history from the coordinator';
  } catch {
    error.value = 'the hub is not reachable';
  }
}
watch(() => [props.pool, props.node, range.value], () => void load(), { immediate: true });
const timer = setInterval(() => void load(), 30_000);
onBeforeUnmount(() => clearInterval(timer));

const last = computed(() => points.value[points.value.length - 1] ?? null);
const rows = computed(() => {
  const p = points.value;
  const l = last.value;
  if (!l) return [];
  const out = [
    {
      key: 'cpu',
      label: 'CPU',
      values: p.map((x) => x.used_cpus),
      max: Math.max(1, ...p.map((x) => x.total_cpus)),
      text: `${l.used_cpus.toFixed(1)} used · ${l.offered_cpus.toFixed(1)} offered · ${l.total_cpus.toFixed(0)} cores`,
    },
    {
      key: 'mem',
      label: 'Memory',
      values: p.map((x) => x.used_memory),
      max: Math.max(1, ...p.map((x) => x.total_memory)),
      text: `${bytes(l.used_memory)} used · ${bytes(l.offered_memory)} offered · ${bytes(l.total_memory)}`,
    },
  ];
  if (p.some((x) => x.gpus > 0))
    out.push({
      key: 'gpu',
      label: 'GPU',
      values: p.map((x) => x.gpu_load),
      max: Math.max(100, ...p.map((x) => x.gpus * 100)),
      text: `${l.gpus} GPU${l.gpus === 1 ? '' : 's'} (${l.gpus_offered} offered) · load ${l.gpu_load.toFixed(0)} % · ${bytes(l.gpu_memory_used)} of ${bytes(l.gpu_memory)}`,
    });
  return out;
});
</script>

<template>
  <div class="capacity" data-testid="capacity-chart">
    <div class="range">
      <button type="button" class="btn btn-xs" :class="{ 'btn-primary': range === '1h' }" @click="range = '1h'">1 h</button>
      <button type="button" class="btn btn-xs" :class="{ 'btn-primary': range === '24h' }" @click="range = '24h'">24 h</button>
    </div>
    <p v-if="error && !points.length" class="faint small">{{ error }}</p>
    <p v-else-if="points.length < 2" class="faint small">Collecting: the coordinator keeps a point per report (every 15 s), since it started.</p>
    <div v-for="r in rows" :key="r.key" class="row">
      <span class="label">{{ r.label }}</span>
      <Sparkline :values="r.values" :max="r.max" :height="34" :width="400" fluid :label="`${r.label} used`" />
      <span class="text small tabular">{{ r.text }}</span>
    </div>
  </div>
</template>

<style scoped>
.capacity {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.range {
  display: flex;
  gap: 4px;
  justify-content: flex-end;
}
.row {
  display: grid;
  grid-template-columns: 70px minmax(160px, 1fr) minmax(200px, auto);
  align-items: center;
  gap: 12px;
}
.label {
  font-size: 12px;
  color: var(--text-muted);
}
.text {
  color: var(--text-muted);
  white-space: nowrap;
}
</style>
