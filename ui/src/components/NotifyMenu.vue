<script setup lang="ts">
// Desktop notifications from the hub (ADR-0045): which kinds notify, a mute
// for the shown stack or lane, and a test. The settings are the hub's, per
// machine; this only shows and changes them.
import { computed, ref, watch } from 'vue';
import { api, paths, type NotifySettings } from '@/api';
import { usePopover } from '@/lib/popover';
import { useStack } from '@/stores/stack';
import { useUi } from '@/stores/ui';
import Icon from './icons/Icon.vue';

const stack = useStack();
const ui = useUi();
const root = ref<HTMLElement | null>(null);
const trigger = ref<HTMLButtonElement | null>(null);
const { open, toggle } = usePopover(root, trigger);

const settings = ref<NotifySettings | null>(null);
const error = ref<string | null>(null);
const busy = ref(false);

function message(body: unknown, status: number) {
  return typeof body === 'object' && body && 'error' in body ? String((body as { error: unknown }).error) : `HTTP ${status}`;
}

async function load() {
  try {
    const res = await api().request<NotifySettings>('GET', paths.notify);
    if (res.ok && typeof res.body === 'object' && res.body) {
      settings.value = res.body;
      error.value = null;
    } else error.value = message(res.body, res.status);
  } catch (e) {
    error.value = (e as Error).message;
  }
}
watch(open, (o) => {
  if (o) void load();
});
// Whether the shown stack is muted shows on the bell: load once.
void load();

async function save(change: { kinds?: Record<string, boolean>; muted?: string[] }) {
  busy.value = true;
  try {
    const res = await api().request<NotifySettings>('PUT', paths.notify, change);
    if (res.ok && typeof res.body === 'object' && res.body) settings.value = res.body;
    else ui.toast('error', 'Notifications not changed', message(res.body, res.status));
  } catch (e) {
    ui.toast('error', 'Notifications not changed', (e as Error).message);
  } finally {
    busy.value = false;
  }
}

/** The shown stack's scope, and its lane's. */
const stackScope = computed(() => stack.id ?? null);
const laneScope = computed(() => (stack.id && stack.lane ? `${stack.id}/${stack.lane}` : null));
const stackMuted = computed(() => !!stackScope.value && !!settings.value?.muted.includes(stackScope.value));
const laneMuted = computed(() => !!laneScope.value && !!settings.value?.muted.includes(laneScope.value));
const quiet = computed(() => stackMuted.value || laneMuted.value);

function setMuted(scope: string, muted: boolean) {
  const cur = new Set(settings.value?.muted ?? []);
  if (muted) cur.add(scope);
  else cur.delete(scope);
  void save({ muted: [...cur] });
}

async function test() {
  try {
    const res = await api().request('POST', paths.notifyTest);
    if (res.ok) ui.toast('info', 'Test notification sent', 'It shows on your desktop; a click on it opens the dashboard.');
    else ui.toast('error', 'No notification', message(res.body, res.status));
  } catch (e) {
    ui.toast('error', 'No notification', (e as Error).message);
  }
}

const label = computed(() => (quiet.value ? 'Notifications: muted for this stack' : 'Desktop notifications'));
</script>

<template>
  <div ref="root" class="notify-wrap">
    <button
      ref="trigger"
      type="button"
      class="btn btn-ghost btn-sm btn-icon"
      :aria-expanded="open"
      aria-haspopup="dialog"
      :aria-label="label"
      :title="label"
      data-testid="notify-open"
      @click="toggle()"
    >
      <Icon :name="quiet ? 'bellOff' : 'bell'" />
    </button>
    <div v-if="open" class="notify-pop" role="dialog" aria-label="Desktop notifications" data-testid="notify-popover">
      <p class="head">Desktop notifications</p>
      <p v-if="error" class="err small">{{ error }}</p>
      <template v-else-if="settings">
        <p v-if="!settings.available" class="warn small" data-testid="notify-unavailable">Not shown here: {{ settings.unavailable }}</p>
        <p v-if="settings.trouble" class="warn small" data-testid="notify-trouble">{{ settings.trouble }}</p>
        <ul class="kinds">
          <li v-for="k in settings.kinds" :key="k.kind">
            <label>
              <input type="checkbox" :checked="k.on" :disabled="busy" :data-kind="k.kind" @change="save({ kinds: { [k.kind]: !k.on } })" />
              <span>{{ k.label }}</span>
            </label>
          </li>
        </ul>
        <template v-if="stackScope">
          <hr />
          <label class="mute">
            <input type="checkbox" :checked="stackMuted" :disabled="busy" data-testid="notify-mute-stack" @change="setMuted(stackScope, !stackMuted)" />
            <span>Mute {{ stack.entry?.name ?? 'this stack' }}<span class="faint"> (and its lanes)</span></span>
          </label>
          <label v-if="laneScope" class="mute">
            <input type="checkbox" :checked="laneMuted" :disabled="busy || stackMuted" data-testid="notify-mute-lane" @change="setMuted(laneScope, !laneMuted)" />
            <span>Mute lane {{ stack.lane }}</span>
          </label>
        </template>
        <div class="actions">
          <button type="button" class="btn btn-sm" :disabled="!settings.available" data-testid="notify-test" @click="test()">Send a test</button>
          <span class="faint small">This machine only · <code>devstack hub notify</code></span>
        </div>
      </template>
      <p v-else class="faint small">Loading…</p>
    </div>
  </div>
</template>

<style scoped>
.notify-wrap {
  position: relative;
}
.notify-pop {
  position: absolute;
  right: 0;
  top: calc(100% + 6px);
  z-index: 40;
  width: 320px;
  padding: 12px;
  border: 1px solid var(--border);
  border-radius: 10px;
  background: var(--bg-elev);
  box-shadow: var(--shadow-lg);
  display: flex;
  flex-direction: column;
  gap: 8px;
  font-size: 13px;
}
.notify-pop p {
  margin: 0;
}
.head {
  font-weight: 600;
}
.kinds {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 6px;
}
label {
  display: flex;
  align-items: flex-start;
  gap: 8px;
  cursor: pointer;
}
input[type='checkbox'] {
  margin-top: 2px;
}
hr {
  border: 0;
  border-top: 1px solid var(--border);
  margin: 2px 0;
  width: 100%;
}
.actions {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}
.err {
  color: var(--danger);
}
.warn {
  color: var(--warn);
}
.small {
  font-size: 12px;
}
</style>
