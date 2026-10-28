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
import { laneActions, type LaneAction } from '@/lib/laneActions';
import { laneLabel, samePath } from '@/lib/lanes';
import { usePopover } from '@/lib/popover';
import { useHub } from '@/stores/hub';
import { useStack } from '@/stores/stack';
import Icon from './icons/Icon.vue';

const hub = useHub();
const stack = useStack();
const now = useNow();

const current = computed(() => stack.shownLane);
const branch = computed(() => (current.value ? current.value.branch : stack.view?.branch) ?? null);
/** A lane is named by its branch (shown already): only what kind of tree it is. */
const tree = computed(() => (current.value ? 'lane' : 'main working tree'));
/** Someone else's lane, or no hub: the branch is shown, nothing more. */
const canPick = computed(() => !stack.foreign && hub.reachable && !!stack.id);
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
    .map((b) => ({ b, lane: b.worktree && !b.main ? (stack.lanes.find((l) => samePath(l.path, b.worktree!)) ?? null) : null }))
    .sort((x, y) => Number(y.b.main) - Number(x.b.main) || x.b.name.localeCompare(y.b.name));
});
/** The branches with a worktree (the main tree's first), then the other local ones, then the remote ones; each by name. */
const groups = computed(() =>
  (
    [
      ['Lanes', rows.value.filter((r) => r.b.worktree)],
      ['Local', rows.value.filter((r) => !r.b.worktree && !r.b.remote)],
      ['Remote', rows.value.filter((r) => !r.b.worktree && r.b.remote)],
    ] as const
  ).filter(([, list]) => list.length),
);

/** Shown now: the main tree for its branch, the lane for another worktree's. */
function isShown(r: Row): boolean {
  return r.b.main ? !stack.lane : !!r.lane && r.lane.name === stack.lane;
}

/** What a click on the row does, said on it. */
function hint(r: Row): string {
  if (r.b.main) return 'main tree';
  if (r.lane?.removing) return 'lane · removing';
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
  return r.lane ? r.lane.active === false && !r.lane.owns_main && !r.lane.moving && !r.lane.removing : !stack.running;
}
function remove(r: Row) {
  close();
  void stack.removeWorktree(r.b.worktree!, r.lane ? laneLabel(r.lane) : r.b.name);
}

// A lane's actions, as the header's menu has them: by its "…" button or a
// right click on its row, under the row.
const actionsOf = ref<string | null>(null);
function toggleActions(r: Row) {
  if (!r.lane || stack.readOnly) return;
  creating.value = null;
  actionsOf.value = actionsOf.value === r.b.name ? null : r.b.name;
}
function laneAct(r: Row, a: LaneAction) {
  close();
  actionsOf.value = null;
  if (r.lane) void stack.laneAction(r.lane, a);
}

// A new branch from the one chosen, and a lane on it.
const creating = ref<string | null>(null);
const newName = ref('');
function startNew(r: Row) {
  actionsOf.value = null;
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
      <span class="branch">{{ branch ?? (current ? laneLabel(current) : 'detached') }}</span>
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
      <template v-for="([group, list], gi) in groups" :key="group">
        <div class="group faint small" :class="{ sep: gi > 0 }">{{ group }}</div>
        <template v-for="r in list.slice(0, SHOWN)" :key="r.b.name">
          <div
            class="row"
            :class="{ on: isShown(r) }"
            role="option"
            :aria-selected="isShown(r)"
            data-testid="branch-option"
            :data-branch="r.b.name"
            @contextmenu="r.lane && !stack.readOnly && ($event.preventDefault(), toggleActions(r))"
          >
            <button
              type="button"
              class="opt grow"
              :disabled="!!busy || (!!r.b.worktree && !r.b.main && !r.lane)"
              :title="r.b.worktree ? r.b.worktree : `Make a lane on ${r.b.name}: a worktree beside the repository's others`"
              @click="pick(r)"
            >
              <span class="dot" :class="r.b.main || (r.lane && r.lane.active !== false) ? 'on' : r.b.worktree ? 'off' : 'none'" />
              <span class="grow truncate">{{ r.b.name }}</span>
              <span
                v-if="r.b.stack_file === false && !r.b.main"
                class="pill sm outline"
                title="This branch has no devstack.toml: its lane runs with the main working tree's until it has its own"
                data-testid="branch-no-stack-file"
                >no devstack.toml</span
              >
              <span v-if="r.lane?.owns_main || (r.b.main && !stack.lanes.some((l) => l.owns_main))" class="pill sm ok">main ports</span>
              <span v-else class="faint small">{{ hint(r) }}</span>
            </button>
            <button
              v-if="r.lane && !stack.readOnly"
              type="button"
              class="icon-btn ghost"
              :disabled="!!busy"
              :aria-expanded="actionsOf === r.b.name"
              :title="`Actions of lane ${laneLabel(r.lane)} (or a right click on its row)`"
              data-testid="branch-lane-actions"
              @click="toggleActions(r)"
            >
              <Icon name="more" :size="13" />
            </button>
            <button type="button" class="icon-btn ghost" :disabled="!!busy" :title="`New branch from ${r.b.name}, and a lane on it`" data-testid="branch-new" @click="startNew(r)">
              <Icon name="plus" :size="13" />
            </button>
            <button v-if="removable(r)" type="button" class="icon-btn danger" :disabled="!!busy" :title="`Remove the worktree ${r.b.worktree} (git refuses one with changes; the branch stays)`" data-testid="branch-remove" @click="remove(r)">
              <Icon name="trash" :size="13" />
            </button>
          </div>
          <div v-if="actionsOf === r.b.name && r.lane" class="lane-actions" role="menu" :aria-label="`Actions of lane ${laneLabel(r.lane)}`" data-testid="branch-lane-menu">
            <template v-for="a in [laneActions(r.lane).main, ...laneActions(r.lane).items]" :key="a.key">
              <button type="button" class="opt" :class="{ danger: a.danger }" role="menuitem" :disabled="a.disabled" :title="a.title" :data-testid="`branch-lane-${a.key}`" @click="laneAct(r, a.key)">
                <Icon :name="a.icon ?? 'gitBranch'" :size="13" class="muted" />
                <span class="grow">{{ a.label }}</span>
                <span v-if="a.hint" class="faint small">{{ a.hint }}</span>
              </button>
            </template>
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
.group.sep {
  margin-top: 4px;
  padding-top: 8px;
  border-top: 1px solid var(--border);
}
.row {
  display: flex;
  align-items: center;
  gap: 2px;
  padding-right: 4px;
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
/* "New branch from" is always there, as a ghost button; removing shows on hover only. */
.icon-btn.ghost {
  opacity: 1;
  color: var(--text-faint);
  border: 1px solid var(--border);
  padding: 4px;
}
.icon-btn.ghost:hover {
  border-color: var(--border-strong);
}
.row:hover .icon-btn,
.icon-btn:focus-visible {
  opacity: 1;
  color: var(--text-muted);
}
.icon-btn:hover {
  background: var(--surface-2);
  color: var(--text);
}
.icon-btn.danger:hover {
  color: var(--danger);
}
.lane-actions {
  display: flex;
  flex-direction: column;
  padding: 2px 8px 6px 24px;
}
.lane-actions .opt.danger {
  color: var(--danger-text, var(--danger));
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
