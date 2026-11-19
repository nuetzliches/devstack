<script setup lang="ts">
// The share switch (ADR-0113): on, the hub runs a node agent for the current
// pool, so other members' lanes can run on this machine once an admin
// granted it; on Windows in a WSL distribution the user names, whose checks
// and setup prompt are shown here. Off while others' lanes run, the agent
// takes no new ones and ends with the last. `compact` is the sidebar tile.
// Switched on, it opens the setup dialog, which turns it on once the checks
// pass; green only while the agent runs and the machine is granted.
import { computed, onBeforeUnmount, ref, watch } from 'vue';
import type { ShareHost, ShareSwitch } from '@/api/types';
import { bytes } from '@/lib/format';
import { usePool } from '@/stores/pool';
import ShareChecks from './ShareChecks.vue';

const props = defineProps<{ compact?: boolean }>();
const pool = usePool();
const s = computed(() => pool.share);
const sw = computed<ShareSwitch>(() => s.value?.switch ?? { enabled: false });
const host = computed<ShareHost | undefined>(() => s.value?.host);
const on = computed(() => sw.value.enabled);
const busy = ref(false);

const GIB = 2 ** 30;
const cpuCap = ref('');
const memCap = ref('');
const distro = ref('');
// The form follows what is saved only when that changes (saved here or
// elsewhere): a reading every few seconds brings the same values in a new
// object, which must not undo what is being typed.
watch(
  () => [sw.value.max_cpus, sw.value.max_memory, sw.value.distro].join('|'),
  () => {
    const v = sw.value;
    cpuCap.value = v.max_cpus !== undefined ? String(v.max_cpus) : '';
    memCap.value = v.max_memory !== undefined ? String(Math.round((v.max_memory / GIB) * 10) / 10) : '';
    distro.value = v.distro ?? '';
  },
  { immediate: true },
);

function num(v: string): number | undefined {
  const n = Number(v.replace(',', '.'));
  return v.trim() && Number.isFinite(n) && n > 0 ? n : undefined;
}
/** The switch as the form says it, `enabled` as given. */
function form(enabled: boolean): ShareSwitch {
  const m = num(memCap.value);
  return {
    enabled,
    max_cpus: num(cpuCap.value),
    max_memory: m === undefined ? undefined : Math.round(m * GIB),
    distro: distro.value || undefined,
    port: sw.value.port,
  };
}
const changed = computed(() => {
  const f = form(on.value);
  const v = sw.value;
  return f.max_cpus !== v.max_cpus || f.max_memory !== v.max_memory || (f.distro ?? '') !== (v.distro ?? '');
});

async function toggle() {
  if (!s.value) return;
  if (!on.value) {
    pool.shareSetup = true;
    return;
  }
  busy.value = true;
  await pool.setShare(props.compact ? { ...sw.value, enabled: false } : form(false));
  busy.value = false;
}
async function save() {
  busy.value = true;
  await pool.setShare(form(on.value));
  busy.value = false;
}

// The agent's state moves on its own (it starts, a check passes, the last
// guest lane ends): read again while something is under way.
const timer = setInterval(() => {
  const st = host.value?.state;
  if (on.value || st === 'draining' || st === 'starting') void pool.loadShare();
}, 5000);
onBeforeUnmount(() => clearInterval(timer));

const stateText = computed(() => {
  const h = host.value;
  if (!h) return on.value ? 'On' : '';
  switch (h.state) {
    case 'running':
      return h.granted === false ? 'Waiting for an admin to grant this machine as a node' : "Runs other members' lanes";
    case 'starting':
      return 'Starting the node agent';
    case 'draining':
      return `No new lanes; ${h.guests} of others still run here`;
    case 'blocked':
      return 'Setup incomplete: finish it';
    case 'failed':
      return 'The node agent ended';
    case 'foreign':
      return 'A node agent started by hand runs';
    case 'unsupported':
      return h.detail ?? 'Not on this platform';
    default:
      return '';
  }
});
const stateClass = computed(() => {
  const st = host.value?.state;
  if (st === 'running' && host.value?.granted !== false) return 'ok';
  if (st === 'blocked' || st === 'failed') return 'warn';
  return 'muted';
});
/** Switched on, its agent does not run: the setup dialog says what is missing. */
const stuck = computed(() => on.value && (host.value?.state === 'blocked' || host.value?.state === 'failed'));
const windows = computed(() => !!host.value?.distros?.length || !!sw.value.distro);
</script>

<template>
  <div v-if="s && (s.member || !props.compact)" class="share" :class="[{ compact: props.compact, on }, on ? `tone-${stateClass}` : '']" data-testid="share">
    <div class="row">
      <button
        type="button"
        role="switch"
        class="switch"
        :class="`tone-${stateClass}`"
        :aria-checked="on"
        :disabled="busy || !s.member || host?.state === 'unsupported'"
        :title="s.member ? (on ? 'Stop sharing this machine' : 'Set up sharing: let other members of the pool run lanes on this machine') : 'Join a pool first'"
        data-testid="share-switch"
        @click="toggle"
      >
        <span class="knob" />
      </button>
      <span class="label">
        <strong>{{ on ? 'Sharing' : 'Not sharing' }}</strong>
        <span v-if="props.compact && pool.pools.filter((p) => p.joined).length > 1" class="faint small" data-testid="share-pool">{{ pool.current }}</span>
        <button v-if="stuck" type="button" class="state-link small warn" data-testid="share-state" title="Open the setup" @click="pool.shareSetup = true">{{ stateText }}</button>
        <span v-else-if="stateText" class="small" :class="stateClass" data-testid="share-state">{{ stateText }}</span>
      </span>
    </div>
    <template v-if="!props.compact">
      <p class="muted small">
        With sharing on, this machine runs a node agent: other members of the pool can start lanes here once an admin granted the machine as a node. It
        offers half of the CPU and memory free right now, never more than the caps below. Turned off, it takes no new lanes; those running go on until
        their owners stop them.
        <template v-if="windows">On Windows the agent runs in a WSL distribution with Docker Engine of its own (not Docker Desktop's).</template>
      </p>
      <div class="caps">
        <label v-if="windows">
          <span>WSL distribution</span>
          <select v-model="distro" class="input" data-testid="share-distro">
            <option value="">choose one</option>
            <option v-for="d in host?.distros ?? []" :key="d" :value="d">{{ d }}</option>
            <option v-if="sw.distro && !(host?.distros ?? []).includes(sw.distro)" :value="sw.distro">{{ sw.distro }}</option>
          </select>
        </label>
        <label>
          <span>CPU cap (cores)</span>
          <input v-model="cpuCap" class="input" type="text" inputmode="decimal" :placeholder="`no cap (of ${s.total.cpus})`" data-testid="share-cap-cpus" />
        </label>
        <label>
          <span>Memory cap (GiB)</span>
          <input v-model="memCap" class="input" type="text" inputmode="decimal" :placeholder="`no cap (of ${bytes(s.total.memory)})`" data-testid="share-cap-memory" />
        </label>
        <button type="button" class="btn btn-sm" :disabled="!changed || busy" data-testid="share-save" @click="save">Save</button>
      </div>
      <p v-if="host?.detail && host.state !== 'unsupported'" class="small" :class="stateClass" data-testid="share-detail">{{ host.detail }}</p>
      <p v-if="stuck">
        <button type="button" class="btn btn-sm btn-primary" data-testid="share-finish" @click="pool.shareSetup = true">Finish the setup</button>
      </p>
      <p v-if="host?.api && (host.state === 'running' || host.state === 'draining')" class="faint small">Other members reach its agent at {{ host.api }}.</p>
      <ShareChecks :checks="host?.checks ?? []" :prompt="host?.prompt" />
      <details v-if="host?.output?.length" class="output">
        <summary class="small">The node agent's last lines</summary>
        <pre>{{ host.output.join('\n') }}</pre>
      </details>
    </template>
  </div>
</template>

<style scoped>
.share.compact {
  padding: 8px;
  border-radius: 8px;
  background: var(--surface-hover);
}
.share.compact.tone-ok {
  background: color-mix(in srgb, var(--ok) 14%, transparent);
}
.share.compact.tone-warn {
  background: color-mix(in srgb, var(--warn) 14%, transparent);
}
.state-link {
  padding: 0;
  border: 0;
  background: none;
  font: inherit;
  font-size: 11px;
  text-align: left;
  text-decoration: underline;
  cursor: pointer;
}
.row {
  display: flex;
  align-items: center;
  gap: 10px;
}
.label {
  display: flex;
  flex-direction: column;
  min-width: 0;
  font-size: 13px;
  line-height: 1.25;
}
.small {
  font-size: 11px;
}
.ok {
  color: var(--ok);
}
.warn {
  color: var(--warn);
}
.muted {
  color: var(--text-muted);
}
.switch {
  position: relative;
  flex: none;
  width: 34px;
  height: 20px;
  border: 0;
  border-radius: 10px;
  background: var(--text-faint);
  cursor: pointer;
  padding: 0;
}
/* On, the switch says how it went: green runs, amber needs the setup. */
.switch[aria-checked='true'] {
  background: var(--accent);
}
.switch[aria-checked='true'].tone-ok {
  background: var(--ok);
}
.switch[aria-checked='true'].tone-warn {
  background: var(--warn);
}
.switch:disabled {
  opacity: 0.5;
  cursor: default;
}
.knob {
  position: absolute;
  top: 2px;
  left: 2px;
  width: 16px;
  height: 16px;
  border-radius: 50%;
  background: #fff;
  transition: transform 0.15s;
}
.switch[aria-checked='true'] .knob {
  transform: translateX(14px);
}
.caps {
  display: flex;
  flex-wrap: wrap;
  align-items: flex-end;
  gap: 12px;
  margin: 10px 0;
}
.caps label {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 12px;
  color: var(--text-muted);
}
.caps input,
.caps select {
  width: 170px;
}
.output pre {
  max-height: 200px;
  overflow: auto;
  font-size: 11px;
  margin: 6px 0 0;
}
</style>
