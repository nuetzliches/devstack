<script setup lang="ts">
// The stack header's branch picker (ADR-0047): the repository's branches,
// each with the worktree that has it. A branch with a worktree shows its
// lane, one without gets a lane (a worktree the hub makes), any gets a new
// branch made from it; a stopped lane's worktree can be removed. The main
// working tree's checkout is never changed. Switching between lanes is the
// sidebar's too.
import { computed, nextTick, ref } from 'vue';
import type { BranchView, LaneView } from '@/api/types';
import { ago, useNow } from '@/lib/format';
import { laneLabel, samePath } from '@/lib/lanes';
import { usePopover } from '@/lib/popover';
import { useHub } from '@/stores/hub';
import { useStack } from '@/stores/stack';
import Icon from './icons/Icon.vue';

const hub = useHub();
const stack = useStack();
const now = useNow();

const current = computed(() => stack.lanes.find((l) => l.name === stack.lane) ?? null);
const branch = computed(() => (current.value ? current.value.branch : stack.view?.branch) ?? null);
const tree = computed(() => (current.value ? laneLabel(current.value) : 'main working tree'));
/** Someone else's lane, or no hub: the branch is shown, nothing more. */
const canPick = computed(() => !stack.readOnly && hub.reachable && !!stack.id);
const busy = computed(() => stack.busy.branches);

const root = ref<HTMLElement | null>(null);
const trigger = ref<HTMLButtonElement | null>(null);
const { open, close } = usePopover(root, trigger);
const filter = ref('');
const filterInput = ref<HTMLInputElement | null>(null);

function toggle() {
  if (!canPick.value) return;
  open.value = !open.value;
  if (open.value) {
    filter.value = '';
    creating.value = null;
    void stack.loadBranches();
    void nextTick(() => filterInput.value?.focus());
  }
}

type Row = { b: BranchView; lane: LaneView | null };
/** At most this many of a group are listed; the filter finds the others. */
const SHOWN = 40;

const rows = computed<Row[]>(() => {
  const q = filter.value.trim().toLowerCase();
  return (stack.branches?.branches ?? [])
    .filter((b) => !q || b.name.toLowerCase().includes(q))
    .map((b) => ({ b, lane: b.worktree && !b.main ? (stack.lanes.find((l) => samePath(l.path, b.worktree!)) ?? null) : null }));
});
const local = computed(() => rows.value.filter((r) => !r.b.remote));
const remote = computed(() => rows.value.filter((r) => r.b.remote));

/** Shown now: the main tree for its branch, the lane for another worktree's. */
function isShown(r: Row): boolean {
  return r.b.main ? !stack.lane : !!r.lane && r.lane.name === stack.lane;
}

/** What a click on the row does, said on it. */
function hint(r: Row): string {
  if (r.b.main) return 'main tree';
  if (r.lane) return r.lane.owns_main ? 'main ports' : r.lane.active === false ? 'lane · not started' : 'lane · running';
  if (r.b.worktree) return 'worktree';
  return busy.value === 'add' ? '' : 'new lane';
}

function pick(r: Row) {
  if (busy.value) return;
  if (r.b.main) {
    close();
    stack.selectLane(null);
  } else if (r.lane) {
    close();
    stack.selectLane(r.lane.name);
  } else if (!r.b.worktree) {
    close();
    void stack.newLane(r.b.name);
  }
}

/** A stopped lane's worktree, or any while the stack is down (nothing of it runs then). */
function removable(r: Row): boolean {
  if (!r.b.worktree || r.b.main) return false;
  return r.lane ? r.lane.active === false && !r.lane.owns_main && !r.lane.moving : !stack.running;
}
function remove(r: Row) {
  close();
  void stack.removeWorktree(r.b.worktree!, r.lane ? laneLabel(r.lane) : r.b.name);
}

// A new branch from the one chosen, and a lane on it.
const creating = ref<string | null>(null);
const newName = ref('');
function startNew(r: Row) {
  creating.value = r.b.name;
  newName.value = '';
  void nextTick(() => root.value?.querySelector<HTMLInputElement>('[data-testid="branch-new-name"]')?.focus());
}
function create() {
  const from = creating.value;
  const name = newName.value.trim();
  if (!from || !name) return;
  close();
  void stack.newLane(from, name);
}
</script>

<template>
  <div ref="root" class="picker">
    <button
      ref="trigger"
      type="button"
      class="tree"
      :class="{ fixed: !canPick }"
      :aria-expanded="open"
      aria-haspopup="listbox"
      :title="canPick ? 'Branches: show a lane, make one from a branch' : 'The branch checked out here'"
      data-testid="worktree-picker"
      @click="toggle"
    >
      <Icon name="gitBranch" :size="13" />
      <span class="branch">{{ branch ?? 'detached' }}</span>
      <span class="muted">· {{ tree }}</span>
      <span v-if="current" class="muted">· ports +{{ current.port_offset }}</span>
      <Icon v-if="canPick" name="chevronDown" :size="13" class="muted" />
    </button>
    <div v-if="open" class="menu" role="listbox" aria-label="Branches" data-testid="branch-menu">
      <div class="head">
        <span class="search">
          <Icon name="search" :size="13" class="muted" />
          <input ref="filterInput" v-model="filter" class="input" placeholder="Filter branches" data-testid="branch-filter" @keydown.esc="close()" />
        </span>
        <button
          type="button"
          class="btn btn-xs"
          :disabled="!!busy"
          :title="`git fetch --all --prune${stack.branches?.fetched ? `; last fetched ${ago(stack.branches.fetched * 1000, now)}` : ''}`"
          data-testid="branch-fetch"
          @click="stack.fetchBranches()"
        >
          <span v-if="busy === 'fetch'" class="spinner" /><Icon v-else name="download" :size="13" /> Fetch
        </button>
      </div>
      <span v-if="!stack.branches" class="faint small note"><span class="spinner" /> Reading branches…</span>
      <span v-else-if="!rows.length" class="faint small note">No branch {{ filter ? `matches “${filter}”` : 'here' }}.</span>
      <template v-for="[group, list] in [['Local', local], ['Remote', remote]] as const" :key="group">
        <div v-if="list.length" class="group faint small">{{ group }}</div>
        <template v-for="r in list.slice(0, SHOWN)" :key="r.b.name">
          <div class="row" :class="{ on: isShown(r) }" role="option" :aria-selected="isShown(r)" data-testid="branch-option" :data-branch="r.b.name">
            <button
              type="button"
              class="opt grow"
              :disabled="!!busy || (!!r.b.worktree && !r.b.main && !r.lane)"
              :title="r.b.worktree ? r.b.worktree : `Make a lane on ${r.b.name}: a worktree beside the repository's others`"
              @click="pick(r)"
            >
              <span class="dot" :class="r.b.main || (r.lane && r.lane.active !== false) ? 'on' : r.b.worktree ? 'off' : 'none'" />
              <span class="grow truncate">{{ r.b.name }}</span>
              <span v-if="r.lane?.owns_main || (r.b.main && !stack.lanes.some((l) => l.owns_main))" class="pill sm ok">main ports</span>
              <span v-else class="faint small">{{ hint(r) }}</span>
            </button>
            <button type="button" class="icon-btn" :disabled="!!busy" :title="`New branch from ${r.b.name}, and a lane on it`" data-testid="branch-new" @click="startNew(r)">
              <Icon name="plus" :size="13" />
            </button>
            <button v-if="removable(r)" type="button" class="icon-btn danger" :disabled="!!busy" :title="`Remove the worktree ${r.b.worktree} (git refuses one with changes; the branch stays)`" data-testid="branch-remove" @click="remove(r)">
              <Icon name="trash" :size="13" />
            </button>
          </div>
          <form v-if="creating === r.b.name" class="new" @submit.prevent="create">
            <input v-model="newName" class="input mono" placeholder="new branch name" data-testid="branch-new-name" @keydown.esc="creating = null" />
            <button type="submit" class="btn btn-xs btn-primary" :disabled="!newName.trim()">Create</button>
          </form>
        </template>
        <span v-if="list.length > SHOWN" class="faint small note">{{ list.length - SHOWN }} more: filter to find them</span>
      </template>
      <p class="faint small note foot">The main working tree's checkout is never changed: a branch runs in a lane, which can then serve the main ports.</p>
    </div>
  </div>
</template>

<style scoped>
.picker {
  position: relative;
}
.tree {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 3px 8px;
  border: 1px solid var(--border);
  border-radius: 999px;
  background: var(--surface-2);
  color: inherit;
  font: inherit;
  font-size: 12.5px;
  cursor: pointer;
}
.tree:hover {
  border-color: var(--border-strong);
}
.tree.fixed {
  cursor: default;
}
.tree.fixed:hover {
  border-color: var(--border);
}
.branch {
  font-weight: 600;
}
.menu {
  position: absolute;
  top: calc(100% + 6px);
  left: 0;
  z-index: 40;
  width: 380px;
  max-height: 60vh;
  overflow: auto;
  background: var(--bg-elev);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  box-shadow: var(--shadow-lg);
  padding: 6px;
  display: flex;
  flex-direction: column;
}
.head {
  display: flex;
  gap: 6px;
  align-items: center;
  padding: 2px 2px 6px;
}
.search {
  flex: 1;
  display: flex;
  align-items: center;
  gap: 6px;
}
.search .input {
  flex: 1;
  min-width: 0;
}
.group {
  padding: 6px 8px 2px;
  text-transform: uppercase;
  letter-spacing: 0.04em;
}
.row {
  display: flex;
  align-items: center;
  gap: 2px;
  border-radius: 8px;
}
.row:hover {
  background: var(--surface-hover);
}
.row.on {
  background: var(--accent-soft);
  color: var(--accent-text);
}
.opt {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 8px;
  border: 0;
  border-radius: 8px;
  background: transparent;
  color: inherit;
  font: inherit;
  font-size: 13px;
  text-align: left;
  cursor: pointer;
}
.opt:disabled {
  cursor: default;
}
.icon-btn {
  display: inline-flex;
  padding: 5px;
  border: 0;
  border-radius: 6px;
  background: transparent;
  color: var(--text-muted);
  cursor: pointer;
  opacity: 0;
}
.row:hover .icon-btn,
.icon-btn:focus-visible {
  opacity: 1;
}
.icon-btn:hover {
  background: var(--surface-2);
  color: var(--text);
}
.icon-btn.danger:hover {
  color: var(--danger);
}
.new {
  display: flex;
  gap: 6px;
  padding: 4px 8px 8px 24px;
}
.new .input {
  flex: 1;
  min-width: 0;
}
.note {
  padding: 6px 8px;
  display: inline-flex;
  align-items: center;
  gap: 6px;
}
.foot {
  margin: 4px 0 0;
  border-top: 1px solid var(--border);
}
.small {
  font-size: 12px;
}
.grow {
  flex: 1;
  min-width: 0;
}
.dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  flex: none;
}
.dot.on {
  background: var(--ok);
}
.dot.off {
  border: 1.5px solid var(--text-faint);
}
.dot.none {
  border: 1.5px dashed var(--text-faint);
}
</style>
