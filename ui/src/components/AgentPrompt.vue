<script setup lang="ts">
// A prompt for a coding agent, written by the hub (ADR-0046, amendment
// 2026-10-22): copied, shown, or handed to an agent the hub starts in the
// right folder. The page names the prompt and what it is about, never text.
import { computed, ref, watch } from 'vue';
import { api, paths } from '@/api';
import type { Opener, PromptKind, PromptTarget } from '@/api/types';
import { act } from '@/lib/actions';
import { useOpenerPrefs } from '@/lib/openers';
import { useHub } from '@/stores/hub';
import { useUi } from '@/stores/ui';
import Icon from './icons/Icon.vue';

const props = defineProps<{ kind: PromptKind; target: PromptTarget }>();
const hub = useHub();
const ui = useUi();
const prefs = useOpenerPrefs();

const text = ref<string | null>(null);
const problem = ref<string | null>(null);
const starting = ref<string | null>(null);

// Keyed by the request itself: a parent that renders again passes a new
// target object for the same prompt.
watch(
  () => paths.prompt(props.kind, props.target),
  async (asked) => {
    text.value = null;
    problem.value = null;
    try {
      const res = await api().request<{ text: string } | { error: string }>('GET', asked);
      if (asked !== paths.prompt(props.kind, props.target)) return;
      const b = typeof res.body === 'object' ? res.body : null;
      if (res.ok && b && 'text' in b) text.value = b.text;
      else problem.value = b && 'error' in b ? b.error : `HTTP ${res.status}`;
    } catch {
      problem.value = 'The hub does not answer.';
    }
  },
  { immediate: true },
);
void hub.loadOpeners();

/** The agents that take a prompt, as the path menu arranges them. */
const agents = computed(() => prefs.shown(hub.openers ?? []).filter((o) => o.prompt));

async function copy() {
  if (!text.value) return;
  try {
    await navigator.clipboard.writeText(text.value);
    ui.toast('success', 'Prompt copied', 'Paste it into your coding agent, in any folder.');
  } catch {
    ui.toast('warning', 'Copy failed', 'Open "Show the prompt" and copy it yourself.');
  }
}

async function start(o: Opener) {
  starting.value = o.id;
  try {
    await act({
      label: `Start ${o.label}`,
      path: paths.agent,
      body: { kind: props.kind, ...props.target, with: o.id },
      success: o.prompt === 'send' ? `${o.label} works on it` : `${o.label} opens with the prompt: send it there`,
      ...(o.prompt === 'send'
        ? { confirmText: `${o.label} starts an agent that begins working on the prompt at once. Read it first under "Show the prompt".`, confirmLabel: 'Start', danger: false }
        : {}),
    });
  } finally {
    starting.value = null;
  }
}
</script>

<template>
  <div class="agent-prompt" data-testid="agent-prompt" :data-kind="kind">
    <div class="row">
      <button
        v-for="(o, i) in agents"
        :key="o.id"
        type="button"
        class="btn btn-sm"
        :class="{ 'btn-primary': i === 0 }"
        :disabled="!text || !!starting"
        :title="o.prompt === 'send' ? `${o.label} starts on the prompt at once` : `${o.label} opens in the folder with the prompt filled in; you send it`"
        :data-testid="`agent-start-${o.id}`"
        @click="start(o)"
      >
        <span v-if="starting === o.id" class="spinner" /><Icon v-else name="zap" :size="14" /> {{ o.label }}
      </button>
      <button type="button" class="btn btn-sm" :class="{ 'btn-primary': !agents.length }" :disabled="!text" data-testid="agent-copy-prompt" @click="copy">
        <Icon name="copy" :size="14" /> Copy prompt
      </button>
    </div>
    <p v-if="problem" class="faint small">No prompt: {{ problem }}</p>
    <details v-else-if="text" class="show">
      <summary>Show the prompt</summary>
      <pre data-testid="agent-prompt-text">{{ text }}</pre>
    </details>
  </div>
</template>

<style scoped>
.agent-prompt {
  margin-top: 8px;
}
.row {
  display: flex;
  gap: 6px;
  flex-wrap: wrap;
}
.show {
  margin-top: 6px;
}
.show pre {
  white-space: pre-wrap;
  font-size: 12px;
  max-height: 240px;
  overflow: auto;
  border: 1px solid var(--border);
  border-radius: 8px;
  padding: 10px;
  margin: 6px 0 0;
}
</style>
