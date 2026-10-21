<script setup lang="ts">
// Arranging the path menu's programs (ADR-0046): their order, and which it
// lists. In this browser only; a change applies at once.
import { computed, ref } from 'vue';
import type { Opener } from '@/api/types';
import { arrange, useOpenerPrefs } from '@/lib/openers';
import Icon from './icons/Icon.vue';
import Modal from './Modal.vue';

const props = defineProps<{ open: boolean; openers: Opener[] }>();
defineEmits<{ close: [] }>();

const { prefs, save } = useOpenerPrefs();
const list = computed(() => arrange(props.openers, prefs.value));
const kindLabel = { files: 'files', terminal: 'terminal', editor: 'editor', git: 'Git client', agent: 'agent' } as const;
const kindIcon = { files: 'folder', terminal: 'terminal', editor: 'file', git: 'gitBranch', agent: 'zap' } as const;

function move(from: number, to: number) {
  if (to < 0 || to >= list.value.length || from === to) return;
  const ids = list.value.map((o) => o.id);
  const [id] = ids.splice(from, 1);
  ids.splice(to, 0, id);
  save({ ...prefs.value, order: ids });
}
function toggle(o: Opener) {
  const hidden = prefs.value.hidden.includes(o.id) ? prefs.value.hidden.filter((h) => h !== o.id) : [...prefs.value.hidden, o.id];
  save({ ...prefs.value, hidden });
}
function reset() {
  save({ order: [], hidden: [] });
}

// Dragging a row by its handle onto another's place.
const dragging = ref<number | null>(null);
const over = ref<number | null>(null);
function drop(i: number) {
  if (dragging.value !== null) move(dragging.value, i);
  dragging.value = over.value = null;
}
</script>

<template>
  <Modal :open="open" title="Programs in the path menu" size="sm" testid="openers-dialog" @close="$emit('close')">
    <p class="muted small intro">Drag them, or move them with the arrows; the ones unchecked are not listed. Kept in this browser.</p>
    <ul class="list">
      <li
        v-for="(o, i) in list"
        :key="o.id"
        class="row"
        :class="{ off: prefs.hidden.includes(o.id), over: over === i && dragging !== i, dragged: dragging === i }"
        data-testid="opener-row"
        :data-id="o.id"
        @dragover.prevent="over = i"
        @dragleave="over === i && (over = null)"
        @drop.prevent="drop(i)"
      >
        <span class="handle" draggable="true" title="Drag to move" @dragstart="dragging = i" @dragend="dragging = over = null">
          <Icon name="arrowUpDown" :size="13" />
        </span>
        <label class="grow pick">
          <input type="checkbox" :checked="!prefs.hidden.includes(o.id)" data-testid="opener-shown" @change="toggle(o)" />
          <Icon :name="kindIcon[o.kind] ?? 'external'" :size="14" class="muted" />
          <span class="grow truncate">{{ o.label }}</span>
          <span class="faint small">{{ kindLabel[o.kind] ?? o.kind }}</span>
        </label>
        <button type="button" class="btn btn-ghost btn-xs btn-icon" :disabled="i === 0" :aria-label="`Move ${o.label} up`" data-testid="opener-up" @click="move(i, i - 1)">
          <Icon name="arrowUp" :size="13" />
        </button>
        <button type="button" class="btn btn-ghost btn-xs btn-icon" :disabled="i === list.length - 1" :aria-label="`Move ${o.label} down`" data-testid="opener-down" @click="move(i, i + 1)">
          <Icon name="arrowDown" :size="13" />
        </button>
      </li>
    </ul>
    <template #footer>
      <button type="button" class="btn btn-sm" :disabled="!prefs.order.length && !prefs.hidden.length" data-testid="openers-reset" @click="reset">Reset</button>
      <button type="button" class="btn btn-sm btn-primary" @click="$emit('close')">Done</button>
    </template>
  </Modal>
</template>

<style scoped>
.intro {
  margin: 0 0 10px;
}
.list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.row {
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 2px 4px;
  border: 1px solid transparent;
  border-radius: 8px;
}
.row:hover {
  background: var(--surface-hover);
}
.row.over {
  border-color: var(--accent);
}
.row.dragged {
  opacity: 0.5;
}
.row.off .pick > :not(input) {
  opacity: 0.5;
}
.handle {
  display: inline-flex;
  padding: 4px;
  color: var(--text-faint);
  cursor: grab;
}
.pick {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 5px 4px;
  font-size: 13px;
  cursor: pointer;
  min-width: 0;
}
.grow {
  flex: 1;
  min-width: 0;
}
.small {
  font-size: 12px;
}
</style>
