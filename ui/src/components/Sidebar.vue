<script setup lang="ts">
// Stacks and their worktrees (lanes, ADR-0024) as a sidebar: every stack the
// hub knows, and under each running one its lanes, named by their branch.
// Started lanes first; the worktrees nobody started fold away. A stack's
// worktrees fold only with its toggle, never by navigating. The shown
// stack's lanes come live from its stream; the others' are read when the
// hub's list of stacks changes (no polling, ADR-0021).
import { computed, nextTick, ref, watch } from 'vue';
import { useRouter } from 'vue-router';
import { api, paths, v1 } from '@/api';
import type { LaneView, StackEntry } from '@/api/types';
import { laneLabel, orderLanes } from '@/lib/lanes';
import { persist } from '@/lib/persist';
import { useHub } from '@/stores/hub';
import { useStack } from '@/stores/stack';
import AddStackDialog from './AddStackDialog.vue';
import Icon from './icons/Icon.vue';

const hub = useHub();
const stack = useStack();
const router = useRouter();
const adding = ref(false);
const unfolded = ref<Record<string, boolean>>({});
/** Stacks whose worktrees are folded away; only the toggle changes it. */
const collapsed = ref<Record<string, boolean>>(persist.get<Record<string, boolean>>('sidebar.collapsed') ?? {});
function toggle(id: string) {
  collapsed.value[id] = !collapsed.value[id];
  persist.set('sidebar.collapsed', collapsed.value);
}

const stacks = computed(() => [...hub.stacks].sort((a, b) => Number(b.running) - Number(a.running) || a.name.localeCompare(b.name)));

/**
 * The worktrees of every stack, kept across navigation: replaced when news
 * arrives (the shown stack's stream, a read when the hub's list changes),
 * never emptied by a switch, so nothing folds or flickers.
 */
const known = ref<Record<string, LaneView[]>>({});
watch(
  () => [stack.lanesOf, stack.lanes] as const,
  ([sid, list]) => {
    if (sid) known.value[sid] = list;
  },
  { immediate: true },
);
async function loadAll(list: StackEntry[]) {
  for (const s of list) {
    if (!s.running) continue;
    try {
      const res = await api().request<LaneView[]>('GET', paths.api(s.id, v1.lanes));
      // The shown stack's list comes from its stream: a read that started
      // before a change and answers after it must not overwrite it.
      if (res.ok && Array.isArray(res.body) && stack.lanesOf !== s.id) known.value[s.id] = res.body;
    } catch {
      /* keep what is known */
    }
  }
}
watch(
  () => hub.stacks.map((s) => `${s.id}:${s.running}`).join(','),
  () => void loadAll(hub.stacks),
  { immediate: true },
);

function lanesOf(s: StackEntry) {
  if (!s.running) return { running: [], idle: [] };
  return orderLanes(known.value[s.id] ?? []);
}
const hasLanes = (s: StackEntry) => s.running && (known.value[s.id]?.length ?? 0) > 0;

function openStack(s: StackEntry) {
  if (s.id === stack.id && stack.lane) stack.selectLane(null);
  persist.set(`lane.${s.id}`, null);
  void router.push({ name: 'overview', params: { stack: s.id } });
}

async function openLane(s: StackEntry, l: LaneView) {
  const idle = l.active === false;
  if (s.id === stack.id) {
    if (idle) void stack.startLane(l.name);
    else stack.selectLane(l.name);
    void router.push({ name: 'overview', params: { stack: s.id } });
    return;
  }
  if (!idle) persist.set(`lane.${s.id}`, l.name);
  await router.push({ name: 'overview', params: { stack: s.id } });
  if (idle) {
    await nextTick();
    void stack.startLane(l.name);
  }
}

const isCurrent = (s: StackEntry) => s.id === stack.id;
</script>

<template>
  <aside class="sidebar" aria-label="Stacks and worktrees" data-testid="sidebar">
    <div class="head">Stacks</div>
    <ul class="stacks">
      <li v-for="s in stacks" :key="s.id" class="stack" :class="{ current: isCurrent(s) }" :data-stack="s.id">
        <div class="stack-row">
        <button
          v-if="hasLanes(s)"
          type="button"
          class="fold-btn"
          :aria-expanded="!collapsed[s.id]"
          :aria-label="`${collapsed[s.id] ? 'Show' : 'Hide'} the worktrees of ${s.name}`"
          :title="collapsed[s.id] ? 'Show worktrees' : 'Hide worktrees'"
          data-testid="sidebar-fold"
          @click="toggle(s.id)"
        >
          <Icon :name="collapsed[s.id] ? 'chevronRight' : 'chevronDown'" :size="13" />
        </button>
        <span v-else class="fold-space" />
        <button type="button" class="item" :class="{ active: isCurrent(s) && !stack.lane }" :title="s.repo" data-testid="sidebar-stack" @click="openStack(s)">
          <span class="dot" :class="!hub.reachable ? 'unknown' : s.running ? 'on' : 'off'" aria-hidden="true" />
          <span class="label truncate">{{ s.name }}</span>
          <span v-if="!s.running" class="faint small">offline</span>
        </button>
        </div>
        <ul v-if="hasLanes(s) && !collapsed[s.id]" class="lanes">
          <li v-for="l in lanesOf(s).running" :key="l.name">
            <button
              type="button"
              class="item lane"
              :class="{ active: isCurrent(s) && stack.lane === l.name }"
              :title="`${l.path} · ports +${l.port_offset}`"
              data-testid="sidebar-lane"
              :data-lane="l.name"
              @click="openLane(s, l)"
            >
              <Icon name="gitBranch" :size="13" class="branch" />
              <span class="label truncate">{{ laneLabel(l) }}</span>
              <span v-if="l.owns_main" class="pill sm ok">main ports</span>
              <span v-else class="faint small tabular">{{ l.running }}/{{ l.services }}</span>
            </button>
          </li>
          <li v-if="lanesOf(s).idle.length">
            <button type="button" class="item fold" :aria-expanded="!!unfolded[s.id]" data-testid="sidebar-idle-toggle" @click="unfolded[s.id] = !unfolded[s.id]">
              <Icon :name="unfolded[s.id] ? 'chevronDown' : 'chevronRight'" :size="13" />
              {{ lanesOf(s).idle.length }} not started
            </button>
          </li>
          <template v-if="unfolded[s.id]">
            <li v-for="l in lanesOf(s).idle" :key="l.name">
              <button
                type="button"
                class="item lane idle"
                :class="{ active: isCurrent(s) && stack.lane === l.name }"
                :title="`${l.path} — click to start it`"
                data-testid="sidebar-lane"
                :data-lane="l.name"
                @click="openLane(s, l)"
              >
                <Icon name="gitBranch" :size="13" class="branch" />
                <span class="label truncate">{{ laneLabel(l) }}</span>
                <span class="faint small">start</span>
              </button>
            </li>
          </template>
        </ul>
      </li>
      <li v-if="!stacks.length" class="empty">No stacks yet. Add one, or run <code>devstack up</code> in a project.</li>
    </ul>
    <button type="button" class="add" data-testid="sidebar-add-stack" @click="adding = true"><Icon name="plus" :size="14" /> Add a stack…</button>
    <AddStackDialog :open="adding" @close="adding = false" />
  </aside>
</template>

<style scoped>
.sidebar {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 12px 8px;
  border-right: 1px solid var(--border);
  background: var(--bg-elev);
  min-width: 0;
  overflow-y: auto;
}
.head {
  font-size: 11px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  color: var(--text-muted);
  padding: 4px 8px;
}
ul {
  list-style: none;
  margin: 0;
  padding: 0;
}
.stacks {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.lanes {
  margin: 2px 0 6px;
  border-left: 1px solid var(--border);
  margin-left: 30px;
  padding-left: 4px;
}
.item {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 6px 8px;
  border: 0;
  border-radius: 8px;
  background: transparent;
  color: inherit;
  font: inherit;
  font-size: 13px;
  text-align: left;
  cursor: pointer;
  min-width: 0;
}
.item:hover {
  background: var(--surface-hover);
}
.item.active {
  background: var(--accent-soft);
  color: var(--accent-text);
}
.stack > .item {
  font-weight: 600;
}
.lane.idle {
  color: var(--text-muted);
}
.fold {
  color: var(--text-muted);
  font-size: 12px;
}
.label {
  flex: 1;
  min-width: 0;
}
.small {
  font-size: 11px;
}
.branch {
  color: var(--text-muted);
  flex: none;
}
.dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  flex: none;
}
.dot.on {
  background: var(--ok);
  box-shadow: 0 0 0 3px color-mix(in srgb, var(--ok) 22%, transparent);
}
.dot.unknown {
  background: var(--text-faint);
}
.dot.off {
  border: 1.5px solid var(--text-faint);
}
.empty {
  padding: 8px;
  color: var(--text-muted);
  font-size: 12px;
}
.add {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-top: 8px;
  padding: 8px;
  border: 0;
  border-top: 1px solid var(--border);
  background: transparent;
  color: var(--accent);
  font: inherit;
  font-size: 13px;
  cursor: pointer;
  text-align: left;
}
.add:hover {
  background: var(--surface-hover);
}
.stack-row {
  display: flex;
  align-items: center;
  gap: 2px;
}
.fold-btn,
.fold-space {
  width: 20px;
  height: 24px;
  flex: none;
}
.fold-btn {
  display: grid;
  place-items: center;
  border: 0;
  border-radius: 6px;
  background: transparent;
  color: var(--text-muted);
  cursor: pointer;
}
.fold-btn:hover {
  background: var(--surface-hover);
  color: var(--text);
}
</style>
