<script setup lang="ts">
// A tiny inline chart: line + soft area, no axes. `max` fixes the scale
// (otherwise it follows the data with a minimum so flat noise stays flat).
import { computed, useId } from 'vue';

const props = withDefaults(
  defineProps<{
    values: number[];
    width?: number;
    height?: number;
    max?: number;
    minMax?: number;
    color?: string;
    label?: string;
    fluid?: boolean;
    /** false: the scale starts near the lowest value (memory), so small changes show. */
    zeroBased?: boolean;
  }>(),
  { width: 120, height: 28, max: undefined, minMax: 1, color: 'var(--accent)', label: undefined, fluid: false, zeroBased: true },
);

const gid = useId();

const geom = computed(() => {
  const v = props.values;
  const w = props.width;
  const h = props.height;
  if (v.length < 2) return null;
  const hi = Math.max(...v);
  const lo = props.zeroBased ? 0 : Math.max(0, Math.min(...v) - (hi - Math.min(...v)) * 0.5 - hi * 0.02);
  const top = props.max ?? Math.max(props.minMax + lo, hi * 1.05 + (props.zeroBased ? hi * 0.05 : 0));
  const span = Math.max(1e-9, top - lo);
  const step = w / (v.length - 1);
  const pts = v.map((y, i) => [i * step, h - 1.5 - ((Math.min(y, top) - lo) / span) * (h - 3)] as const);
  const line = pts.map(([x, y], i) => `${i ? 'L' : 'M'}${x.toFixed(1)} ${y.toFixed(1)}`).join(' ');
  const area = `${line} L${w} ${h} L0 ${h} Z`;
  const last = pts[pts.length - 1];
  return { line, area, last };
});
</script>

<template>
  <svg
    class="spark"
    :class="{ fluid }"
    :width="fluid ? undefined : width"
    :height="height"
    :viewBox="`0 0 ${width} ${height}`"
    preserveAspectRatio="none"
    :role="label ? 'img' : undefined"
    :aria-label="label"
    :aria-hidden="label ? undefined : 'true'"
  >
    <defs>
      <linearGradient :id="gid" x1="0" x2="0" y1="0" y2="1">
        <stop offset="0" :stop-color="color" stop-opacity="0.28" />
        <stop offset="1" :stop-color="color" stop-opacity="0" />
      </linearGradient>
    </defs>
    <template v-if="geom">
      <path :d="geom.area" :fill="`url(#${gid})`" stroke="none" />
      <path :d="geom.line" fill="none" :stroke="color" stroke-width="1.5" vector-effect="non-scaling-stroke" stroke-linejoin="round" />
    </template>
    <line v-else x1="0" :x2="width" :y1="height - 1.5" :y2="height - 1.5" stroke="var(--border-strong)" stroke-dasharray="2 3" />
  </svg>
</template>

<style scoped>
.spark {
  display: block;
  overflow: visible;
}
.spark.fluid {
  width: 100%;
}
</style>
