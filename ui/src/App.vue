<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { useRoute } from 'vue-router';
import { useHub } from '@/stores/hub';
import { SIDEBAR_DEFAULT, SIDEBAR_MAX, SIDEBAR_MIN, useUi } from '@/stores/ui';
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
const ui = useUi();
const route = useRoute();
const now = useNow();

onMounted(() => void hub.init());

// The banners below the top bar stay with it; the sidebar and other sticky
// parts start below them and are as much shorter (`--banners-h`), so a
// banner never makes the page scroll.
const banners = ref<HTMLElement | null>(null);
const bannersObserver = new ResizeObserver(([e]) => {
  document.documentElement.style.setProperty('--banners-h', `${Math.round(e?.borderBoxSize?.[0]?.blockSize ?? 0)}px`);
});
watch(banners, (el, old) => {
  if (old) bannersObserver.unobserve(old);
  if (el) bannersObserver.observe(el);
  else document.documentElement.style.setProperty('--banners-h', '0px');
});
onBeforeUnmount(() => bannersObserver.disconnect());

// The sidebar: Ctrl+B (Cmd+B) or the top bar's button shows and hides it;
// the drawer of a narrow screen closes when a link in it is followed, on
// Escape and on a click beside it. Its edge is dragged for its width, arrow
// keys move it too, a double click sets it back.
function onGlobalKey(e: KeyboardEvent) {
  if ((e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === 'b') {
    const t = e.target as HTMLElement | null;
    if (t?.closest('input, textarea, [contenteditable]')) return;
    e.preventDefault();
    ui.toggleSidebar();
  } else if (e.key === 'Escape' && ui.drawerOpen) {
    e.preventDefault();
    ui.drawerOpen = false;
  }
}
onMounted(() => window.addEventListener('keydown', onGlobalKey));
onBeforeUnmount(() => window.removeEventListener('keydown', onGlobalKey));
watch(
  () => route.fullPath,
  () => (ui.drawerOpen = false),
);

const resizing = ref(false);
function startResize(e: PointerEvent) {
  if (e.button !== 0) return;
  e.preventDefault();
  const handle = e.currentTarget as HTMLElement;
  handle.setPointerCapture(e.pointerId);
  const startX = e.clientX;
  const startW = ui.sidebarWidth;
  resizing.value = true;
  const move = (ev: PointerEvent) => ui.setSidebarWidth(startW + ev.clientX - startX, false);
  const end = () => {
    handle.removeEventListener('pointermove', move);
    handle.removeEventListener('pointerup', end);
    handle.removeEventListener('pointercancel', end);
    resizing.value = false;
    ui.setSidebarWidth(ui.sidebarWidth);
  };
  handle.addEventListener('pointermove', move);
  handle.addEventListener('pointerup', end);
  handle.addEventListener('pointercancel', end);
}
function resizeKey(e: KeyboardEvent) {
  const step = e.shiftKey ? 64 : 16;
  const to = { ArrowLeft: ui.sidebarWidth - step, ArrowRight: ui.sidebarWidth + step, Home: SIDEBAR_MIN, End: SIDEBAR_MAX }[e.key];
  if (to === undefined) return;
  e.preventDefault();
  ui.setSidebarWidth(to);
}

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
    <div ref="banners" class="banners">
      <div v-if="hubDown" class="hub-banner" role="alert" data-testid="hub-unreachable">
        <div class="hub-banner-inner">
          <Icon name="wifiOff" :size="18" />
          <div class="grow">
            <strong>devstack hub is not reachable.</strong>
            <template v-if="hub.linkUnavailable">
              Run <code>devstack hub start</code>, or <code>devstack ui</code> in a project
              <span class="last" data-testid="hub-start-unavailable">(the dashboard cannot start it: the devstack:// link is {{ hub.linkUnavailable }}).</span>
            </template>
            <template v-else>Start it here, or run <code>devstack ui</code> in a project.</template>
            <span class="last">Showing the last known state<template v-if="hub.lastContact"> from {{ ago(hub.lastContact, now) }}</template>.</span>
          </div>
          <a
            v-if="!hub.linkUnavailable"
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
    </div>
    <div
      class="shell"
      :class="{ 'side-open': ui.sidebarOpen, drawer: ui.drawerOpen, resizing }"
      :style="{ '--sidebar-w': `${ui.sidebarWidth}px` }"
    >
      <template v-if="!booting">
        <div v-if="ui.drawerOpen" class="scrim" aria-hidden="true" @click="ui.drawerOpen = false" />
        <div class="side-wrap">
          <Sidebar id="sidebar" class="side" />
          <div
            class="resizer"
            role="separator"
            aria-orientation="vertical"
            aria-label="Sidebar width"
            :aria-valuenow="ui.sidebarWidth"
            :aria-valuemin="SIDEBAR_MIN"
            :aria-valuemax="SIDEBAR_MAX"
            tabindex="0"
            title="Drag to resize, double-click to reset"
            data-testid="sidebar-resizer"
            @pointerdown="startResize"
            @dblclick="ui.setSidebarWidth(SIDEBAR_DEFAULT)"
            @keydown="resizeKey"
          />
        </div>
      </template>
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
/* Stacks and worktrees on the left on a wide screen, unless hidden; on a
   narrow one (or with it hidden) the stack switcher in the top bar does that
   job and the sidebar is a drawer over the page. */
.shell {
  display: grid;
  grid-template-columns: minmax(0, 1fr);
}
.side-wrap {
  display: none;
  position: relative;
  top: calc(var(--topbar-h) + var(--banners-h));
  height: calc(100vh - var(--topbar-h) - var(--banners-h));
}
.side {
  flex: 1;
}
.shell.drawer .side-wrap {
  display: flex;
  position: fixed;
  left: 0;
  z-index: 28;
  width: min(var(--sidebar-w), 85vw);
  box-shadow: var(--shadow-lg);
}
.scrim {
  position: fixed;
  inset: calc(var(--topbar-h) + var(--banners-h)) 0 0 0;
  z-index: 27;
  background: rgb(0 0 0 / 0.35);
}
@media (min-width: 1000px) {
  .shell.side-open {
    grid-template-columns: var(--sidebar-w) minmax(0, 1fr);
  }
  .shell.side-open .side-wrap {
    display: flex;
    position: sticky;
  }
}
.resizer {
  position: absolute;
  top: 0;
  right: -3px;
  bottom: 0;
  width: 6px;
  z-index: 1;
  cursor: col-resize;
  touch-action: none;
  outline: none;
}
.resizer::after {
  content: '';
  position: absolute;
  inset: 0 2px;
  transition: background 0.12s;
}
.resizer:hover::after,
.resizer:focus-visible::after,
.resizing .resizer::after {
  background: var(--accent);
}
.resizing {
  cursor: col-resize;
  user-select: none;
}
.banners {
  position: sticky;
  top: var(--topbar-h);
  z-index: 29;
}
.banners:empty {
  display: none;
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
