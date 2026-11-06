<script setup lang="ts">
// The mark in the top bar. On hover it turns over to the nut of the nuts
// family, and the favicon turns with it; a dot top right says something
// needs attention, red or yellow, as the favicon's does (ADR-0097).
import { attentionLabel, markFlipped, useAttentionTone } from '@/lib/brand';

defineProps<{ size?: number }>();
const tone = useAttentionTone();
</script>

<template>
  <span
    class="mark"
    :style="{ width: `${size ?? 22}px`, height: `${size ?? 22}px` }"
    data-testid="brand-mark"
    @mouseenter="markFlipped = true"
    @mouseleave="markFlipped = false"
  >
    <span class="mark-inner">
      <svg class="face" viewBox="0 0 512 512" aria-hidden="true">
        <rect width="512" height="512" rx="112" fill="#0f1115" stroke="var(--border-strong)" stroke-width="16" />
        <g fill="none" stroke-linecap="round" stroke-width="40">
          <path d="M136 176h240" stroke="#5b8cff" />
          <path d="M136 256h168" stroke="#3ecf8e" />
          <path d="M136 336h240" stroke="#5b8cff" opacity=".55" />
        </g>
        <circle cx="364" cy="256" r="32" fill="#3ecf8e" />
      </svg>
      <!-- The nut: the favicon's (lib/brand.ts), drawn a little bolder at this size. -->
      <svg class="face back" viewBox="0 0 512 512" aria-hidden="true">
        <rect width="512" height="512" rx="112" fill="#0f1115" stroke="var(--border-strong)" stroke-width="16" />
        <svg x="84" y="84" width="344" height="344" viewBox="0 0 24 24" fill="none" stroke="#3ecf8e" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round">
          <path d="M16.847 5.847L20 9a7.23 7.23 0 0 1 1.551 7.516C21.241 17.352 21 17.932 21 19v1a1 1 0 0 1-1 1h-1c-1.069 0-1.648.242-2.485.552A7.2 7.2 0 0 1 9.002 20l-3.155-3.153" />
          <path d="M18.21 5.43c-1.71.69-5.07 1.07-6.71 1.07c.46 1.38.91 2.74.61 4.88a.88.88 0 0 1-.73.74c-1.78.28-3.54-.17-4.88-.62c0 1.64-.38 5-1.07 6.71c-.21.52-.82.55-1.17.12A10 10 0 0 1 18.33 4.26c.43.35.4.97-.12 1.17" />
          <path d="M4.93 4.93L3 3a.7.7 0 0 1 0-1" />
        </svg>
      </svg>
    </span>
    <span v-if="tone" class="dot" :class="`dot-${tone}`" :title="attentionLabel(tone)" :data-tone="tone" data-testid="attention-dot" />
    <span v-if="tone" class="sr-only">{{ attentionLabel(tone) }}</span>
  </span>
</template>

<style scoped>
.mark {
  position: relative;
  display: inline-block;
  flex: none;
  perspective: 600px;
}
.mark-inner {
  display: block;
  position: relative;
  width: 100%;
  height: 100%;
  transition: transform 0.55s cubic-bezier(0.4, 0, 0.2, 1);
  transform-style: preserve-3d;
}
.mark:hover .mark-inner {
  transform: rotateY(180deg);
}
.face {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  backface-visibility: hidden;
}
.back {
  transform: rotateY(180deg);
}
.dot {
  position: absolute;
  top: -3px;
  right: -3px;
  width: 9px;
  height: 9px;
  border-radius: 50%;
  border: 2px solid var(--bg-elev);
  box-sizing: content-box;
}
.dot-error {
  background: var(--danger);
}
.dot-warn {
  background: var(--warn);
}
@media (prefers-reduced-motion: reduce) {
  .mark-inner {
    transition: none;
  }
}
</style>
