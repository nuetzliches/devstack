<script setup lang="ts">
import { computed } from 'vue';
import { icons, type IconName } from './icons';

const props = withDefaults(defineProps<{ name: IconName; size?: number; strokeWidth?: number; label?: string }>(), {
  size: 16,
  strokeWidth: 2,
  label: undefined,
});
// Static markup from icons.ts, never user input.
const inner = computed(() => icons[props.name]);
</script>

<template>
  <svg
    :width="size"
    :height="size"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    :stroke-width="strokeWidth"
    stroke-linecap="round"
    stroke-linejoin="round"
    :role="label ? 'img' : undefined"
    :aria-label="label"
    :aria-hidden="label ? undefined : 'true'"
    focusable="false"
    style="flex: none"
    v-html="inner"
  />
</template>
