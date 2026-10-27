<script setup lang="ts">
// A prompt for a coding agent, written by the hub (ADR-0046, amendment
// 2026-10-22): copied, shown, or handed to an agent the hub starts in the
// right folder. The page names the prompt and what it is about, never text.
// `compact` fits it into a one-line banner: one button for the first agent,
// the others, the copy and the text in its menu.
import { computed, ref, watch } from 'vue';
import { api, paths } from '@/api';
import type { Opener, PromptKind, PromptTarget } from '@/api/types';
import { act } from '@/lib/actions';
import { useOpenerPrefs } from '@/lib/openers';
import { usePopover } from '@/lib/popover';
import { useHub } from '@/stores/hub';
import { useUi } from '@/stores/ui';
import Icon from './icons/Icon.vue';
import Modal from './Modal.vue';

const props = defineProps<{ kind: PromptKind; target: PromptTarget; compact?: boolean; label?: string }>();
const hub = useHub();
const ui = useUi();
const prefs = useOpenerPrefs();

const text = ref<string | null>(null);
const problem = ref<string | null>(null);
const starting = ref<string | null>(null);
const showing = ref(false);
const root = ref<HTMLElement | null>(null);
const trigger = ref<HTMLButtonElement | null>(null);
const menu = usePopover(root, trigger);

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
  menu.close();
  if (!text.value) return;
  try {
    await navigator.clipboard.writeText(text.value);
    ui.toast('success', 'Prompt copied', 'Paste it into your coding agent, in any folder.');
  } catch {
    ui.toast('warning', 'Copy failed', 'Open "Show the prompt" and copy it yourself.');
  }
}

async function start(o: Opener) {
  menu.close();
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
  <div v-if="compact" ref="root" class="agent-prompt compact" data-testid="agent-prompt" :data-kind="kind">
    <div class="split">
      <button
        type="button"
        class="btn btn-sm btn-primary"
        :disabled="!text || !!starting"
        :title="problem ? `No prompt: ${problem}` : agents[0] ? `${agents[0].label} works on the prompt; the others, the prompt itself: the arrow` : 'Paste it into your coding agent'"
        :data-testid="agents[0] ? `agent-start-${agents[0].id}` : 'agent-copy-prompt'"
        @click="agents[0] ? start(agents[0]) : copy()"
      >
        <span v-if="starting" class="spinner" /><Icon v-else :name="agents[0] ? 'zap' : 'copy'" :size="14" />
        {{ agents[0] ? `${label ?? 'Start'} in ${agents[0].label}` : 'Copy prompt' }}
      </button>
      <button ref="trigger" type="button" class="btn btn-sm btn-primary btn-icon" :disabled="!text" :aria-expanded="menu.open.value" aria-haspopup="true" aria-label="More ways" data-testid="agent-prompt-more" @click="menu.toggle">
        <Icon name="chevronDown" :size="14" />
      </button>
    </div>
    <Transition name="drop">
      <div v-if="menu.open.value" class="menu" role="menu" data-testid="agent-prompt-menu">
        <button v-for="o in agents.slice(1)" :key="o.id" type="button" class="item" role="menuitem" :data-testid="`agent-start-${o.id}`" @click="start(o)">
          <Icon name="zap" :size="14" /> {{ o.label }}
        </button>
        <button v-if="agents.length" type="button" class="item" role="menuitem" data-testid="agent-copy-prompt" @click="copy">
          <Icon name="copy" :size="14" /> Copy prompt
        </button>
        <button type="button" class="item" role="menuitem" data-testid="agent-show-prompt" @click="(menu.close(), (showing = true))">
          <Icon name="file" :size="14" /> Show the prompt
        </button>
      </div>
    </Transition>
    <Modal :open="showing" title="The prompt" size="lg" testid="agent-prompt-modal" @close="showing = false">
      <pre class="text" data-testid="agent-prompt-text">{{ text }}</pre>
    </Modal>
  </div>
  <div v-else class="agent-prompt" data-testid="agent-prompt" :data-kind="kind">
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
.compact {
  position: relative;
  margin-top: 0;
  flex: none;
}
.split {
  display: flex;
  gap: 1px;
}
.split > .btn:first-child {
  border-top-right-radius: 0;
  border-bottom-right-radius: 0;
}
.split > .btn:last-child {
  border-top-left-radius: 0;
  border-bottom-left-radius: 0;
}
.menu {
  position: absolute;
  right: 0;
  top: calc(100% + 6px);
  z-index: 40;
  min-width: 200px;
  background: var(--bg-elev);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  box-shadow: var(--shadow-lg);
  padding: 6px;
}
.item {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  border: 0;
  background: transparent;
  color: var(--text);
  padding: 7px 8px;
  border-radius: 8px;
  cursor: pointer;
  text-align: left;
  font-size: 13px;
}
.item:hover,
.item:focus-visible {
  background: var(--surface-hover);
}
.text {
  white-space: pre-wrap;
  font-size: 12px;
  margin: 0;
}
.drop-enter-active,
.drop-leave-active {
  transition:
    opacity 0.12s,
    transform 0.12s var(--ease);
}
.drop-enter-from,
.drop-leave-to {
  opacity: 0;
  transform: translateY(-4px);
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
