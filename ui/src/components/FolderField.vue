<script setup lang="ts">
// A folder path as one control: the path field, the button that opens the
// operating system's folder dialog through the hub (a page never learns a
// picked folder's absolute path), and the action. The hub answers 501 where
// it has no desktop; then only the field stays.
import { ref } from 'vue';
import { api, paths } from '@/api';
import { useUi } from '@/stores/ui';
import Icon from './icons/Icon.vue';

const path = defineModel<string>({ required: true });
defineProps<{ placeholder: string; busy?: boolean; testid: string }>();
const emit = defineEmits<{ submit: []; picked: []; problem: [message: string] }>();
const ui = useUi();

const canPick = ref(true);
const picking = ref(false);

async function pick() {
  picking.value = true;
  try {
    const res = await api().request<{ path: string | null } | { error: string }>('POST', paths.pickFolder);
    const b = typeof res.body === 'object' ? res.body : null;
    if (res.status === 501) {
      canPick.value = false;
      ui.toast('info', 'No folder dialog on this machine', 'Paste the folder path instead.');
    } else if (res.ok && b && 'path' in b) {
      if (b.path) {
        path.value = b.path;
        emit('picked');
      }
    } else {
      emit('problem', b && 'error' in b ? b.error : `HTTP ${res.status}`);
    }
  } catch {
    emit('problem', 'The hub does not answer.');
  } finally {
    picking.value = false;
  }
}
</script>

<template>
  <form class="field" @submit.prevent="emit('submit')">
    <Icon name="folder" :size="15" class="lead" />
    <input v-model="path" type="text" :placeholder="placeholder" spellcheck="false" autocomplete="off" aria-label="Folder path" :data-testid="`${testid}-path`" />
    <span v-if="busy" class="spinner" />
    <button v-if="canPick" type="button" class="browse" :disabled="picking" title="Choose the folder in a dialog" :data-testid="`${testid}-pick`" @click="pick">
      <span v-if="picking" class="spinner" />
      <template v-else>Browse…</template>
    </button>
    <slot />
  </form>
</template>

<style scoped>
.field {
  display: flex;
  align-items: center;
  height: 38px;
  margin: 12px 0;
  border: 1px solid var(--border-strong);
  border-radius: var(--radius);
  background: var(--surface);
  overflow: hidden;
  transition: border-color 0.12s var(--ease);
}
.field:focus-within {
  border-color: var(--accent);
  box-shadow: var(--focus);
}
.lead {
  flex: none;
  margin: 0 2px 0 11px;
  color: var(--text-faint);
}
input {
  flex: 1;
  min-width: 0;
  height: 100%;
  padding: 0 8px;
  border: 0;
  background: transparent;
  color: inherit;
  font: 13px/1.4 var(--font-mono, ui-monospace, Consolas, monospace);
}
input:focus,
input:focus-visible {
  outline: none;
  box-shadow: none;
}
.field > .spinner {
  flex: none;
  margin-right: 10px;
}
.browse {
  flex: none;
  display: inline-flex;
  align-items: center;
  height: 100%;
  padding: 0 14px;
  border: 0;
  border-left: 1px solid var(--border);
  background: var(--surface-2);
  color: var(--text-2);
  font-weight: 500;
  font-size: 13px;
  cursor: pointer;
}
.browse:hover:not(:disabled) {
  background: var(--surface-hover);
  color: var(--text);
}
.browse:focus-visible,
.field :slotted(button:focus-visible) {
  outline: none;
  box-shadow: inset var(--focus);
}
.field :slotted(.btn) {
  height: 100%;
  border: 0;
  border-left: 1px solid var(--border);
  border-radius: 0;
}
</style>
