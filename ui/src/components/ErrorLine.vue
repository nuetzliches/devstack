<script setup lang="ts">
// An error or warning in one line: the first line, cut with "…"; the whole
// text in its tooltip, and a button that copies it (a stack trace belongs
// in an issue or a search, not across the page).
import { computed } from 'vue';
import { useUi } from '@/stores/ui';
import Icon from './icons/Icon.vue';

const props = withDefaults(defineProps<{ text: string; tone?: 'danger' | 'warn' }>(), { tone: 'warn' });
const ui = useUi();

const first = computed(() => props.text.split('\n').find((l) => l.trim()) ?? props.text);

async function copy() {
  try {
    await navigator.clipboard.writeText(props.text);
    ui.toast('success', 'Copied');
  } catch {
    ui.toast('error', 'Copying failed', 'The browser did not allow it.');
  }
}
</script>

<template>
  <div class="err-line" :class="tone" data-testid="error-line">
    <span class="txt" :title="text">{{ first }}</span>
    <button type="button" class="btn btn-ghost btn-sm btn-icon" title="Copy the whole message" aria-label="Copy the whole message" data-testid="error-copy" @click.stop.prevent="copy">
      <Icon name="copy" :size="13" />
    </button>
  </div>
</template>
