<script setup lang="ts">
// The stack's head on every tab: name, state, version, where it runs from and
// which worktree (branch) is shown, with a picker to switch worktrees (lanes,
// ADR-0024), the lane's actions and start/stop. Replaces the lane banner.
import { computed, onBeforeUnmount, ref } from 'vue';
import { duration, ago, useNow } from '@/lib/format';
import { laneLabel, orderLanes, nodeLabel } from '@/lib/lanes';
import { usePopover } from '@/lib/popover';
import type { LaneView } from '@/api/types';
import { useHub } from '@/stores/hub';
import { useStack } from '@/stores/stack';
import { usePool } from '@/stores/pool';
import Icon from './icons/Icon.vue';

const hub = useHub();
const stack = useStack();
const now = useNow();

const view = computed(() => stack.view);
const daemon = computed(() => stack.entry?.daemon ?? null);
const hubAway = computed(() => !hub.reachable);
const starting = computed(() => !!(stack.id && hub.starting[stack.id]));
const current = computed(() => stack.lanes.find((l) => l.name === stack.lane) ?? null);
const servedBy = computed(() => stack.lanes.find((l) => l.owns_main) ?? null);
const lanes = computed(() => orderLanes(stack.lanes));
const busyMain = computed(() => !!stack.busy['lane-main']);

/** The shown tree's state and its event stream: live, or why not. */
const liveState = computed(() => {
  if (!stack.running) return { tone: '', label: 'offline', title: 'The stack daemon is not running' };
  if (current.value && current.value.active === false) return { tone: '', label: 'not started', title: 'This worktree runs nothing yet' };
  if (stack.live) return { tone: 'ok', label: 'live', title: `Receiving live events from the ${current.value ? 'lane' : 'main working tree'}` };
  if (stack.stream === 'error') return { tone: 'warn', label: 'reconnecting', title: 'The event stream dropped; reconnecting' };
  return { tone: '', label: 'connecting', title: 'Connecting to the event stream' };
});

/** Where the shown tree lives and which branch it has. */
const path = computed(() => current.value?.path ?? view.value?.repo ?? stack.entry?.repo ?? '');
const branch = computed(() => (current.value ? current.value.branch : view.value?.branch) ?? null);
const tree = computed(() => (current.value ? laneLabel(current.value) : 'main working tree'));

// Where a started lane can move (ADR-0025 point 10): this machine, and the
// pool's nodes that are online and run lanes.
const pool = usePool();
onBeforeUnmount(pool.watch(30_000));
const moveRoot = ref<HTMLElement | null>(null);
const moveTrigger = ref<HTMLButtonElement | null>(null);
const { open: moveOpen, close: moveClose } = usePopover(moveRoot, moveTrigger);
const moveTargets = computed(() => {
  const l = current.value;
  if (!l) return [];
  const at = l.node ? nodeLabel(l) : null;
  const nodes = pool.targets
    .filter((t) => t.label !== at)
    .map((t) => ({ to: t.on, label: t.label, hint: `${t.node.report!.cpus.toFixed(1)} CPU free` }));
  return l.node ? [{ to: 'local', label: 'This machine', hint: 'here' }, ...nodes] : nodes;
});
/** A move of the shown lane under way (from the daemon: it survives navigating away). */
const moving = computed(() => {
  const m = current.value?.moving;
  if (m && !m.error && m.kind === 'move') return m;
  return null;
});
const moveFailed = computed(() => {
  const m = current.value?.moving;
  return m?.error && m.kind === 'move' ? m : null;
});
const placeName = (to: string) => (to === 'local' ? 'this machine' : to === 'pool' ? 'the pool' : to);

// Sharing the shown lane with the pool (ADR-0035): nobody, everyone, or
// members by id or tag.
const shareRoot = ref<HTMLElement | null>(null);
const shareTrigger = ref<HTMLButtonElement | null>(null);
const { open: shareOpen, close: shareClose } = usePopover(shareRoot, shareTrigger);
const shareText = ref('');
const sharedWith = computed(() => current.value?.shared_with ?? []);
const shareLabel = computed(() =>
  !sharedWith.value.length ? 'Not shared' : sharedWith.value.includes('*') ? 'Shared with the pool' : `Shared with ${sharedWith.value.join(', ')}`,
);
function share(withWhom: string[]) {
  shareClose();
  if (current.value) void stack.shareLane(current.value.name, withWhom);
}
function shareTags() {
  share(
    shareText.value
      .split(',')
      .map((t) => t.trim())
      .filter(Boolean),
  );
}
function moveTo(to: string) {
  moveClose();
  if (current.value) void stack.moveLane(current.value.name, to);
}

const root = ref<HTMLElement | null>(null);
const trigger = ref<HTMLButtonElement | null>(null);
const { open, close } = usePopover(root, trigger);

function pick(l: LaneView | null) {
  close();
  // A lane not started is shown with its start options, not started here.
  stack.selectLane(l?.name ?? null);
}
</script>

<template>
  <header class="stack-head" data-testid="stack-header">
    <div class="line1">
      <h1 class="truncate">{{ view?.name ?? stack.entry?.name ?? 'Stack' }}</h1>
      <span v-if="hubAway" class="pill" title="The hub is not reachable"><span class="dot" />last seen {{ ago(stack.viewAt ?? hub.lastContact, now) }}</span>
      <span v-else class="pill" :class="liveState.tone" :title="liveState.title" data-testid="stack-live" role="status"><span class="dot" />{{ liveState.label }}</span>
      <span v-if="daemon" class="tag" title="daemon version">devstack {{ daemon.version }}</span>
      <span class="grow" />
      <template v-if="!hubAway">
        <button
          v-if="current && !current.owns_main && current.active !== false"
          type="button"
          class="btn btn-sm"
          :disabled="busyMain"
          title="This worktree's services, dev servers included, answer on the main ports; workers move with them"
          data-testid="lane-take-main"
          @click="stack.laneMain(current.name)"
        >
          <span v-if="busyMain" class="spinner" /><Icon v-else name="promote" :size="14" /> Serve main ports
        </button>
        <button v-if="servedBy" type="button" class="btn btn-sm" :disabled="busyMain" :title="`${laneLabel(servedBy)} serves the main ports`" data-testid="lane-main-back" @click="stack.laneMain('main')">
          <Icon name="rollback" :size="14" /> Main ports back
        </button>
        <div v-if="current && current.node" ref="shareRoot" class="picker" data-testid="lane-share">
          <button
            ref="shareTrigger"
            type="button"
            class="btn btn-sm"
            :class="{ 'btn-primary': sharedWith.length }"
            :aria-expanded="shareOpen"
            title="Who in the pool sees this lane and may open its services (ADR-0035)"
            @click="
              shareText = sharedWith.filter((w) => w !== '*').join(', ');
              shareOpen = !shareOpen;
            "
          >
            <Icon name="globe" :size="13" /> {{ shareLabel }}
          </button>
          <div v-if="shareOpen" class="menu" role="menu">
            <button type="button" class="opt" role="menuitem" data-testid="lane-share-none" @click="share([])">
              <span>Nobody</span><span class="faint small">only you</span>
            </button>
            <button type="button" class="opt" role="menuitem" data-testid="lane-share-all" @click="share(['*'])">
              <span>Everyone in the pool</span><span class="faint small">every member</span>
            </button>
            <form class="share-tags" @submit.prevent="shareTags">
              <input v-model="shareText" class="input" placeholder="team:qa, customer:acme" data-testid="lane-share-tags" />
              <button type="submit" class="btn btn-xs btn-primary">Share</button>
            </form>
            <p class="faint small share-note">They open its services through its node; they do not control the lane.</p>
          </div>
        </div>
        <div v-if="current && !current.owns_main && current.active !== false && (moveTargets.length || moving)" ref="moveRoot" class="picker">
          <button
            ref="moveTrigger"
            type="button"
            class="btn btn-sm"
            :disabled="!!moving"
            :aria-expanded="moveOpen"
            aria-haspopup="menu"
            title="Run this worktree's lane elsewhere: a cold start there that takes over once it is ready"
            data-testid="lane-move"
            @click="moveOpen = !moveOpen"
          >
            <span v-if="moving" class="spinner" /><Icon v-else name="server" :size="14" />
            {{ moving ? `Moving to ${placeName(moving.to)}… ${moving.step}/${moving.steps}` : 'Move' }}
          </button>
          <div v-if="moveOpen" class="menu menu-right" role="menu">
            <button v-for="t in moveTargets" :key="t.to" type="button" class="opt" role="menuitem" data-testid="lane-move-target" @click="moveTo(t.to)">
              <span class="grow">{{ t.label }}</span>
              <span class="faint">{{ t.hint }}</span>
            </button>
          </div>
        </div>
        <button
          v-if="current && !current.owns_main && current.active !== false"
          type="button"
          class="btn btn-sm btn-stop"
          :title="`Stop ${laneLabel(current)}: its services and containers stop; the worktree stays`"
          data-testid="lane-stop"
          @click="stack.stopLane(current.name)"
        >
          <Icon name="stop" :size="14" /> Stop worktree
        </button>
        <template v-if="!current">
          <button v-if="!stack.running" type="button" class="btn btn-primary btn-sm" :disabled="starting || !stack.entry" data-testid="action-stack-start" @click="stack.startStack()">
            <span v-if="starting" class="spinner" /><Icon v-else name="play" :size="14" /> Start stack
          </button>
          <button v-else type="button" class="btn btn-sm btn-stop" :disabled="!!stack.busy.stack" data-testid="action-stack-stop" @click="stack.stopStack()">
            <span v-if="stack.busy.stack" class="spinner" /><Icon v-else name="power" :size="14" /> Stop stack
          </button>
        </template>
      </template>
    </div>
    <div class="line2">
      <span class="path truncate mono" :title="path">{{ path }}</span>
      <div ref="root" class="picker">
        <button
          ref="trigger"
          type="button"
          class="tree"
          :aria-expanded="open"
          aria-haspopup="listbox"
          :title="stack.lanes.length ? 'Switch the worktree shown' : 'The branch checked out here'"
          data-testid="worktree-picker"
          @click="open = !open"
        >
          <Icon name="gitBranch" :size="13" />
          <span class="branch">{{ branch ?? 'detached' }}</span>
          <span class="muted">· {{ tree }}</span>
          <span v-if="current" class="muted">· ports +{{ current.port_offset }}</span>
          <Icon v-if="stack.lanes.length" name="chevronDown" :size="13" class="muted" />
        </button>
        <div v-if="open && stack.lanes.length" class="menu" role="listbox" aria-label="Worktrees">
          <button type="button" class="opt" :class="{ on: !stack.lane }" role="option" :aria-selected="!stack.lane" data-testid="worktree-option" @click="pick(null)">
            <span class="dot on" /><span class="grow truncate">main working tree</span>
            <span v-if="!servedBy" class="pill sm">main ports</span>
          </button>
          <button
            v-for="l in [...lanes.running, ...lanes.idle]"
            :key="l.name"
            type="button"
            class="opt"
            :class="{ on: stack.lane === l.name, idle: l.active === false }"
            role="option"
            :aria-selected="stack.lane === l.name"
            :title="l.path"
            data-testid="worktree-option"
            @click="pick(l)"
          >
            <span class="dot" :class="l.active === false ? 'off' : l.running ? 'on' : 'off'" />
            <span class="grow truncate">{{ laneLabel(l) }}</span>
            <span v-if="l.node" class="pill sm info" :title="`Runs on ${nodeLabel(l)}`">{{ nodeLabel(l) }}</span>
            <span v-if="l.owns_main" class="pill sm ok">main ports</span>
            <span v-else-if="l.active === false" class="faint">start</span>
            <span v-else class="faint tabular">{{ l.running }}/{{ l.services }}</span>
          </button>
        </div>
      </div>
      <span v-if="daemon && !current" class="muted small">daemon pid {{ daemon.pid }} · port {{ daemon.port }} · up {{ duration(now - daemon.started) }}</span>
      <span v-if="moving" class="muted small move-line" data-testid="lane-moving" :title="moving.what">
        <span class="bar"><span :style="{ width: `${(100 * moving.step) / moving.steps}%` }" /></span>
        {{ moving.what }}
      </span>
      <span v-else-if="moveFailed" class="err small" data-testid="lane-move-failed" :title="moveFailed.error">
        Moving to {{ placeName(moveFailed.to) }} failed: {{ moveFailed.error }}
      </span>
      <span v-else-if="current && current.active !== false" class="muted small" data-testid="lane-placement">
        <template v-if="current.node">runs on <strong>{{ nodeLabel(current) }}</strong> · </template>
        <template v-else>runs on this machine · </template>
        {{ current.owns_main ? 'serves the main ports' : 'no background workers' }}
        <span v-if="current.node_error" class="err" :title="current.node_error"> · {{ current.node_error }}</span>
        <span v-if="current.node_warning" class="warn-text" :title="current.node_warning" data-testid="lane-node-warning"> · {{ current.node_warning }}</span>
      </span>
    </div>
  </header>
</template>

<style scoped>
.share-tags {
  display: flex;
  gap: 6px;
  padding: 6px 8px;
}
.share-tags .input {
  flex: 1;
  min-width: 160px;
}
.share-note {
  padding: 0 8px 6px;
  margin: 0;
  max-width: 260px;
}
.stack-head {
  padding: 14px var(--space-5) 10px;
  border-bottom: 1px solid var(--border);
  background: var(--bg-elev);
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.line1,
.line2 {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
  flex-wrap: wrap;
}
h1 {
  font-size: 20px;
  margin: 0;
  min-width: 0;
}
.path {
  font-size: 12.5px;
  color: var(--text-2);
  max-width: 48ch;
}
.err {
  color: var(--danger);
}
.warn-text {
  color: var(--warn-text);
}
.small {
  font-size: 12px;
}
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
.branch {
  font-weight: 600;
}
.menu {
  position: absolute;
  top: calc(100% + 6px);
  left: 0;
  z-index: 40;
  min-width: 300px;
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
.move-line {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
}
.bar {
  width: 90px;
  height: 5px;
  border-radius: 3px;
  background: var(--surface-hover);
  overflow: hidden;
  flex: none;
}
.bar > span {
  display: block;
  height: 100%;
  background: var(--accent);
  transition: width 0.3s;
}
.menu-right {
  left: auto;
  right: 0;
  min-width: 240px;
}
.opt {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 7px 8px;
  border: 0;
  border-radius: 8px;
  background: transparent;
  color: inherit;
  font: inherit;
  font-size: 13px;
  text-align: left;
  cursor: pointer;
}
.opt:hover {
  background: var(--surface-hover);
}
.opt.on {
  background: var(--accent-soft);
  color: var(--accent-text);
}
.opt.idle {
  color: var(--text-muted);
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
</style>
