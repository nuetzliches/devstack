<script setup lang="ts">
import { computed, watch } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { ago, useNow } from '@/lib/format';
import { laneTab } from '@/lib/lanes';
import { laneOfParam, laneParam, laneView, stackPage } from '@/lib/stackRoutes';
import { useHub } from '@/stores/hub';
import { useStack } from '@/stores/stack';
import Icon from '@/components/icons/Icon.vue';
import StackHeader from '@/components/StackHeader.vue';
import LaneStart from '@/components/LaneStart.vue';
import { rememberTab } from '@/lib/lanes';
import SkillsBanner from '@/components/SkillsBanner.vue';
import UpgradeBanner from '@/components/UpgradeBanner.vue';
import AgentPrompt from '@/components/AgentPrompt.vue';
import type { IconName } from '@/components/icons/icons';

const route = useRoute();
// The tab shown, for the next stack or lane opened (`lastTab`).
watch(() => route.name, (n) => rememberTab(n), { immediate: true });
const router = useRouter();
const hub = useHub();
const stack = useStack();
const now = useNow();

// The lane shown is the route's (ADR-0081): `#/s/<stack>/l/<lane>/...`,
// `main` for the main working tree; the store follows it.
watch(
  () => [route.params.stack, route.params.lane] as const,
  ([id, lane]) => {
    if (typeof id !== 'string' || !id) return;
    stack.select(id, { lane: laneOfParam(lane) });
  },
  { immediate: true },
);
// And the route the store, when it shows another lane by itself (one made
// from a branch, the main tree after a lane's worktree went): that lane's
// list of the same tab, as a switch in the sidebar does.
watch(
  () => stack.lane,
  (lane) => {
    const id = route.params.stack;
    if (typeof id !== 'string' || id !== stack.id || typeof route.params.lane !== 'string' || route.params.lane === laneParam(lane)) return;
    void router.replace(laneView(id, lane, laneTab(route.name)));
  },
);

const hubDown = computed(() => hub.status === 'unreachable' || hub.stream === 'error');
/**
 * The shown lane, `main` too, when it is not started: its start options
 * instead of its views and tabs, the same for every lane and whether the
 * stack's daemon runs or not (ADR-0078, ADR-0081).
 */
const idleLane = computed(() => {
  const l = stack.shownLane;
  if (!l || l.active !== false || l.removing) return null;
  if (!stack.running && (hubDown.value || stack.readOnly || !stack.entry)) return null;
  return l;
});
const offlineStart = computed(() => !stack.running && !!idleLane.value);
/** The shown lane's worktree is gone: made again on its branch, if the branch is still there. */
const restorable = computed(() => {
  const l = stack.ending;
  return !!l?.gone && !!l.branch && hub.reachable && !!stack.branches?.branches.some((b) => b.name === l.branch && !b.worktree);
});
watch(
  () => stack.ending?.gone,
  (gone) => {
    if (gone) void stack.loadBranches();
  },
);
const unknown = computed(() => hub.fresh && !stack.entry && !stack.view);
/** What no request reports (ADR-0050): this tree's own, and what the main
 * tree knows of the lane shown (its worktree missing). */
const notices = computed(() => {
  if (!stack.running) return [];
  const own = stack.view?.notices ?? [];
  const lane = stack.lane ? (stack.lanes.find((l) => l.name === stack.lane)?.notices ?? []) : [];
  return [...own, ...lane.filter((n) => !own.some((o) => o.id === n.id))];
});
/** The shown lane runs with the main tree's stack file: its branch has none of its own (ADR-0055). */
const borrowedFile = computed(() => {
  const l = stack.shownLane;
  return l?.main_stack_file && !stack.readOnly ? l : null;
});
/** Docker is down and this stack has containers (or we do not know yet). */
const dockerDown = computed(() => {
  const d = hub.docker;
  if (!d || d.state === 'running' || hubDown.value) return null;
  if (stack.view && stack.view.infra.length === 0) return null;
  return d;
});

watch(
  () => [stack.view?.name, route.meta.title] as const,
  ([name, title]) => {
    document.title = name ? `${title && title !== 'Overview' ? `${title} · ` : ''}${name} · devstack` : 'devstack';
  },
  { immediate: true },
);

const tabs: { name: string; label: string; icon: IconName }[] = [
  { name: 'overview', label: 'Overview', icon: 'grid' },
  { name: 'infra', label: 'Infrastructure', icon: 'box' },
  { name: 'commands', label: 'Commands', icon: 'terminal' },
  { name: 'usage', label: 'Usage', icon: 'gauge' },
  { name: 'events', label: 'Activity', icon: 'activity' },
];

function isActive(name: string) {
  return route.name === name || (name === 'overview' && (route.name === 'service' || route.name === 'container'));
}
</script>

<template>
  <div>
    <StackHeader />
    <!-- A lane not started has no views yet: its start options stand in for them. -->
    <nav v-if="!idleLane" class="subnav" aria-label="Stack views">
      <div class="subnav-inner">
        <RouterLink
          v-for="t in tabs"
          :key="t.name"
          :to="laneView(stack.id ?? '', stack.lane, t.name)"
          class="tab"
          :class="{ active: isActive(t.name) }"
          :aria-current="isActive(t.name) ? 'page' : undefined"
          :data-testid="`nav-${t.name}`"
        >
          <Icon :name="t.icon" :size="15" />
          {{ t.label }}
          <span v-if="t.name === 'overview' && stack.pending.length" class="badge" :class="stack.pendingTone" data-testid="pending-badge">{{ stack.pending.length }}</span>
        </RouterLink>
      </div>
    </nav>

    <div class="banners">
      <div v-if="dockerDown" class="banner danger" data-testid="docker-down">
        <Icon name="box" :size="18" />
        <span class="grow">
          <template v-if="dockerDown.state === 'missing'"><strong>Docker is not installed</strong> (no <code>docker</code> on PATH). This stack's containers need it.</template>
          <template v-else-if="hub.dockerStarting"><strong>Docker is starting…</strong> The containers come up once it answers.</template>
          <template v-else><strong>Docker is not running.</strong> This stack's containers need it<template v-if="!dockerDown.starter">: {{ dockerDown.hint }}</template>.</template>
        </span>
        <button
          v-if="dockerDown.state === 'stopped' && dockerDown.starter && !stack.readOnly"
          type="button"
          class="btn btn-primary btn-sm"
          :disabled="hub.dockerStarting"
          data-testid="action-start-docker"
          @click="hub.startDocker()"
        >
          <span v-if="hub.dockerStarting" class="spinner" /><Icon v-else name="play" :size="14" />
          {{ hub.dockerStarting ? 'Starting…' : `Start ${dockerDown.starter}` }}
        </button>
      </div>
      <div v-if="!hubDown && stack.running && stack.view?.watcher_error" class="banner warn" data-testid="watcher-error">
        <Icon name="alert" :size="18" />
        <span class="grow"><strong>The file watcher does not see every edit.</strong> {{ stack.view.watcher_error }}</span>
      </div>
      <div v-for="n in hubDown ? [] : notices" :key="n.id" class="banner notice" :class="n.level === 'error' ? 'danger' : 'warn'" data-testid="notice" :data-notice="n.id">
        <Icon name="alert" :size="18" />
        <span class="grow">{{ n.text }} <span class="faint">({{ ago(n.since, now) }})</span></span>
      </div>
      <div v-if="!hubDown && borrowedFile" class="banner info" data-testid="lane-main-stack-file">
        <Icon name="file" :size="18" />
        <span class="grow">
          <strong>{{ borrowedFile.branch ?? 'This worktree' }} has no devstack.toml of its own.</strong>
          The lane runs with the main working tree's<template v-if="borrowedFile.node">, on {{ borrowedFile.node }} as well</template>. Where the branch differs, give it its own: a coding agent copies it and adapts it to the branch, to commit with it.
        </span>
        <AgentPrompt v-if="stack.id && hub.reachable" compact label="Give it its own" kind="lane-file" :target="{ stack: stack.id, path: borrowedFile.path }" />
      </div>
      <UpgradeBanner v-if="!hubDown && stack.entry && !stack.foreign" />
      <SkillsBanner v-if="!hubDown && stack.entry && !stack.foreign" />
      <div v-if="unknown" class="banner neutral">
        <Icon name="info" :size="18" />
        <span class="grow">This stack is not registered with the hub. It appears once its daemon starts.</span>
      </div>
      <div v-else-if="!hubDown && stack.entry && !stack.running && (!offlineStart || stack.entry.start_error)" class="banner warn" data-testid="stack-offline">
        <Icon name="power" :size="18" />
        <span class="grow">
          <strong>{{ stack.entry.name }} is not running.</strong>
          <template v-if="offlineStart" />
          <template v-else-if="stack.view"> Showing the last known state<template v-if="stack.viewAt"> from {{ ago(stack.viewAt, now) }}</template>.</template>
          <template v-else> No state has been seen yet.</template>
          <template v-if="stack.entry.start_error">
            <span class="start-error-head"><strong>The last start failed:</strong></span>
            <pre class="start-error" data-testid="stack-start-error">{{ stack.entry.start_error }}</pre>
            <span v-if="/needs devstack/.test(stack.entry.start_error)">
              The stack file needs a newer devstack.
              <button v-if="hub.newer" type="button" class="btn btn-sm" :disabled="hub.updating" @click="hub.installUpdate()">Update to {{ hub.newer }}</button>
              <template v-else>Run <code>devstack update</code>.</template>
            </span>
            <AgentPrompt v-if="!stack.readOnly && stack.id" kind="start-failed" :target="{ stack: stack.id }" />
          </template>
        </span>
        <RouterLink v-if="!stack.readOnly && !offlineStart && stack.id" :to="stackPage(stack.id)" class="btn btn-sm" data-testid="stack-page-link">
          <Icon name="layers" :size="14" /> Stack page
        </RouterLink>
      </div>
      <div v-else-if="stack.ending" class="banner warn" data-testid="lane-ending">
        <span v-if="stack.ending.removing" class="spinner" /><Icon v-else name="trash" :size="18" />
        <span class="grow">
          <template v-if="stack.ending.removing">
            <strong>The worktree of {{ stack.ending.branch ?? stack.ending.name }} was removed.</strong> devstack stops its lane and removes its containers and data.
          </template>
          <template v-else>
            <strong>The worktree of {{ stack.ending.branch ?? stack.ending.name }} was removed, and its lane with it.</strong> Showing the lane as it was last<template v-if="stack.viewAt"> ({{ ago(stack.viewAt, now) }})</template>.
          </template>
        </span>
        <button
          v-if="restorable"
          type="button"
          class="btn btn-sm"
          :disabled="!!stack.busy.branches"
          :title="`A new worktree on ${stack.ending.branch}, and its lane: it starts fresh, its data was removed`"
          data-testid="lane-restore"
          @click="stack.newLane(stack.ending.branch!)"
        >
          <span v-if="stack.busy.branches" class="spinner" /><Icon v-else name="rollback" :size="14" /> Restore worktree
        </button>
        <RouterLink :to="laneView(stack.id ?? '', null)" class="btn btn-primary btn-sm" data-testid="lane-ending-main">Show main</RouterLink>
      </div>
      <div v-else-if="!hubDown && stack.running && !stack.live" class="banner info" data-testid="stack-connecting">
        <span class="spinner" />
        <span class="grow">
          {{ stack.stream === 'error' ? 'Lost the event stream; reconnecting…' : 'Connecting to the stack…' }}
          <template v-if="stack.view && stack.viewAt"> Showing the state from {{ ago(stack.viewAt, now) }}.</template>
        </span>
      </div>
    </div>

    <div v-if="idleLane" class="idle-lane">
      <LaneStart :lane="idleLane" />
    </div>
    <div v-else :data-stale="stack.stale && !!stack.view ? 'true' : 'false'">
      <RouterView v-slot="{ Component }">
        <component :is="Component" />
      </RouterView>
    </div>
  </div>
</template>

<style scoped>
.idle-lane {
  padding: 0 var(--space-5);
}
/* Below a banner, the same gap as between banners and the page. */
.banners:has(> *) ~ .idle-lane {
  padding-top: var(--space-4);
}
.subnav {
  border-bottom: 1px solid var(--border);
  background: var(--bg-elev);
}
.subnav-inner {
  padding: 0 var(--space-5);
  display: flex;
  gap: 2px;
  overflow-x: auto;
  scrollbar-width: none;
}
.subnav-inner::-webkit-scrollbar {
  display: none;
}
@media (max-width: 640px) {
  .subnav-inner {
    padding: 0 var(--space-2);
  }
}
.tab {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  padding: 0 12px;
  height: 44px;
  color: var(--text-muted);
  font-weight: 550;
  font-size: 13px;
  border-bottom: 2px solid transparent;
  white-space: nowrap;
  text-decoration: none;
  margin-bottom: -1px;
}
.tab:hover {
  color: var(--text);
  text-decoration: none;
}
.tab.active {
  color: var(--text);
  border-bottom-color: var(--accent);
}
.tab:focus-visible {
  box-shadow: inset var(--focus);
}
.badge {
  min-width: 18px;
  height: 18px;
  padding: 0 5px;
  border-radius: 999px;
  background: var(--accent);
  color: var(--on-accent);
  font-size: 11px;
  font-weight: 700;
  display: inline-grid;
  place-items: center;
}
.badge.warn {
  background: var(--warn);
  color: #1a1204;
}
.badge.danger {
  background: var(--danger);
  color: #fff;
}
.banners {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
  padding: 0 var(--space-5);
}
.banners:has(> *) {
  padding-top: var(--space-4);
}
@media (max-width: 640px) {
  .banners {
    padding: 0 var(--space-3);
  }
}
.banners .banner {
  flex-wrap: wrap;
}
/* No button to wrap: its text stays beside the icon. */
.banners .banner.notice {
  flex-wrap: nowrap;
}
.lane-file-agent {
  margin-top: 6px;
}
.start-error-head {
  display: block;
  margin-top: 8px;
}
.start-error {
  margin: 4px 0 6px;
  padding: 8px 10px;
  border-radius: 6px;
  background: color-mix(in srgb, var(--warn) 10%, transparent);
  font-size: 12px;
  white-space: pre-wrap;
  max-height: 14em;
  overflow: auto;
}
</style>
