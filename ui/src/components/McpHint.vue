<script setup lang="ts">
// No agent configuration registers `devstack mcp` for the stack's repository:
// a small label in the stack's head, its menu the hub's prompt for a coding
// agent that adds the entry (the agent knows its own file's format and keeps
// what is there). "Not for this stack" hides it on this browser.
import { computed, ref, watch } from 'vue';
import { persist } from '@/lib/persist';
import { usePopover } from '@/lib/popover';
import { useHub } from '@/stores/hub';
import { useStack } from '@/stores/stack';
import AgentPrompt from './AgentPrompt.vue';
import Icon from './icons/Icon.vue';

const hub = useHub();
const stack = useStack();

// A new devstack or an edited stack file: an agent may have added it since.
watch(
  () => [hub.version, stack.view?.config_changed] as const,
  () => void stack.loadMcp(),
);

const dismissed = ref<Record<string, boolean>>({});
const key = (id: string) => `mcp-hint.${id}`;
const hidden = computed(() => {
  const id = stack.id;
  if (!id) return true;
  return dismissed.value[id] ?? persist.get<boolean>(key(id)) ?? false;
});
const shown = computed(() => stack.mcp?.registered === false && !hidden.value && !stack.readOnly && !stack.sharedLane);

const root = ref<HTMLElement | null>(null);
const trigger = ref<HTMLButtonElement | null>(null);
const { open, close } = usePopover(root, trigger);
function toggle() {
  open.value = !open.value;
  // Back from the agent: ask again.
  if (open.value) void stack.loadMcp();
}
function dismiss() {
  const id = stack.id;
  if (!id) return;
  close();
  persist.set(key(id), true);
  dismissed.value = { ...dismissed.value, [id]: true };
}
</script>

<template>
  <div v-if="shown" ref="root" class="picker" data-testid="mcp-hint">
    <button
      ref="trigger"
      type="button"
      class="pill hint"
      :aria-expanded="open"
      title="No agent configuration registers devstack's MCP server for this repository"
      data-testid="mcp-hint-open"
      @click="toggle"
    >
      <Icon name="zap" :size="12" /> no MCP
    </button>
    <div v-if="open" class="menu" role="dialog" aria-label="devstack's MCP server" data-testid="mcp-hint-menu">
      <p>
        <strong>Coding agents do not find devstack's MCP server here.</strong>
        With it they read the stack's status and logs and restart services through tools, not the CLI: an entry such as
        <code>{ "devstack": { "command": "devstack", "args": ["mcp"] } }</code> in <code>.mcp.json</code> (or VS Code's, Cursor's, Codex's file).
      </p>
      <p class="muted">A coding agent adds it next to what is there, for the agents you use:</p>
      <AgentPrompt v-if="stack.id" kind="mcp" :target="{ stack: stack.id }" />
      <div class="foot">
        <button type="button" class="link-btn" data-testid="mcp-hint-dismiss" @click="dismiss">Not for this stack</button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.picker {
  position: relative;
}
.hint {
  cursor: pointer;
  border: 1px dashed var(--border-strong);
  background: transparent;
  color: var(--text-muted);
  font: inherit;
  font-size: 12px;
  display: inline-flex;
  align-items: center;
  gap: 4px;
}
.hint:hover,
.hint[aria-expanded='true'] {
  color: var(--text);
  border-color: var(--accent);
}
.menu {
  position: absolute;
  top: calc(100% + 6px);
  left: 0;
  z-index: 40;
  width: min(420px, 90vw);
  background: var(--bg-elev);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  box-shadow: var(--shadow-lg);
  padding: 12px 14px;
  font-size: 13px;
}
.menu p {
  margin: 0 0 8px;
}
.menu code {
  word-break: break-all;
}
.foot {
  margin-top: 10px;
  padding-top: 8px;
  border-top: 1px solid var(--border);
}
.link-btn {
  border: 0;
  background: none;
  padding: 0;
  color: var(--accent);
  font: inherit;
  font-size: 12px;
  cursor: pointer;
}
.link-btn:hover {
  text-decoration: underline;
}
</style>
