<script setup lang="ts">
// Stacks and their worktrees (lanes, ADR-0024) as a sidebar: every stack the
// hub knows, and under each running one all of its trees, named by their
// branch: the main working tree first, then the started lanes, then the
// worktrees nobody started. A tree's state is its dot, as a stack's is. A
// stack's worktrees fold only with its toggle, never by navigating. The shown
// stack's lanes come live from its stream; the others' are read when the
// hub's list of stacks changes (no polling, ADR-0021). A stack's menu (its
// "…" button, or a right click) starts, stops and removes it.
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { api, paths, sharedId, v1 } from '@/api';
import type { LaneView, StackEntry } from '@/api/types';
import { act } from '@/lib/actions';
import { laneLabel, orderLanes, nodeLabel, started } from '@/lib/lanes';
import { persist } from '@/lib/persist';
import { statusBucket } from '@/lib/status';
import { useHub } from '@/stores/hub';
import { usePool } from '@/stores/pool';
import { useStack } from '@/stores/stack';
import { useUi } from '@/stores/ui';
import AddStackDialog from './AddStackDialog.vue';
import GuestTile from './GuestTile.vue';
import Icon from './icons/Icon.vue';

const hub = useHub();
const stack = useStack();
const ui = useUi();
const router = useRouter();
const route = useRoute();
// The pool (ADR-0032): always in reach, with the requests waiting for an
// admin as a badge.
const pool = usePool();
onBeforeUnmount(pool.watch(30_000));
const poolHint = computed(() => {
  const o = pool.overview;
  if (!o) return '';
  if (!o.joined) return 'not joined';
  if (o.me?.state === 'pending') return 'waiting';
  const n = o.nodes ?? [];
  return `${n.filter((x) => x.online).length}/${n.length}`;
});
// Lanes others share with this machine (ADR-0035), listed as the stacks are;
// a click opens the lane read only, as one's own is shown (ADR-0040).
const sharedLanes = computed(() =>
  [...pool.shared].sort((a, b) => `${a.repo} ${a.lane} ${a.owner_name}`.localeCompare(`${b.repo} ${b.lane} ${b.owner_name}`)),
);
function sharedTitle(l: (typeof pool.shared)[number]): string {
  const node = l.pool === 'default' ? l.node.name : `${l.pool}:${l.node.name}`;
  return `${l.repo} · ${l.lane}\nshared by ${l.owner_name}, runs on ${node} · read only`;
}
const adding = ref(false);
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

/** Every worktree of a running stack: started ones first, then the rest. */
function lanesOf(s: StackEntry): LaneView[] {
  if (!s.running) return [];
  const { running, idle } = orderLanes(known.value[s.id] ?? []);
  return [...running, ...idle];
}
const hasLanes = (s: StackEntry) => s.running && (known.value[s.id]?.length ?? 0) > 0;

/**
 * Whether the main working tree runs something, as last seen while it was
 * shown; a stack not shown yet counts as running while its daemon does.
 */
const mainActive = ref<Record<string, boolean>>({});
watch(
  () => [stack.id, stack.lane, stack.live, stack.services] as const,
  ([id, lane, live, services]) => {
    if (id && lane === null && live && services.length) mainActive.value[id] = services.some((x) => statusBucket(x.status) !== 'idle');
  },
  { immediate: true },
);
type Dot = 'on' | 'off' | 'warn' | 'err' | 'unknown';
function stackDot(s: StackEntry): Dot {
  return !hub.reachable ? 'unknown' : s.running ? 'on' : 'off';
}
function mainDot(s: StackEntry): Dot {
  if (!hub.reachable) return 'unknown';
  return mainActive.value[s.id] === false ? 'off' : 'on';
}
const servedBy = (s: StackEntry) => (known.value[s.id] ?? []).find((l) => l.owns_main) ?? null;
/**
 * How many of a lane's services run: the shown lane's from its own stream,
 * which the list (the main tree's, not followed meanwhile) lags behind.
 */
function laneRuns(s: StackEntry, l: LaneView): number {
  if (s.id !== stack.id || stack.lane !== l.name || !stack.live || !stack.services.length) return l.running;
  return stack.services.filter((x) => statusBucket(x.status) !== 'idle').length;
}
/** A lane's state as a dot, like a stack's: the words are in its tooltip. */
function laneDot(s: StackEntry, l: LaneView): Dot {
  if (!hub.reachable) return 'unknown';
  if (l.moving?.error) return 'err';
  if (!started(l)) return 'off';
  return laneRuns(s, l) > 0 ? 'on' : 'warn';
}
function laneTitle(s: StackEntry, l: LaneView): string {
  const where = l.node ? `runs on ${nodeLabel(l)}` : `ports +${l.port_offset}`;
  if (l.moving?.error) return `${l.path}\n${l.moving.error}`;
  if (l.moving) return `${l.path}\n${l.moving.what}`;
  if (!started(l)) return `${l.path}\nnot started`;
  return `${l.path}\n${laneRuns(s, l)} of ${l.services} services running · ${where}${l.owns_main ? ' · serves the main ports' : ''}`;
}

function openStack(s: StackEntry) {
  if (s.id === stack.id && stack.lane) stack.selectLane(null);
  persist.set(`lane.${s.id}`, null);
  void router.push({ name: 'overview', params: { stack: s.id } });
}

// A lane not started is shown with its start options (here, a node, the
// pool), never started by navigating to it.
async function openLane(s: StackEntry, l: LaneView) {
  if (s.id === stack.id) {
    stack.selectLane(l.name);
    void router.push({ name: 'overview', params: { stack: s.id } });
    return;
  }
  persist.set(`lane.${s.id}`, l.name);
  await router.push({ name: 'overview', params: { stack: s.id } });
}

const isCurrent = (s: StackEntry) => s.id === stack.id;

// ------------------------------------------------------------ stack menu

const menu = ref<{ s: StackEntry; x: number; y: number } | null>(null);
const menuEl = ref<HTMLElement | null>(null);
function openMenu(s: StackEntry, e: MouseEvent) {
  const at = e.type === 'contextmenu' ? { x: e.clientX, y: e.clientY } : (e.currentTarget as HTMLElement).getBoundingClientRect();
  const x = 'right' in at ? at.left : at.x;
  const y = 'bottom' in at ? at.bottom + 2 : at.y;
  menu.value = { s, x, y };
  void nextTick(() => {
    // Inside the window, and its first entry focused for the keyboard.
    const el = menuEl.value;
    if (!el || !menu.value) return;
    const r = el.getBoundingClientRect();
    menu.value.x = Math.max(4, Math.min(menu.value.x, window.innerWidth - r.width - 4));
    menu.value.y = Math.max(4, Math.min(menu.value.y, window.innerHeight - r.height - 4));
    el.querySelector<HTMLButtonElement>('button:not(:disabled)')?.focus();
  });
}
function closeMenu() {
  menu.value = null;
}
function onDown(e: PointerEvent) {
  if (menuEl.value && !menuEl.value.contains(e.target as Node)) closeMenu();
}
function onKey(e: KeyboardEvent) {
  if (e.key === 'Escape') closeMenu();
}
watch(menu, (m, before) => {
  if (m && !before) {
    document.addEventListener('pointerdown', onDown, true);
    document.addEventListener('keydown', onKey);
    window.addEventListener('blur', closeMenu);
  } else if (!m) {
    document.removeEventListener('pointerdown', onDown, true);
    document.removeEventListener('keydown', onKey);
    window.removeEventListener('blur', closeMenu);
  }
});
onBeforeUnmount(closeMenu);

async function startStack(s: StackEntry) {
  closeMenu();
  hub.starting[s.id] = true;
  const res = await act({ label: `Start ${s.name}`, path: paths.up(s.id), success: 'Starting the stack…' });
  if (!res.ok) delete hub.starting[s.id];
}
async function stopStack(s: StackEntry) {
  closeMenu();
  // The shown stack's own stop knows its lanes and says what stays.
  if (isCurrent(s)) return void stack.stopStack();
  const option = stack.stopContainers(false);
  const ok = await ui.confirm({
    title: `Stop ${s.name}`,
    text: 'Its services stop. Lanes that still run keep its daemon and the containers they share.',
    confirmLabel: 'Stop stack',
    danger: true,
    option,
  });
  if (ok) await act({ label: `Stop ${s.name}`, path: paths.down(s.id, option.checked), success: 'Stopping the stack…' });
}
async function removeStack(s: StackEntry) {
  closeMenu();
  const ok = await ui.confirm({
    title: `Remove ${s.name}`,
    text: `It leaves the dashboard; its files and its stack file stay. It is listed again the next time it starts (devstack up in ${s.repo}).`,
    confirmLabel: 'Remove',
    danger: true,
  });
  if (!ok) return;
  const res = await act({ label: `Remove ${s.name}`, method: 'DELETE', path: paths.forget(s.id), success: `${s.name} removed from the dashboard` });
  if (!res.ok) return;
  persist.forgetStack(s.id);
  delete known.value[s.id];
  if (isCurrent(s)) void router.push('/');
}
async function copyPath(s: StackEntry) {
  closeMenu();
  try {
    await navigator.clipboard.writeText(s.repo);
    ui.toast('info', 'Copied', s.repo);
  } catch {
    ui.toast('info', s.repo, 'Copying is not allowed here.');
  }
}
</script>

<template>
  <aside class="sidebar" aria-label="Stacks and worktrees" data-testid="sidebar">
    <div class="head head-row">
      <span>Stacks</span>
      <button type="button" class="head-btn" title="Add a stack…" aria-label="Add a stack" data-testid="sidebar-add-stack" @click="adding = true">
        <Icon name="plus" :size="14" />
      </button>
    </div>
    <ul class="stacks">
      <li v-for="s in stacks" :key="s.id" class="stack" :class="{ current: isCurrent(s) }" :data-stack="s.id">
        <div class="stack-row" @contextmenu.prevent="openMenu(s, $event)">
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
          <button
            type="button"
            class="item"
            :class="{ active: isCurrent(s) && !stack.lane && !hasLanes(s) }"
            :title="`${s.repo}\n${s.running ? 'running' : 'offline'} · right click for more`"
            data-testid="sidebar-stack"
            @click="openStack(s)"
          >
            <span v-if="hub.starting[s.id]" class="spinner sm" aria-hidden="true" />
            <span v-else class="dot" :class="stackDot(s)" aria-hidden="true" />
            <span class="label truncate">{{ s.name }}</span>
            <span v-if="!s.running" class="faint small">offline</span>
          </button>
          <button
            type="button"
            class="more-btn"
            :class="{ open: menu?.s.id === s.id }"
            :aria-label="`More for ${s.name}`"
            aria-haspopup="menu"
            :aria-expanded="menu?.s.id === s.id"
            title="Start, stop, remove…"
            data-testid="sidebar-stack-menu"
            @click.stop="menu?.s.id === s.id ? closeMenu() : openMenu(s, $event)"
          >
            <Icon name="more" :size="14" />
          </button>
        </div>
        <ul v-if="hasLanes(s) && !collapsed[s.id]" class="lanes">
          <li>
            <button
              type="button"
              class="item lane"
              :class="{ active: isCurrent(s) && !stack.lane }"
              :title="`${s.repo}\nthe main working tree${servedBy(s) ? ` · its ports serve ${laneLabel(servedBy(s)!)}` : ''}`"
              data-testid="sidebar-lane-main"
              @click="openStack(s)"
            >
              <span class="dot" :class="mainDot(s)" aria-hidden="true" />
              <span class="label truncate">main</span>
              <span v-if="servedBy(s)" class="faint small" title="A lane serves the main ports">ports → lane</span>
            </button>
          </li>
          <li v-for="l in lanesOf(s)" :key="l.name">
            <button
              type="button"
              class="item lane"
              :class="{ active: isCurrent(s) && stack.lane === l.name, idle: !started(l) }"
              :title="laneTitle(s, l)"
              data-testid="sidebar-lane"
              :data-lane="l.name"
              @click="openLane(s, l)"
            >
              <span v-if="l.moving && !l.moving.error" class="spinner sm" aria-hidden="true" />
              <span v-else class="dot" :class="laneDot(s, l)" aria-hidden="true" />
              <span class="label truncate">{{ laneLabel(l) }}</span>
              <span v-if="l.node" class="pill sm info" data-testid="sidebar-lane-node">{{ nodeLabel(l) }}</span>
              <span v-if="l.owns_main" class="pill sm ok">main ports</span>
            </button>
          </li>
        </ul>
      </li>
      <li v-if="!stacks.length" class="empty">
        No stacks yet. <button type="button" class="link" @click="adding = true">Add one</button>, or run <code>devstack up</code> in a project.
      </li>
    </ul>
    <template v-if="sharedLanes.length">
      <div class="head" data-testid="sidebar-shared">Shared with you</div>
      <ul class="stacks">
        <li v-for="l in sharedLanes" :key="`${l.pool}~${l.lease}`" class="stack" :class="{ current: stack.id === sharedId(l.pool, l.lease) }">
          <div class="stack-row">
            <span class="fold-space" />
            <button
              type="button"
              class="item"
              :class="{ active: stack.id === sharedId(l.pool, l.lease) }"
              :title="sharedTitle(l)"
              data-testid="sidebar-shared-lane"
              @click="router.push({ name: 'overview', params: { stack: sharedId(l.pool, l.lease) } })"
            >
              <span class="dot on" aria-hidden="true" />
              <span class="label truncate">{{ l.repo }} · {{ l.lane }}</span>
              <span class="faint small truncate owner" data-testid="sidebar-shared-owner">{{ l.owner_name }}</span>
            </button>
            <span class="more-space" />
          </div>
        </li>
      </ul>
    </template>
    <div class="pool-block">
      <GuestTile />
      <button
        type="button"
        class="item pool-item"
        :class="{ active: route.name === 'pool' }"
        :title="pool.pending.length ? `${pool.pending.length} request(s) to join the pool` : 'The machines that run lanes for each other'"
        data-testid="sidebar-pool"
        @click="router.push({ name: 'pool' })"
      >
        <Icon name="server" :size="14" class="branch" />
        <span class="label">Pool</span>
        <span v-if="pool.isAdmin && pool.pending.length" class="pill sm warn" data-testid="sidebar-pool-badge">{{ pool.pending.length }}</span>
        <span v-else class="faint small tabular">{{ poolHint }}</span>
      </button>
    </div>
    <AddStackDialog :open="adding" @close="adding = false" />
    <Teleport to="body">
      <div
        v-if="menu"
        ref="menuEl"
        class="ctx-menu"
        role="menu"
        :aria-label="`${menu.s.name}`"
        :style="{ left: `${menu.x}px`, top: `${menu.y}px` }"
        data-testid="sidebar-stack-menu-list"
      >
        <button type="button" class="opt" role="menuitem" @click="(closeMenu(), openStack(menu!.s))"><Icon name="layers" :size="14" /> Open</button>
        <button v-if="!menu.s.running" type="button" class="opt" role="menuitem" :disabled="!!hub.starting[menu.s.id]" data-testid="sidebar-menu-start" @click="startStack(menu!.s)">
          <Icon name="play" :size="14" /> Start
        </button>
        <button v-else type="button" class="opt" role="menuitem" data-testid="sidebar-menu-stop" @click="stopStack(menu!.s)"><Icon name="power" :size="14" /> Stop…</button>
        <button type="button" class="opt" role="menuitem" @click="copyPath(menu!.s)"><Icon name="copy" :size="14" /> Copy path</button>
        <div class="sep" role="separator" />
        <button
          type="button"
          class="opt danger"
          role="menuitem"
          :disabled="menu.s.running"
          :title="menu.s.running ? 'Stop it first: a running stack is listed again at once' : 'Remove it from the dashboard; nothing on disk changes'"
          data-testid="sidebar-menu-remove"
          @click="removeStack(menu!.s)"
        >
          <Icon name="trash" :size="14" /> Remove from the dashboard…
        </button>
      </div>
    </Teleport>
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
.head-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding-right: 2px;
}
.head-btn,
.more-btn {
  display: grid;
  place-items: center;
  width: 24px;
  height: 24px;
  flex: none;
  border: 0;
  border-radius: 6px;
  background: transparent;
  color: var(--text-muted);
  cursor: pointer;
}
.head-btn:hover,
.more-btn:hover,
.more-btn.open {
  background: var(--surface-hover);
  color: var(--text);
}
/* The "…" shows on hover and focus; a right click opens the same menu. */
.more-btn {
  opacity: 0;
}
.stack-row:hover .more-btn,
.more-btn:focus-visible,
.more-btn.open {
  opacity: 1;
}
@media (hover: none) {
  .more-btn {
    opacity: 1;
  }
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
.owner {
  flex: 0 1 auto;
  max-width: 45%;
  font-weight: 400;
}
.more-space {
  width: 24px;
  flex: none;
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
.stack-row > .item {
  font-weight: 600;
}
.lane.idle {
  color: var(--text-muted);
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
.spinner.sm {
  width: 10px;
  height: 10px;
  border-width: 1.5px;
  color: var(--text-muted);
}
.dot.on {
  background: var(--ok);
  box-shadow: 0 0 0 3px color-mix(in srgb, var(--ok) 22%, transparent);
}
.dot.warn {
  background: var(--warn);
}
.dot.err {
  background: var(--danger);
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
.link {
  border: 0;
  padding: 0;
  background: none;
  color: var(--accent);
  font: inherit;
  cursor: pointer;
}
.pool-block {
  margin-top: auto;
  padding-top: 10px;
  border-top: 1px solid var(--border);
  display: flex;
  flex-direction: column;
  gap: 4px;
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
.ctx-menu {
  position: fixed;
  z-index: 60;
  min-width: 210px;
  padding: 4px;
  border: 1px solid var(--border);
  border-radius: 10px;
  background: var(--bg-elev);
  box-shadow: var(--shadow-lg, 0 8px 24px rgb(0 0 0 / 0.25));
  display: flex;
  flex-direction: column;
}
.ctx-menu .opt {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 10px;
  border: 0;
  border-radius: 6px;
  background: transparent;
  color: inherit;
  font: inherit;
  font-size: 13px;
  text-align: left;
  cursor: pointer;
}
.ctx-menu .opt:hover:not(:disabled),
.ctx-menu .opt:focus-visible {
  background: var(--surface-hover);
  outline: none;
}
.ctx-menu .opt:disabled {
  color: var(--text-faint);
  cursor: default;
}
.ctx-menu .opt.danger:not(:disabled) {
  color: var(--danger);
}
.ctx-menu .sep {
  height: 1px;
  margin: 4px 2px;
  background: var(--border);
}
</style>
