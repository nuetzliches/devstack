<script setup lang="ts">
// A modal on the native <dialog>: focus trapping, Escape and the top layer
// come from the browser.
import { nextTick, onBeforeUnmount, ref, watch } from 'vue';
import Icon from './icons/Icon.vue';

const props = withDefaults(defineProps<{ open: boolean; title: string; size?: 'sm' | 'md' | 'lg' | 'xl'; testid?: string; tone?: 'default' | 'danger' | 'warn' }>(), {
  size: 'md',
  testid: undefined,
  tone: 'default',
});
const emit = defineEmits<{ close: [] }>();
const el = ref<HTMLDialogElement | null>(null);

watch(
  () => props.open,
  async (o) => {
    await nextTick();
    const d = el.value;
    if (!d) return;
    if (o && !d.open) d.showModal();
    if (!o && d.open) d.close();
  },
  { immediate: true },
);

onBeforeUnmount(() => {
  if (el.value?.open) el.value.close();
});

function onCancel(e: Event) {
  e.preventDefault();
  emit('close');
}

function onClick(e: MouseEvent) {
  // a click on the backdrop (the dialog element itself, outside the panel)
  if (e.target === el.value) emit('close');
}
</script>

<template>
  <dialog
    ref="el"
    class="modal"
    :class="[`size-${size}`, `tone-${tone}`]"
    :aria-label="title"
    :data-testid="testid"
    @cancel="onCancel"
    @click="onClick"
  >
    <div v-if="open" class="panel">
      <header class="modal-header">
        <slot name="icon" />
        <h2 class="grow">{{ title }}</h2>
        <button type="button" class="btn btn-ghost btn-sm btn-icon" aria-label="Close" @click="emit('close')">
          <Icon name="x" />
        </button>
      </header>
      <div class="modal-body">
        <slot />
      </div>
      <footer v-if="$slots.footer" class="modal-footer">
        <slot name="footer" />
      </footer>
    </div>
  </dialog>
</template>

<style scoped>
.modal {
  padding: 0;
  border: 1px solid var(--border);
  border-radius: var(--radius-lg);
  background: var(--bg-elev);
  color: var(--text);
  box-shadow: var(--shadow-lg);
  width: min(560px, calc(100vw - 24px));
  max-height: min(86vh, 900px);
}
.modal.size-sm {
  width: min(440px, calc(100vw - 24px));
}
.modal.size-lg {
  width: min(860px, calc(100vw - 24px));
}
.modal.size-xl {
  width: min(1180px, calc(100vw - 24px));
}
.modal::backdrop {
  background: var(--backdrop);
  backdrop-filter: blur(2px);
}
.modal[open] {
  animation: pop 0.16s var(--ease);
}
@keyframes pop {
  from {
    opacity: 0;
    transform: translateY(6px) scale(0.98);
  }
}
.panel {
  display: flex;
  flex-direction: column;
  max-height: inherit;
}
.modal-header {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  padding: 14px 16px 10px;
}
.tone-danger .modal-header {
  color: var(--danger-text);
}
.tone-warn .modal-header {
  color: var(--warn-text);
}
.modal-header h2 {
  font-size: 16px;
  color: var(--text);
}
.modal-body {
  padding: 4px 16px 16px;
  overflow: auto;
  min-height: 0;
}
.modal-footer {
  display: flex;
  justify-content: flex-end;
  gap: var(--space-2);
  padding: 12px 16px;
  border-top: 1px solid var(--border);
  background: var(--surface-2);
  border-radius: 0 0 var(--radius-lg) var(--radius-lg);
  flex-wrap: wrap;
}
</style>
