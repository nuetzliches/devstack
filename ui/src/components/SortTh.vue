<script setup lang="ts">
// A sortable column header (see lib/sort.ts). Classes such as `num` or
// `hide-sm` go on the <th> as usual.
import { computed } from 'vue';
import type { SortDir } from '@/lib/sort';
import Icon from './icons/Icon.vue';

const props = defineProps<{ dir: SortDir | null; label: string }>();
const emit = defineEmits<{ toggle: [] }>();
const aria = computed(() => (props.dir === 'asc' ? 'ascending' : props.dir === 'desc' ? 'descending' : 'none'));
const title = computed(() =>
  props.dir === null ? `Sort by ${props.label}` : props.dir === 'asc' ? `Sorted by ${props.label}, ascending` : `Sorted by ${props.label}, descending`,
);
</script>

<template>
  <th :aria-sort="aria" class="sort-th" :class="{ sorted: dir !== null }">
    <button type="button" class="sort-btn" :title="title" @click="emit('toggle')">
      <span>{{ label }}</span>
      <Icon :name="dir === 'asc' ? 'arrowUp' : dir === 'desc' ? 'arrowDown' : 'arrowUpDown'" :size="12" class="sort-icon" />
    </button>
  </th>
</template>

<style scoped>
.sort-btn {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 0;
  border: 0;
  background: none;
  color: inherit;
  font: inherit;
  letter-spacing: inherit;
  text-transform: inherit;
  cursor: pointer;
}
.num .sort-btn {
  flex-direction: row-reverse;
}
.sort-icon {
  opacity: 0;
  transition: opacity 0.12s;
}
.sort-btn:hover .sort-icon,
.sort-btn:focus-visible .sort-icon {
  opacity: 0.55;
}
.sorted {
  color: var(--text);
}
.sorted .sort-icon {
  opacity: 1;
}
</style>
