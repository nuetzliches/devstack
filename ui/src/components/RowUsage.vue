<script setup lang="ts">
// What a service or container costs now, in its row: memory with its last
// ten minutes as a small graph, and CPU. The usage page has the details.
import { computed } from 'vue';
import { bytes, percent } from '@/lib/format';
import { useStack } from '@/stores/stack';
import Sparkline from './Sparkline.vue';

const props = defineProps<{ seriesKey: string; load: { cpu: number; memory: number } | null }>();
const stack = useStack();
const mem = computed(() => (stack.history.get(props.seriesKey) ?? []).map((p) => p.memory));
</script>

<template>
  <div v-if="load" class="row-usage hide-sm" :title="`CPU ${percent(load.cpu)} of one core · memory ${bytes(load.memory)} (last 10 min in the graph)`" data-testid="row-usage">
    <Sparkline :values="mem" :width="56" :height="18" :min-max="8 * 1024 * 1024" :zero-based="false" color="var(--ok)" />
    <span class="nums tabular">
      <span>{{ bytes(load.memory) }}</span>
      <span class="faint">{{ percent(load.cpu) }}</span>
    </span>
  </div>
  <!-- Its column all the same: the next columns stay where the other rows have them. -->
  <div v-else class="row-usage hide-sm" aria-hidden="true" />
</template>

<style scoped>
.row-usage {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 6px;
  flex: none;
  width: var(--col-usage);
}
.nums {
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  font-size: 11px;
  line-height: 1.2;
  min-width: 52px;
}
</style>
