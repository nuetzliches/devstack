<script setup lang="ts">
import { computed, nextTick, ref } from 'vue';
import { useRouter } from 'vue-router';
import { ago, useNow } from '@/lib/format';
import { usePopover } from '@/lib/popover';
import { useHub } from '@/stores/hub';
import { useStack } from '@/stores/stack';
import Icon from './icons/Icon.vue';
import AddStackDialog from './AddStackDialog.vue';

const hub = useHub();
const stack = useStack();
const router = useRouter();
const now = useNow();

const root = ref<HTMLElement | null>(null);
const trigger = ref<HTMLButtonElement | null>(null);
const list = ref<HTMLElement | null>(null);
const { open, close } = usePopover(root, trigger);
const active = ref(0);
const adding = ref(false);

function addStack() {
  close();
  adding.value = true;
}

const stacks = computed(() => [...hub.stacks].sort((a, b) => Number(b.running) - Number(a.running) || a.name.localeCompare(b.name)));
const current = computed(() => hub.entry(stack.id));
const currentName = computed(() => current.value?.name ?? stack.view?.name ?? 'Select a stack');

async function toggle() {
  open.value = !open.value;
  if (open.value) {
    active.value = Math.max(0, stacks.value.findIndex((s) => s.id === stack.id));
    await nextTick();
    focusActive();
  }
}

function focusActive() {
  const items = list.value?.querySelectorAll<HTMLElement>('[role="option"]');
  items?.[active.value]?.focus();
}

function choose(id: string) {
  close();
  trigger.value?.focus();
  void router.push({ name: 'overview', params: { stack: id } });
}

function onKey(e: KeyboardEvent) {
  const n = stacks.value.length;
  if (!n) return;
  if (e.key === 'ArrowDown') {
    active.value = (active.value + 1) % n;
    focusActive();
    e.preventDefault();
  } else if (e.key === 'ArrowUp') {
    active.value = (active.value - 1 + n) % n;
    focusActive();
    e.preventDefault();
  } else if (e.key === 'Home') {
    active.value = 0;
    focusActive();
    e.preventDefault();
  } else if (e.key === 'End') {
    active.value = n - 1;
    focusActive();
    e.preventDefault();
  } else if (e.key === 'Enter' || e.key === ' ') {
    const s = stacks.value[active.value];
    if (s) choose(s.id);
    e.preventDefault();
  } else if (e.key === 'Tab') {
    close();
  }
}
</script>

<template>
  <div ref="root" class="switcher">
    <button
      ref="trigger"
      type="button"
      class="trigger"
      aria-haspopup="listbox"
      :aria-expanded="open"
      aria-label="Switch stack"
      data-testid="stack-switcher"
      @click="toggle"
      @keydown.down.prevent="!open && toggle()"
    >
      <span class="sdot" :class="!hub.reachable ? 'unknown' : current?.running ? 'on' : 'off'" aria-hidden="true" />
      <span class="sname truncate">{{ currentName }}</span>
      <Icon name="chevronDown" :size="14" class="chev" />
    </button>
    <Transition name="drop">
      <div v-if="open" class="menu" role="presentation">
        <div class="menu-head">Stacks</div>
        <ul ref="list" role="listbox" aria-label="Stacks" @keydown="onKey">
          <li
            v-for="(s, i) in stacks"
            :key="s.id"
            role="option"
            tabindex="-1"
            :aria-selected="s.id === stack.id"
            class="opt"
            :class="{ current: s.id === stack.id, focus: i === active }"
            data-testid="stack-option"
            :data-stack="s.id"
            @click="choose(s.id)"
            @mouseenter="active = i"
          >
            <span class="sdot" :class="!hub.reachable ? 'unknown' : s.running ? 'on' : 'off'" aria-hidden="true" />
            <div class="grow">
              <div class="row">
                <span class="opt-name">{{ s.name }}</span>
                <span v-if="hub.reachable" class="pill sm" :class="s.running ? 'ok' : ''">{{ s.running ? 'running' : 'offline' }}</span>
              </div>
              <div class="opt-sub truncate" :title="s.repo">{{ s.repo }}</div>
              <div v-if="!s.running" class="opt-sub">last seen {{ ago(s.last_seen, now) }}</div>
            </div>
            <Icon v-if="s.id === stack.id" name="check" :size="15" class="tick" />
          </li>
          <li v-if="!stacks.length" class="empty-opt">No stacks registered yet. Add one, or run <code>devstack up</code> in a project.</li>
        </ul>
        <button type="button" class="add-opt" data-testid="add-stack-open" @click="addStack"><Icon name="plus" :size="14" /> Add a stack…</button>
      </div>
    </Transition>
    <AddStackDialog :open="adding" @close="adding = false" />
  </div>
</template>

<style scoped>
.switcher {
  position: relative;
  min-width: 0;
}
.trigger {
  display: flex;
  align-items: center;
  gap: 8px;
  height: 34px;
  padding: 0 10px 0 12px;
  border-radius: 8px;
  border: 1px solid var(--border);
  background: var(--surface-2);
  cursor: pointer;
  max-width: 260px;
  min-width: 0;
  font-weight: 600;
}
.trigger:hover {
  border-color: var(--border-strong);
  background: var(--surface-hover);
}
.chev {
  color: var(--text-muted);
  flex: none;
}
.sdot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  flex: none;
}
.sdot.on {
  background: var(--ok);
  box-shadow: 0 0 0 3px color-mix(in srgb, var(--ok) 22%, transparent);
}
.sdot.unknown {
  background: var(--text-faint);
}
.sdot.off {
  background: transparent;
  border: 1.5px solid var(--text-faint);
}
.menu {
  position: absolute;
  top: calc(100% + 6px);
  left: 0;
  z-index: 40;
  width: min(340px, calc(100vw - 24px));
  background: var(--bg-elev);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  box-shadow: var(--shadow-lg);
  padding: 6px;
}
.menu-head {
  font-size: 11px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  color: var(--text-muted);
  padding: 6px 8px 4px;
}
ul {
  list-style: none;
  margin: 0;
  padding: 0;
  max-height: 60vh;
  overflow: auto;
}
.opt {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  padding: 8px;
  border-radius: 8px;
  cursor: pointer;
}
.opt .sdot {
  margin-top: 6px;
}
.opt.focus,
.opt:focus {
  background: var(--surface-hover);
  outline: none;
}
.opt.current {
  background: var(--accent-soft);
}
.opt-name {
  font-weight: 600;
}
.opt-sub {
  font-size: 12px;
  color: var(--text-muted);
}
.tick {
  color: var(--accent-text);
  margin-top: 3px;
}
.empty-opt {
  padding: 10px;
  color: var(--text-muted);
  font-size: 13px;
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
@media (max-width: 520px) {
  .trigger {
    max-width: 150px;
  }
}
.add-opt {
  display: flex;
  align-items: center;
  gap: 6px;
  width: 100%;
  padding: 9px 12px;
  border: 0;
  border-top: 1px solid var(--border);
  background: transparent;
  color: var(--accent);
  font: inherit;
  font-size: 13px;
  cursor: pointer;
  text-align: left;
}
.add-opt:hover {
  background: var(--surface-hover);
}
</style>
