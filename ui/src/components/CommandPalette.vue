<script setup lang="ts">
// Ctrl+K / Cmd+K: go anywhere and run anything of the current stack by typing
// (plan §6): views, stacks, services, groups, containers, the stack file's
// commands. Actions that change something go through the same calls as the
// buttons, a command with a confirmation text still asks.
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { useRouter } from 'vue-router';
import { useHub } from '@/stores/hub';
import { useStack } from '@/stores/stack';
import { useUi } from '@/stores/ui';
import Icon from './icons/Icon.vue';
import type { IconName } from './icons/icons';

const hub = useHub();
const stack = useStack();
const ui = useUi();
const router = useRouter();

const open = ref(false);
const query = ref('');
const active = ref(0);
const input = ref<HTMLInputElement | null>(null);
const dialog = ref<HTMLDialogElement | null>(null);

type Item = { id: string; label: string; hint: string; icon: IconName; run: () => unknown };

function go(name: string, params: Record<string, string> = {}) {
  if (!stack.id) return;
  void router.push({ name, params: { stack: stack.id, ...params } });
}

const items = computed<Item[]>(() => {
  const out: Item[] = [];
  const v = stack.view;
  const sid = stack.id;
  if (sid) {
    for (const [name, label, icon] of [
      ['overview', 'Overview', 'grid'],
      ['infra', 'Infrastructure', 'box'],
      ['commands', 'Commands', 'terminal'],
      ['usage', 'Usage', 'gauge'],
      ['events', 'Activity', 'activity'],
    ] as [string, string, IconName][])
      out.push({ id: `view:${name}`, label, hint: 'view', icon, run: () => go(name) });
  }
  out.push({ id: 'ui:sidebar', label: `${ui.sidebarShown ? 'Hide' : 'Show'} sidebar`, hint: 'layout · Ctrl+B', icon: 'panelLeft', run: () => ui.toggleSidebar() });
  for (const s of hub.stacks)
    if (s.id !== sid) out.push({ id: `stack:${s.id}`, label: s.name, hint: `stack${s.running ? '' : ', stopped'}`, icon: 'layers', run: () => router.push({ name: 'overview', params: { stack: s.id } }) });
  // Someone else's lane (ADR-0040) is viewed: nothing that would change it is offered.
  const changes = !stack.readOnly;
  if (sid && stack.entry && changes) {
    if (stack.running) {
      out.push({ id: 'stack:restart', label: `Restart stack ${stack.entry.name}`, hint: 'stack', icon: 'restart', run: () => stack.restartStack() });
      out.push({ id: 'stack:stop', label: `Stop stack ${stack.entry.name}`, hint: 'stack', icon: 'stop', run: () => stack.stopStack() });
    } else out.push({ id: 'stack:start', label: `Start stack ${stack.entry.name}`, hint: 'stack', icon: 'play', run: () => stack.startStack() });
  }
  if (v && stack.running) {
    for (const s of v.services) {
      out.push({ id: `svc:${s.name}`, label: s.name, hint: 'service', icon: 'server', run: () => go('service', { name: s.name }) });
      if (changes) out.push({ id: `svc-restart:${s.name}`, label: `Restart ${s.name}`, hint: 'service, rebuild', icon: 'restart', run: () => stack.serviceAction(s.name, 'restart') });
    }
  }
  if (v && stack.running && changes) {
    for (const g of v.groups) {
      out.push({ id: `grp-restart:${g.name}`, label: `Restart group ${g.label}`, hint: 'group', icon: 'restart', run: () => stack.groupAction(g.name, g.label, 'restart') });
      out.push({ id: `grp-start:${g.name}`, label: `Start group ${g.label}`, hint: 'group', icon: 'play', run: () => stack.groupAction(g.name, g.label, 'start') });
      out.push({ id: `grp-stop:${g.name}`, label: `Stop group ${g.label}`, hint: 'group', icon: 'stop', run: () => stack.groupAction(g.name, g.label, 'stop') });
    }
    for (const g of v.infra)
      for (const c of g.containers.filter((c) => c.managed !== false))
        out.push({ id: `ctr:${g.group}/${c.service}`, label: `Restart container ${c.service}`, hint: `container · ${g.group}`, icon: 'box', run: () => stack.restartContainer(g.group, c.service) });
    for (const c of v.commands)
      out.push({
        id: `cmd:${c.name}`,
        label: `Run ${c.name}`,
        hint: c.description || 'command',
        icon: 'terminal',
        run: async () => {
          const run = await stack.runCommand(c.name);
          if (run !== null && sid) void router.push({ name: 'commands', params: { stack: sid }, query: { cmd: c.name, run: String(run) } });
        },
      });
  }
  return out;
});

/** Every typed character in order; words starting earlier and closer together rank higher. */
function score(text: string, q: string): number | null {
  if (!q) return 0;
  const t = text.toLowerCase();
  if (t.includes(q)) return 1000 - t.indexOf(q) * 2 - t.length / 100;
  let pos = -1;
  let gaps = 0;
  for (const ch of q) {
    const i = t.indexOf(ch, pos + 1);
    if (i < 0) return null;
    gaps += i - pos - 1;
    pos = i;
  }
  return 500 - gaps - t.length / 100;
}

const results = computed(() => {
  const q = query.value.trim().toLowerCase();
  return items.value
    .map((it) => ({ it, s: score(`${it.label} ${it.hint}`, q) }))
    .filter((x) => x.s !== null)
    .sort((a, b) => (b.s as number) - (a.s as number))
    .slice(0, 50)
    .map((x) => x.it);
});

watch(results, () => {
  active.value = 0;
});

function show() {
  query.value = '';
  active.value = 0;
  open.value = true;
}
function hide() {
  open.value = false;
}
watch(open, async (o) => {
  const d = dialog.value;
  if (!d) return;
  if (o) {
    if (!d.open) d.showModal();
    await nextTick();
    input.value?.focus();
  } else if (d.open) d.close();
});

async function choose(it: Item | undefined) {
  if (!it) return;
  hide();
  await it.run();
}

function onKey(e: KeyboardEvent) {
  if (e.key === 'ArrowDown') {
    e.preventDefault();
    active.value = Math.min(active.value + 1, results.value.length - 1);
  } else if (e.key === 'ArrowUp') {
    e.preventDefault();
    active.value = Math.max(active.value - 1, 0);
  } else if (e.key === 'Enter') {
    e.preventDefault();
    void choose(results.value[active.value]);
  }
  void nextTick(() => document.querySelector('.palette .opt.active')?.scrollIntoView({ block: 'nearest' }));
}

function onGlobalKey(e: KeyboardEvent) {
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'k') {
    e.preventDefault();
    if (open.value) hide();
    else show();
  }
}
// The top bar's button opens it too.
const onOpenRequest = () => show();
onMounted(() => {
  window.addEventListener('keydown', onGlobalKey);
  window.addEventListener('devstack:palette', onOpenRequest);
});
onBeforeUnmount(() => {
  window.removeEventListener('keydown', onGlobalKey);
  window.removeEventListener('devstack:palette', onOpenRequest);
});

defineExpose({ show });
</script>

<template>
  <dialog ref="dialog" class="palette" aria-label="Command palette" data-testid="command-palette" @close="open = false" @click.self="hide">
    <div class="panel">
      <div class="search">
        <Icon name="search" :size="16" />
        <input
          ref="input"
          v-model="query"
          type="text"
          placeholder="Go to or run…"
          spellcheck="false"
          autocomplete="off"
          role="combobox"
          aria-controls="palette-list"
          :aria-activedescendant="results[active] ? `opt-${active}` : undefined"
          aria-expanded="true"
          data-testid="palette-input"
          @keydown="onKey"
        />
        <kbd>Esc</kbd>
      </div>
      <ul id="palette-list" class="list" role="listbox">
        <li
          v-for="(it, i) in results"
          :id="`opt-${i}`"
          :key="it.id"
          class="opt"
          :class="{ active: i === active }"
          role="option"
          :aria-selected="i === active"
          :data-palette="it.id"
          @mousemove="active = i"
          @click="choose(it)"
        >
          <Icon :name="it.icon" :size="15" class="muted" />
          <span class="label">{{ it.label }}</span>
          <span class="hint faint">{{ it.hint }}</span>
        </li>
        <li v-if="!results.length" class="none muted">Nothing matches.</li>
      </ul>
    </div>
  </dialog>
</template>

<style scoped>
.palette {
  padding: 0;
  border: 0;
  background: transparent;
  width: min(620px, calc(100vw - 32px));
  margin: 12vh auto auto;
  max-height: 70vh;
}
.palette::backdrop {
  background: rgba(0, 0, 0, 0.45);
}
.panel {
  background: var(--surface);
  color: var(--text);
  border: 1px solid var(--border);
  border-radius: var(--radius-lg);
  box-shadow: var(--shadow-lg, 0 20px 50px rgba(0, 0, 0, 0.35));
  overflow: hidden;
  display: flex;
  flex-direction: column;
  max-height: 70vh;
}
.search {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 12px 14px;
  border-bottom: 1px solid var(--border);
}
.search input:focus,
.search input:focus-visible {
  outline: none;
  box-shadow: none;
}
.search input {
  flex: 1;
  border: 0;
  outline: 0;
  background: transparent;
  color: inherit;
  font: inherit;
  font-size: 15px;
  min-width: 0;
}
kbd {
  font-size: 11px;
  border: 1px solid var(--border);
  border-radius: 4px;
  padding: 1px 5px;
  color: var(--text-muted, inherit);
}
.list {
  list-style: none;
  margin: 0;
  padding: 6px;
  overflow: auto;
}
.opt {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 10px;
  border-radius: 8px;
  cursor: pointer;
}
.opt.active {
  background: var(--accent-soft);
}
.label {
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.hint {
  margin-left: auto;
  font-size: 12px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 45%;
}
.none {
  padding: 12px;
}
</style>
