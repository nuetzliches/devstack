<script setup lang="ts">
// The confirm dialog (428) and the problem panel (409), driven by the ui store.
import { computed } from 'vue';
import { useUi } from '@/stores/ui';
import Modal from './Modal.vue';
import Diagnostics from './Diagnostics.vue';
import Icon from './icons/Icon.vue';

const ui = useUi();
const req = computed(() => ui.confirmRequest);
const problem = computed(() => ui.problem);

async function copy(text: string) {
  try {
    await navigator.clipboard.writeText(text);
    ui.toast('info', 'Copied to the clipboard');
  } catch {
    ui.toast('error', 'Copy failed');
  }
}
</script>

<template>
  <Modal
    :open="!!req"
    :title="req?.title ?? ''"
    size="sm"
    :tone="req?.danger ? 'danger' : 'default'"
    testid="confirm-dialog"
    @close="req?.resolve(false)"
  >
    <template #icon>
      <Icon :name="req?.danger ? 'alert' : 'info'" :size="20" />
    </template>
    <p class="confirm-text" data-testid="confirm-text">{{ req?.text }}</p>
    <label v-if="req?.option" class="confirm-option" data-testid="confirm-option">
      <input v-model="req.option.checked" type="checkbox" />
      <span>
        {{ req.option.label }}
        <span v-if="req.option.hint" class="faint small">{{ req.option.hint }}</span>
      </span>
    </label>
    <template #footer>
      <button type="button" class="btn" data-testid="confirm-cancel" @click="req?.resolve(false)">Cancel</button>
      <button
        type="button"
        class="btn"
        :class="req?.danger ? 'btn-danger' : 'btn-primary'"
        data-testid="confirm-accept"
        autofocus
        @click="req?.resolve(true)"
      >
        {{ req?.confirmLabel }}
      </button>
    </template>
  </Modal>

  <Modal :open="!!problem" :title="problem?.title ?? ''" size="lg" tone="danger" testid="problem-dialog" @close="ui.problem = null">
    <template #icon>
      <Icon name="xCircle" :size="20" />
    </template>
    <div class="stack">
      <p v-if="problem?.hint" class="muted">{{ problem.hint }}</p>
      <Diagnostics v-if="problem" :text="problem.message" max-height="55vh" />
    </div>
    <template #footer>
      <button v-if="problem" type="button" class="btn" @click="copy(problem.message)"><Icon name="copy" /> Copy</button>
      <button type="button" class="btn btn-primary" autofocus @click="ui.problem = null">Close</button>
    </template>
  </Modal>
</template>

<style scoped>
.confirm-text {
  font-size: 14px;
  line-height: 1.55;
  white-space: pre-wrap;
}
.confirm-option {
  display: flex;
  gap: 8px;
  align-items: flex-start;
  margin-top: 12px;
  font-size: 14px;
  cursor: pointer;
}
.confirm-option input {
  margin-top: 3px;
}
.confirm-option .faint {
  display: block;
}
</style>
