<script setup lang="ts">
// Marks a button whose confirmation Shift-click skips (ui.confirm, `skippable`).
// Only for a dialog that is skippable: not on one marked `skippable: false`.
// Hidden from screen readers; the button's own title or label says it.
import { SHIFT_SKIP_HINT } from '@/stores/ui';
import Icon from './icons/Icon.vue';

defineProps<{
  /** A small badge in the button's bottom-right corner, for an icon-only button (which needs position: relative). */
  corner?: boolean;
}>();
</script>

<template>
  <span class="shift-hint" :class="{ corner }" :title="SHIFT_SKIP_HINT" aria-hidden="true" data-testid="shift-hint">
    <Icon name="shift" :size="corner ? 9 : 11" :stroke-width="2.25" />
  </span>
</template>

<style scoped>
.shift-hint {
  display: inline-flex;
  align-items: center;
  margin-left: 2px;
  color: currentColor;
  opacity: 0.55;
  transition: opacity 0.12s;
}
:is(button, .btn):hover:not(:disabled) > .shift-hint {
  opacity: 0.9;
}
.shift-hint.corner {
  position: absolute;
  right: 0;
  bottom: 0;
  margin: 0;
  pointer-events: auto;
}
</style>
