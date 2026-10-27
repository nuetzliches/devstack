<script setup lang="ts">
// How busy this machine is, beside the nodes of the pool, to choose where a
// lane starts: per resource the chart the nodes show (LoadChart), with what
// the hub measures (`GET /hub/pool/share`). The hub keeps no history of it:
// the chart grows from the samples taken while the page is open.
import { computed, onBeforeUnmount, ref } from 'vue';
import { api, paths } from '@/api';
import type { ShareStatus } from '@/api/types';
import { bytes } from '@/lib/format';
import LoadChart from './LoadChart.vue';

defineProps<{ need?: { cpus: number; memory: number } }>();

/** The hub measures every 15 s; an hour of it. */
const EVERY = 15_000;
const KEEP = 240;
const now = ref<ShareStatus | null>(null);
const samples = ref<{ cpus: number; memory: number }[]>([]);

async function load() {
  try {
    const r = await api().request<ShareStatus>('GET', paths.poolShare);
    if (!r.ok || !r.body || typeof r.body !== 'object' || !r.body.total?.cpus) return;
    now.value = r.body;
    const s = { cpus: r.body.total.cpus - r.body.free.cpus, memory: r.body.total.memory - r.body.free.memory };
    samples.value = [...samples.value, s].slice(-KEEP);
  } catch {
    // The last sample stays.
  }
}
void load();
const timer = setInterval(() => void load(), EVERY);
onBeforeUnmount(() => clearInterval(timer));

/** In use now: everything the machine runs, not only lanes. */
const cpu = computed(() => {
  const total = now.value?.total.cpus ?? 0;
  const free = now.value?.free.cpus ?? 0;
  return { total, free, used: Math.max(0, total - free) };
});
const mem = computed(() => {
  const total = now.value?.total.memory ?? 0;
  const free = now.value?.free.memory ?? 0;
  return { total, free, used: Math.max(0, total - free) };
});
</script>

<template>
  <div v-if="now" class="load" data-testid="local-load">
    <div class="row">
      <span class="k">CPU</span>
      <LoadChart
        :total="cpu.total"
        :used="cpu.used"
        :need="need?.cpus"
        :history="samples.map((s) => s.cpus)"
        label="CPU: what this machine used since the page opened, and now, with the lane on top"
      />
      <span class="v tabular" :title="`${cpu.used.toFixed(1)} in use, ${cpu.total.toFixed(0)} cores in all`">
        <strong>{{ cpu.free.toFixed(1) }}</strong> free of {{ cpu.total.toFixed(0) }}
      </span>
    </div>
    <div class="row">
      <span class="k">Memory</span>
      <LoadChart
        :total="mem.total"
        :used="mem.used"
        :need="need?.memory"
        :history="samples.map((s) => s.memory)"
        label="Memory: what this machine used since the page opened, and now, with the lane on top"
      />
      <span class="v tabular" :title="`${bytes(mem.used)} in use, ${bytes(mem.total)} in all`">
        <strong>{{ bytes(mem.free) }}</strong> free of {{ bytes(mem.total) }}
      </span>
    </div>
    <div class="row meta">everything this machine runs, not only lanes</div>
  </div>
  <p v-else class="meta" data-testid="local-load">Nothing measured here yet.</p>
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
  font-size: 12px;
  margin: 0;
}
</style>
