<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import { useHub } from '@/stores/hub';
import { ago, useNow } from '@/lib/format';
import TopBar from '@/components/TopBar.vue';
import Toasts from '@/components/Toasts.vue';
import GlobalDialogs from '@/components/GlobalDialogs.vue';
import CommandPalette from '@/components/CommandPalette.vue';
import Icon from '@/components/icons/Icon.vue';
import Sidebar from '@/components/Sidebar.vue';
import GuestBanner from '@/components/GuestBanner.vue';
import UnpairedView from '@/views/UnpairedView.vue';

const hub = useHub();
const now = useNow();

onMounted(() => void hub.init());

const hubDown = computed(() => hub.status === 'unreachable' || (hub.status === 'ok' && hub.stream === 'error'));
const booting = computed(() => hub.status === 'loading' && !hub.stacks.length);

// Nothing listens while the hub is down, so the page cannot ask it to start.
// The `devstack://hub/start` link asks the system instead (registered by
// `devstack up` / `devstack hub install`); the browser confirms it once.
// Afterwards look for the hub for a short while instead of waiting for the
// stream's backoff.
const startingHub = ref(false);
async function startHub() {
  startingHub.value = true;
  const until = Date.now() + 20_000;
  while (Date.now() < until) {
    await new Promise((r) => setTimeout(r, 1000));
    await hub.checkSession();
    if (hub.status === 'ok') {
      await hub.init();
      break;
    }
  }
  startingHub.value = false;
}
</script>

<template>
  <UnpairedView v-if="hub.status === 'unpaired'" />
  <template v-else>
    <a class="skip" href="#main">Skip to content</a>
    <TopBar />
    <div v-if="hubDown" class="hub-banner" role="alert" data-testid="hub-unreachable">
      <div class="hub-banner-inner">
        <Icon name="wifiOff" :size="18" />
        <div class="grow">
          <strong>devstack hub is not reachable.</strong> Start it here, or run <code>devstack ui</code> in a project.
          <span class="last">Showing the last known state<template v-if="hub.lastContact"> from {{ ago(hub.lastContact, now) }}</template>.</span>
        </div>
        <a
          class="btn btn-primary btn-sm"
          href="devstack://hub/start"
          title="Needs the devstack:// link (`devstack hub install`, done by `devstack up`)"
          data-testid="action-start-hub"
          @click="startHub()"
        >
          <span v-if="startingHub" class="spinner" /><Icon v-else name="play" :size="14" />
          {{ startingHub ? 'Starting…' : 'Start hub' }}
        </a>
        <span class="retry"><span class="spinner" aria-hidden="true" /> retrying</span>
      </div>
    </div>
    <GuestBanner v-if="!hubDown" />
    <div class="shell">
      <Sidebar v-if="!booting" class="side" />
      <main id="main" tabindex="-1">
        <div v-if="booting" class="boot" aria-busy="true">
          <span class="spinner" /> Connecting to the devstack hub…
        </div>
        <RouterView v-else />
      </main>
    </div>
    <Toasts />
    <GlobalDialogs />
    <CommandPalette />
  </template>
</template>

<style scoped>
.skip {
  position: absolute;
  left: -9999px;
  top: 8px;
  z-index: 100;
  background: var(--accent);
  color: var(--on-accent);
  padding: 6px 12px;
  border-radius: 6px;
}
.skip:focus {
  left: 8px;
}
main {
  outline: none;
  min-width: 0;
}
/* Stacks and worktrees on the left on a wide screen; the stack switcher in
   the top bar does that job on a narrow one. */
.shell {
  display: grid;
  grid-template-columns: minmax(0, 1fr);
}
.side {
  display: none;
}
@media (min-width: 1000px) {
  .shell {
    grid-template-columns: 248px minmax(0, 1fr);
  }
  .side {
    display: flex;
    position: sticky;
    top: var(--topbar-h);
    height: calc(100vh - var(--topbar-h));
  }
}
.hub-banner {
  background: var(--danger-soft);
  border-bottom: 1px solid color-mix(in srgb, var(--danger) 30%, transparent);
  color: var(--danger-text);
}
.hub-banner-inner {
  max-width: 1280px;
  margin: 0 auto;
  padding: 10px var(--space-5);
  display: flex;
  align-items: center;
  gap: 12px;
  font-size: 13px;
}
@media (max-width: 640px) {
  .hub-banner-inner {
    padding: 10px var(--space-3);
    align-items: flex-start;
  }
}
.hub-banner code {
  background: color-mix(in srgb, var(--danger) 14%, transparent);
  color: inherit;
}
.last {
  color: var(--text-2);
  margin-left: 4px;
}
.retry {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  white-space: nowrap;
}
.retry .spinner {
  width: 12px;
  height: 12px;
}
.boot {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 10px;
  min-height: 50vh;
  color: var(--text-muted);
}
</style>
