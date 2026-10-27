<script setup lang="ts">
// One resource of a machine in one chart, on the scale of all it has: above,
// the last hour of what is used (line, area) and what may be used at most
// (dashed: what a node offers the pool; above it, hatched, the part its owner
// keeps); below, now, as a bar: what is used, what a lane would take on top
// (its need, hatched; red where it does not fit) and the same limit.
// Machines compare at a glance.
import { computed, useId } from 'vue';

const props = withDefaults(
  defineProps<{
    /** What the machine has in all: the scale. */
    total: number;
    /** What is used now. */
    used: number;
    /** The most that may be used now (used plus offered); absent: all of it. */
    limit?: number;
    /** What the lane would take on top. */
    need?: number;
    /** What was used over time, oldest first; absent or one point: only now. */
    history?: number[];
    /** The limit over time, as many points as `history`. */
    ceiling?: number[];
    label?: string;
    width?: number;
  }>(),
  { limit: undefined, need: undefined, history: () => [], ceiling: undefined, label: undefined, width: 150 },
);

const id = useId();
/** The history's height, the gap, the bar's height. */
const HIST = 20;
const GAP = 3;
const BAR = 7;
const HEIGHT = HIST + GAP + BAR;

const g = computed(() => {
  const w = props.width;
  // A node that does not report what it has (an older one): the scale is what it offers.
  const total = props.total > 0 ? props.total : Math.max(props.limit ?? 0, props.used + (props.need ?? 0), 1e-9);
  const clamp = (v: number) => Math.max(0, Math.min(v, total)) / total;
  const y = (v: number) => HIST - clamp(v) * HIST;
  const x = (v: number) => clamp(v) * w;
  const hist = props.history.length > 1 ? props.history : [props.used, props.used];
  const step = w / (hist.length - 1);
  const pts = (vs: number[]) => vs.map((v, i) => `${i ? 'L' : 'M'}${(i * step).toFixed(1)} ${y(v).toFixed(1)}`).join(' ');
  const line = pts(hist);
  const ceil = props.ceiling && props.ceiling.length === hist.length ? props.ceiling : props.limit !== undefined ? hist.map(() => props.limit as number) : null;
  const limitLine = ceil ? pts(ceil) : null;
  const limit = Math.min(props.limit ?? total, total);
  const need = props.need ?? 0;
  const share = limit > 0 ? props.used / limit : 1;
  const usedX = x(props.used);
  return {
    line,
    area: `${line} L${w} ${HIST} L0 ${HIST} Z`,
    limitLine,
    kept: limitLine ? `${limitLine} L${w} 0 L0 0 Z` : null,
    usedX,
    needW: need > 0 ? Math.max(2, x(props.used + need) - usedX) : 0,
    limitX: x(limit),
    over: need > 0 && props.used + need > limit,
    tone: share > 0.85 ? 'var(--danger)' : share > 0.6 ? 'var(--warn)' : 'var(--ok)',
  };
});
</script>

<template>
  <svg class="load-chart" :width="width" :height="HEIGHT" :viewBox="`0 0 ${width} ${HEIGHT}`" role="img" :aria-label="label">
    <defs>
      <pattern :id="`${id}-kept`" width="4" height="4" patternUnits="userSpaceOnUse" patternTransform="rotate(45)">
        <line x1="0" y1="0" x2="0" y2="4" stroke="var(--border-strong)" stroke-width="1" />
      </pattern>
      <pattern :id="`${id}-need`" width="4" height="4" patternUnits="userSpaceOnUse" patternTransform="rotate(-45)">
        <rect width="4" height="4" :fill="g.over ? 'var(--danger)' : 'var(--accent)'" fill-opacity="0.3" />
        <line x1="0" y1="0" x2="0" y2="4" :stroke="g.over ? 'var(--danger)' : 'var(--accent)'" stroke-width="2" />
      </pattern>
      <clipPath :id="`${id}-hist`"><rect :width="width" :height="HIST" rx="3" /></clipPath>
      <clipPath :id="`${id}-bar`"><rect :y="HIST + GAP" :width="width" :height="BAR" rx="2" /></clipPath>
    </defs>
    <!-- The last hour. -->
    <g :clip-path="`url(#${id}-hist)`">
      <rect :width="width" :height="HIST" fill="var(--surface-hover)" />
      <path v-if="g.kept" :d="g.kept" :fill="`url(#${id}-kept)`" />
      <path :d="g.area" :fill="g.tone" fill-opacity="0.22" />
      <path :d="g.line" fill="none" :stroke="g.tone" stroke-width="1.5" stroke-linejoin="round" />
      <path v-if="g.limitLine" :d="g.limitLine" fill="none" stroke="var(--text-muted)" stroke-width="1" stroke-dasharray="3 2" />
    </g>
    <!-- Now, with the lane on top. -->
    <g :clip-path="`url(#${id}-bar)`">
      <rect :y="HIST + GAP" :width="width" :height="BAR" fill="var(--surface-hover)" />
      <rect v-if="g.limitX < width" :x="g.limitX" :y="HIST + GAP" :width="width - g.limitX" :height="BAR" :fill="`url(#${id}-kept)`" />
      <rect :y="HIST + GAP" :width="g.usedX" :height="BAR" :fill="g.tone" />
      <rect v-if="g.needW" :x="g.usedX" :y="HIST + GAP" :width="g.needW" :height="BAR" :fill="`url(#${id}-need)`" />
    </g>
    <line v-if="g.limitX < width" :x1="g.limitX" :x2="g.limitX" :y1="HIST + GAP - 1" :y2="HEIGHT" stroke="var(--text-muted)" stroke-width="1.5" />
  </svg>
</template>

<style scoped>
.load-chart {
  display: block;
  flex: none;
  overflow: visible;
}
</style>
