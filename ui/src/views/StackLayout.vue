<script setup lang="ts">
import { computed, watch } from 'vue';
import { useRoute } from 'vue-router';
import { ago, useNow } from '@/lib/format';
import { useHub } from '@/stores/hub';
import { useStack } from '@/stores/stack';
import Icon from '@/components/icons/Icon.vue';
import LaneBar from '@/components/LaneBar.vue';
import { laneLabel } from '@/lib/lanes';
import type { IconName } from '@/components/icons/icons';

const route = useRoute();
const hub = useHub();
const stack = useStack();
const now = useNow();

watch(
  () => route.params.stack as string | undefined,
  (id) => {
    if (id) stack.select(id);
  },
  { immediate: true },
);

const hubDown = computed(() => hub.status === 'unreachable' || hub.stream === 'error');
const starting = computed(() => !!(stack.id && hub.starting[stack.id]));
const unknown = computed(() => hub.fresh && !stack.entry && !stack.view);
/** The daemon runs another devstack than the hub: a restart of the stack
 * starts it with the hub's (the installed) one. */
const daemonOutdated = computed(() => {
  const d = stack.entry?.daemon?.version;
  const h = hub.version;
  return d && h && d !== h ? { daemon: d, hub: h } : null;
});
const restartReason = computed(() => {
  if (!stack.running) return null;
  if (daemonOutdated.value) return 'version';
  if (stack.view?.config_changed) return 'config';
  return null;
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
  { name: 'events', label: 'Events', icon: 'activity' },
];

function isActive(name: string) {
  return route.name === name || (name === 'overview' && route.name === 'service');
}
</script>

<template>
  <div>
    <nav class="subnav" aria-label="Stack views">
      <div class="subnav-inner">
        <RouterLink
          v-for="t in tabs"
          :key="t.name"
          :to="{ name: t.name, params: { stack: stack.id ?? '' } }"
          class="tab"
          :class="{ active: isActive(t.name) }"
          :aria-current="isActive(t.name) ? 'page' : undefined"
          :data-testid="`nav-${t.name}`"
        >
          <Icon :name="t.icon" :size="15" />
          {{ t.label }}
          <span v-if="t.name === 'overview' && stack.pending.length" class="badge">{{ stack.pending.length }}</span>
        </RouterLink>
        <LaneBar />
      </div>
    </nav>

    <div class="banners">
      <div v-if="stack.lane" class="banner info" data-testid="lane-banner">
        <Icon name="gitBranch" :size="18" />
        <span class="grow">
          <template v-for="l in stack.lanes.filter((x) => x.name === stack.lane)" :key="l.name">
            <strong>Worktree {{ laneLabel(l) }}</strong>
            · ports +{{ l.port_offset }} · <code>{{ l.path }}</code> · runs from a copy
            <template v-if="l.owns_main"> · serves the main ports</template>
            <template v-else> · no background workers</template>
          </template>
        </span>
        <button type="button" class="btn btn-sm" @click="stack.selectLane(null)">Show the main tree</button>
      </div>
      <div v-if="dockerDown" class="banner danger" data-testid="docker-down">
        <Icon name="box" :size="18" />
        <span class="grow">
          <template v-if="dockerDown.state === 'missing'"><strong>Docker is not installed</strong> (no <code>docker</code> on PATH). This stack's containers need it.</template>
          <template v-else-if="hub.dockerStarting"><strong>Docker is starting…</strong> The containers come up once it answers.</template>
          <template v-else><strong>Docker is not running.</strong> This stack's containers need it<template v-if="!dockerDown.starter">: {{ dockerDown.hint }}</template>.</template>
        </span>
        <button
          v-if="dockerDown.state === 'stopped' && dockerDown.starter"
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
      <div v-if="!hubDown && restartReason" class="banner warn" :data-testid="restartReason === 'version' ? 'daemon-outdated' : 'config-changed'">
        <Icon :name="restartReason === 'version' ? 'alert' : 'file'" :size="18" />
        <span v-if="restartReason === 'version'" class="grow">
          <strong>{{ stack.entry?.name ?? 'This stack' }} runs devstack {{ daemonOutdated?.daemon }}, devstack {{ daemonOutdated?.hub }} is installed.</strong>
          A restart of the stack switches it over<template v-if="stack.view?.config_changed"> and applies the changed stack file</template>.
        </span>
        <span v-else class="grow"><strong>The stack file changed since {{ stack.entry?.name ?? 'the stack' }} started.</strong> It applies after a restart.</span>
        <button type="button" class="btn btn-primary btn-sm" :disabled="!!stack.busy['stack']" data-testid="action-restart-stack" @click="stack.restartStack()">
          <span v-if="stack.busy['stack'] === 'restart'" class="spinner" /><Icon v-else name="restart" :size="14" />
          {{ stack.busy['stack'] === 'restart' ? 'Restarting…' : 'Restart stack' }}
        </button>
      </div>
      <div v-if="unknown" class="banner neutral">
        <Icon name="info" :size="18" />
        <span class="grow">This stack is not registered with the hub. It appears once its daemon starts.</span>
      </div>
      <div v-else-if="!hubDown && stack.entry && !stack.running" class="banner warn" data-testid="stack-offline">
        <Icon name="power" :size="18" />
        <span class="grow">
          <strong>{{ stack.entry.name }} is not running.</strong>
          <template v-if="stack.view"> Showing the last known state<template v-if="stack.viewAt"> from {{ ago(stack.viewAt, now) }}</template>.</template>
          <template v-else> No state has been seen yet.</template>
          <template v-if="stack.entry.start_error">
            <span class="start-error-head"><strong>The last start failed:</strong></span>
            <pre class="start-error" data-testid="stack-start-error">{{ stack.entry.start_error }}</pre>
            <span v-if="/needs devstack/.test(stack.entry.start_error)">
              The stack file needs a newer devstack.
              <button v-if="hub.newer" type="button" class="btn btn-sm" :disabled="hub.updating" @click="hub.installUpdate()">Update to {{ hub.newer }}</button>
              <template v-else>Run <code>devstack update</code>.</template>
            </span>
          </template>
        </span>
        <button type="button" class="btn btn-primary btn-sm" :disabled="starting" data-testid="action-start-stack" @click="stack.startStack()">
          <span v-if="starting" class="spinner" /><Icon v-else name="play" :size="14" />
          {{ starting ? 'Starting…' : 'Start stack' }}
        </button>
      </div>
      <div v-else-if="!hubDown && stack.running && !stack.live" class="banner info" data-testid="stack-connecting">
        <span class="spinner" />
        <span class="grow">
          {{ stack.stream === 'error' ? 'Lost the event stream; reconnecting…' : 'Connecting to the stack…' }}
          <template v-if="stack.view && stack.viewAt"> Showing the state from {{ ago(stack.viewAt, now) }}.</template>
        </span>
      </div>
    </div>

    <div :data-stale="stack.stale && !!stack.view ? 'true' : 'false'">
      <RouterView v-slot="{ Component }">
        <component :is="Component" />
      </RouterView>
    </div>
  </div>
</template>

<style scoped>
.subnav {
  border-bottom: 1px solid var(--border);
  background: var(--bg-elev);
}
.subnav-inner {
  max-width: 1280px;
  margin: 0 auto;
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
.banners {
  max-width: 1280px;
  margin: 0 auto;
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
