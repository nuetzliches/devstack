<script setup lang="ts">
import { useUi, type ToastKind } from '@/stores/ui';
import Icon from './icons/Icon.vue';
import type { IconName } from './icons/icons';

const ui = useUi();
const icon: Record<ToastKind, IconName> = { success: 'checkCircle', error: 'xCircle', info: 'info', warning: 'alert' };
</script>

<template>
  <div class="toasts" role="region" aria-label="Notifications" aria-live="polite">
    <TransitionGroup name="toast">
      <div v-for="t in ui.toasts" :key="t.id" class="toast" :class="t.kind" role="status" data-testid="toast">
        <Icon :name="icon[t.kind]" :size="18" class="toast-icon" />
        <div class="grow">
          <div class="toast-title">{{ t.title }}</div>
          <div v-if="t.message" class="toast-msg">{{ t.message }}</div>
        </div>
        <button type="button" class="btn btn-ghost btn-xs btn-icon" aria-label="Dismiss" @click="ui.dismiss(t.id)">
          <Icon name="x" :size="14" />
        </button>
      </div>
    </TransitionGroup>
  </div>
</template>

<style scoped>
.toasts {
  position: fixed;
  right: 16px;
  bottom: 16px;
  z-index: 50;
  display: flex;
  flex-direction: column;
  gap: 8px;
  width: min(380px, calc(100vw - 24px));
  pointer-events: none;
}
@media (max-width: 480px) {
  .toasts {
    right: 12px;
    bottom: 12px;
  }
}
.toast {
  pointer-events: auto;
  display: flex;
  align-items: flex-start;
  gap: 10px;
  padding: 10px 10px 10px 12px;
  border-radius: var(--radius);
  background: var(--bg-elev);
  border: 1px solid var(--border);
  border-left-width: 3px;
  box-shadow: var(--shadow-lg);
  font-size: 13px;
}
.toast-icon {
  margin-top: 1px;
  flex: none;
}
.toast.success {
  border-left-color: var(--ok);
}
.toast.success .toast-icon {
  color: var(--ok);
}
.toast.error {
  border-left-color: var(--danger);
}
.toast.error .toast-icon {
  color: var(--danger);
}
.toast.warning {
  border-left-color: var(--warn);
}
.toast.warning .toast-icon {
  color: var(--warn);
}
.toast.info {
  border-left-color: var(--accent);
}
.toast.info .toast-icon {
  color: var(--accent);
}
.toast-title {
  font-weight: 600;
}
.toast-msg {
  color: var(--text-muted);
  margin-top: 2px;
}
.toast-enter-active,
.toast-leave-active {
  transition: all 0.2s var(--ease);
}
.toast-enter-from,
.toast-leave-to {
  opacity: 0;
  transform: translateY(8px);
}
</style>
