<script setup lang="ts">
import { computed } from 'vue';
import { isMock } from '@/api';
import { useHub } from '@/stores/hub';
import { useStack } from '@/stores/stack';
import { useUi } from '@/stores/ui';
import StackSwitcher from './StackSwitcher.vue';
import PendingIndicator from './PendingIndicator.vue';
import Icon from './icons/Icon.vue';

const hub = useHub();
const stack = useStack();
const ui = useUi();

const conn = computed(() => {
  if (hub.status === 'unreachable' || (hub.status === 'ok' && hub.stream === 'error')) return { tone: 'danger', label: 'Hub offline', title: 'The devstack hub is not reachable' };
  if (hub.status === 'loading' || hub.stream === 'connecting') return { tone: 'neutral', label: 'Connecting', title: 'Connecting to the hub' };
  if (!stack.id) return { tone: 'ok', label: 'Hub', title: 'Connected to the hub' };
  if (!stack.running) return { tone: 'warn', label: 'Stack offline', title: 'The stack daemon is not running' };
  if (stack.live) return { tone: 'ok', label: 'Live', title: 'Receiving live events' };
  if (stack.stream === 'error') return { tone: 'warn', label: 'Reconnecting', title: 'The stack event stream dropped; reconnecting' };
  return { tone: 'neutral', label: 'Connecting', title: 'Connecting to the stack' };
});

const themeIcon = computed(() => (ui.theme === 'system' ? 'monitor' : ui.theme === 'light' ? 'sun' : 'moon'));
const themeLabel = computed(() => `Theme: ${ui.theme} (click to change)`);
const version = computed(() => hub.version ?? __APP_VERSION__);
const openPalette = () => window.dispatchEvent(new Event('devstack:palette'));

async function copyUpdate() {
  try {
    await navigator.clipboard.writeText('devstack update');
    ui.toast('info', 'Copied: devstack update', `Run it in a terminal to install ${hub.newer}; running stacks switch at their next restart.`);
  } catch {
    ui.toast('info', 'Run: devstack update', `It installs ${hub.newer}; running stacks switch at their next restart.`);
  }
}
const mismatch = computed(() => {
  const d = stack.entry?.daemon?.version;
  const h = hub.version;
  if (!d || !h) return null;
  const minor = (v: string) => v.split('.').slice(0, 2).join('.');
  return minor(d) !== minor(h) ? d : null;
});
</script>

<template>
  <header class="topbar">
    <div class="topbar-inner">
      <RouterLink to="/" class="brand" aria-label="devstack home">
        <svg width="22" height="22" viewBox="0 0 512 512" aria-hidden="true">
          <rect width="512" height="512" rx="112" fill="#0f1115" stroke="var(--border-strong)" stroke-width="16" />
          <g fill="none" stroke-linecap="round" stroke-width="40">
            <path d="M136 176h240" stroke="#5b8cff" />
            <path d="M136 256h168" stroke="#3ecf8e" />
            <path d="M136 336h240" stroke="#5b8cff" opacity=".55" />
          </g>
          <circle cx="364" cy="256" r="32" fill="#3ecf8e" />
        </svg>
        <span class="brand-name hide-xs">devstack</span>
      </RouterLink>
      <span class="sep hide-xs switcher-narrow" aria-hidden="true">/</span>
      <StackSwitcher class="switcher-narrow" />
      <span class="grow" />
      <button
        type="button"
        class="btn btn-ghost btn-sm palette-btn"
        title="Go to or run anything (Ctrl+K)"
        aria-label="Open the command palette"
        aria-keyshortcuts="Control+K Meta+K"
        data-testid="palette-open"
        @click="openPalette"
      >
        <Icon name="search" :size="14" /><kbd class="hide-sm">Ctrl K</kbd>
      </button>
      <PendingIndicator />
      <span class="conn" :class="`tone-${conn.tone}`" :title="conn.title" data-testid="connection-indicator" role="status">
        <span class="cdot" aria-hidden="true" />
        <span class="hide-sm">{{ conn.label }}</span>
        <span class="sr-only">{{ conn.title }}</span>
      </span>
      <button type="button" class="btn btn-ghost btn-sm btn-icon" :aria-label="themeLabel" :title="themeLabel" data-testid="theme-toggle" @click="ui.cycleTheme()">
        <Icon :name="themeIcon" />
      </button>
      <button
        v-if="hub.newer"
        type="button"
        class="btn btn-sm update"
        :disabled="hub.updating"
        :title="`devstack ${hub.newer} is out. Click to install it (devstack update); right-click copies the command`"
        data-testid="update-available"
        @click="hub.installUpdate()"
        @contextmenu.prevent="copyUpdate()"
      >
        <span v-if="hub.updating" class="spinner" /><Icon v-else name="download" :size="13" /> {{ hub.updating ? `Installing ${hub.newer}…` : `Update to ${hub.newer}` }}
      </button>
      <span class="ver hide-sm" :title="mismatch ? `daemon ${mismatch} differs from the hub's minor version` : 'hub version'">
        <span v-if="isMock" class="pill sm info">mock</span>
        <span>v{{ version }}</span>
        <Icon v-if="mismatch" name="alert" :size="13" class="mismatch" label="version mismatch" />
      </span>
    </div>
  </header>
</template>

<style scoped>
.palette-btn kbd {
  font-size: 11px;
  border: 1px solid var(--border);
  border-radius: 4px;
  padding: 0 4px;
  margin-left: 4px;
  font-family: inherit;
}
/* The sidebar lists the stacks on a wide screen (App.vue). */
@media (min-width: 1000px) {
  .switcher-narrow {
    display: none;
  }
}
.topbar {
  position: sticky;
  top: 0;
  z-index: 30;
  background: color-mix(in srgb, var(--bg-elev) 88%, transparent);
  backdrop-filter: saturate(1.4) blur(10px);
  border-bottom: 1px solid var(--border);
}
.topbar-inner {
  height: var(--topbar-h);
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 0 var(--space-5);
}
@media (max-width: 640px) {
  .topbar-inner {
    padding: 0 var(--space-3);
    gap: 6px;
  }
}
.brand {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  color: var(--text);
  font-weight: 700;
  letter-spacing: -0.01em;
  text-decoration: none;
  flex: none;
}
.brand:hover {
  text-decoration: none;
}
.brand-name {
  font-size: 15px;
}
.sep {
  color: var(--text-faint);
  font-size: 18px;
}
.conn {
  --tone: var(--text-faint);
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 12.5px;
  font-weight: 550;
  color: var(--text-2);
  white-space: nowrap;
  padding: 0 4px;
}
.cdot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--tone);
  box-shadow: 0 0 0 3px color-mix(in srgb, var(--tone) 22%, transparent);
}
.tone-ok {
  --tone: var(--ok);
}
.tone-warn {
  --tone: var(--warn);
}
.tone-danger {
  --tone: var(--danger);
}
.tone-neutral .cdot {
  animation: pulse 1.4s ease-in-out infinite;
}
.update {
  color: var(--accent);
  font-size: 12px;
  gap: 4px;
}
.ver {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  color: var(--text-muted);
  font-variant-numeric: tabular-nums;
}
.mismatch {
  color: var(--warn);
}
@media (max-width: 640px) {
  .hide-sm {
    display: none;
  }
}
@media (max-width: 420px) {
  .hide-xs {
    display: none;
  }
}
</style>
