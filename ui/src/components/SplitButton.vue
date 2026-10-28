<script setup lang="ts">
// A button with a menu beside it: a click does the usual thing (a warm
// restart), the arrow offers the others (Rebuild, Cold…). The menu closes on
// an outside click and on Escape, as the header's menus do.
import { ref } from 'vue';
import { usePopover } from '@/lib/popover';
import type { MenuItem } from '@/lib/laneActions';
import Icon from './icons/Icon.vue';
import type { IconName } from './icons/icons';

const props = withDefaults(
  defineProps<{
    /** Shown on the main button; none: an icon button. */
    label?: string;
    icon: IconName;
    /** The main button's title and accessible name. */
    title: string;
    items: MenuItem[];
    disabled?: boolean;
    /** The main button only (the menu may still offer something). */
    mainDisabled?: boolean;
    /** A spinner instead of the icon. */
    busy?: boolean;
    /** `ghost`: a row's or a group head's icon button; `normal`: the header's. */
    look?: 'ghost' | 'normal';
    /** The menu's accessible name. */
    menuLabel?: string;
    testid?: string;
  }>(),
  { label: undefined, disabled: false, mainDisabled: false, busy: false, look: 'normal', menuLabel: 'More', testid: 'split' },
);
const emit = defineEmits<{ main: []; pick: [key: string] }>();

const root = ref<HTMLElement | null>(null);
const trigger = ref<HTMLButtonElement | null>(null);
const { open, close } = usePopover(root, trigger);

function pick(i: MenuItem) {
  if (i.disabled) return;
  close();
  emit('pick', i.key);
}
</script>

<template>
  <div ref="root" class="split" :class="look" :data-testid="`${testid}-split`">
    <button
      type="button"
      class="btn btn-sm main"
      :class="[look === 'ghost' ? 'btn-ghost' : '', label ? '' : 'btn-icon']"
      :disabled="disabled || mainDisabled"
      :title="title"
      :aria-label="title"
      :data-testid="testid"
      @click="emit('main')"
    >
      <span v-if="busy" class="spinner" /><Icon v-else :name="icon" :size="label ? 14 : 16" />
      <template v-if="label">{{ label }}</template>
    </button>
    <button
      ref="trigger"
      type="button"
      class="btn btn-sm more"
      :class="look === 'ghost' ? 'btn-ghost' : ''"
      :disabled="disabled || !items.length"
      :aria-expanded="open"
      aria-haspopup="menu"
      :aria-label="menuLabel"
      :title="menuLabel"
      :data-testid="`${testid}-more`"
      @click="open = !open"
    >
      <Icon name="chevronDown" :size="12" />
    </button>
    <div v-if="open" class="menu" role="menu" :aria-label="menuLabel" :data-testid="`${testid}-menu`">
      <button
        v-for="i in items"
        :key="i.key"
        type="button"
        class="opt"
        :class="{ danger: i.danger }"
        role="menuitem"
        :disabled="i.disabled"
        :title="i.title"
        :data-testid="`${testid}-${i.key}`"
        @click="pick(i)"
      >
        <Icon v-if="i.icon" :name="i.icon" :size="14" class="muted" />
        <span class="grow">{{ i.label }}</span>
        <span v-if="i.hint" class="faint small">{{ i.hint }}</span>
      </button>
    </div>
  </div>
</template>

<style scoped>
.split {
  position: relative;
  display: inline-flex;
  flex: none;
}
.split.normal .main {
  border-top-right-radius: 0;
  border-bottom-right-radius: 0;
}
.split.normal .more {
  border-top-left-radius: 0;
  border-bottom-left-radius: 0;
  border-left: 0;
  padding: 0 6px;
}
.split.ghost .more {
  width: 16px;
  padding: 0;
  min-width: 0;
}
.menu {
  position: absolute;
  top: calc(100% + 6px);
  right: 0;
  z-index: 40;
  min-width: 240px;
  background: var(--bg-elev);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  box-shadow: var(--shadow-lg);
  padding: 6px;
  display: flex;
  flex-direction: column;
}
.opt {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 7px 8px;
  border: 0;
  border-radius: 8px;
  background: transparent;
  color: inherit;
  font: inherit;
  font-size: 13px;
  text-align: left;
  cursor: pointer;
  white-space: nowrap;
}
.opt:hover:not(:disabled) {
  background: var(--surface-hover);
}
.opt:disabled {
  opacity: 0.5;
  cursor: default;
}
.opt.danger {
  color: var(--danger-text, var(--danger));
}
.grow {
  flex: 1;
  min-width: 0;
}
</style>
