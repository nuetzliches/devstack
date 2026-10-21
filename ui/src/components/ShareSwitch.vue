<script setup lang="ts">
// Whether this machine shares its resources with the pool (ADR-0031 point 3):
// with the switch on it offers half of what is free right now, never more
// than the caps. `compact` is the tile in the sidebar; the pool page shows the
// caps too.
import { computed, ref, watch } from 'vue';
import { bytes } from '@/lib/format';
import { usePool } from '@/stores/pool';
import { useHub } from '@/stores/hub';
import { guestLabel, guestsReserved } from '@/lib/guests';

const props = defineProps<{ compact?: boolean }>();
const pool = usePool();
const hub = useHub();
/** Lanes of other members on this machine, a few by name. */
const guests = computed(() => hub.guests);
const guestsShown = computed(() => guests.value.slice(0, 3));
const s = computed(() => pool.share);
const on = computed(() => !!s.value?.settings.enabled);
const busy = ref(false);

const GIB = 2 ** 30;
const cpuCap = ref('');
const memCap = ref('');
watch(
  () => s.value?.settings,
  (v) => {
    cpuCap.value = v?.max_cpus !== undefined ? String(v.max_cpus) : '';
    memCap.value = v?.max_memory !== undefined ? String(Math.round((v.max_memory / GIB) * 10) / 10) : '';
  },
  { immediate: true },
);

async function toggle() {
  if (!s.value) return;
  busy.value = true;
  await pool.setShare({ ...s.value.settings, enabled: !on.value });
  busy.value = false;
}

function num(v: string): number | undefined {
  const n = Number(v.replace(',', '.'));
  return v.trim() && Number.isFinite(n) && n >= 0 ? n : undefined;
}
const capsChanged = computed(() => {
  const cur = s.value?.settings;
  if (!cur) return false;
  const c = num(cpuCap.value);
  const m = num(memCap.value);
  return c !== cur.max_cpus || (m === undefined ? undefined : Math.round(m * GIB)) !== cur.max_memory;
});
async function saveCaps() {
  if (!s.value) return;
  const m = num(memCap.value);
  busy.value = true;
  await pool.setShare({ enabled: on.value, max_cpus: num(cpuCap.value), max_memory: m === undefined ? undefined : Math.round(m * GIB) });
  busy.value = false;
}

const offerText = computed(() => {
  const o = s.value?.offer;
  return o ? `${o.cpus.toFixed(1)} CPU · ${bytes(o.memory)}` : '';
});
const reported = computed(() => {
  const at = s.value?.reported_at;
  return at ? new Date(at).toLocaleTimeString() : null;
});
</script>

<template>
  <div v-if="s && (s.member || !props.compact)" class="share" :class="{ compact: props.compact, on, hosting: props.compact && guests.length }" data-testid="share">
    <div class="row">
      <button
        type="button"
        role="switch"
        class="switch"
        :aria-checked="on"
        :disabled="busy || !s.member"
        :title="s.member ? (on ? 'Stop sharing this machine' : 'Share this machine with the pool') : 'Join a pool first'"
        data-testid="share-switch"
        @click="toggle"
      >
        <span class="knob" />
      </button>
      <span class="label">
        <strong>{{ on ? 'Sharing' : 'Not sharing' }}</strong>
        <span v-if="props.compact && pool.pools.filter((p) => p.joined).length > 1" class="faint small" data-testid="share-pool">{{ pool.current }}</span>
        <span v-if="on" class="faint small tabular" data-testid="share-offer">{{ offerText }}</span>
      </span>
    </div>
    <RouterLink v-if="props.compact && guests.length" :to="{ name: 'pool' }" class="guests" title="Lanes of other members on this machine" data-testid="share-guests">
      <strong>{{ guests.length === 1 ? '1 guest lane' : `${guests.length} guest lanes` }}</strong>
      <span class="small tabular">{{ guestsReserved(guests) }} reserved</span>
      <span v-for="g in guestsShown" :key="g.lease" class="small truncate">
        {{ guestLabel(g) }}<template v-if="g.state !== 'running'"> (starting)</template>
      </span>
      <span v-if="guests.length > guestsShown.length" class="small">and {{ guests.length - guestsShown.length }} more</span>
    </RouterLink>
    <template v-if="!props.compact">
      <p class="muted small">
        With sharing on, the pool may run other members' lanes on this machine: half of the memory and CPU free right now, recomputed every 15 s,
        never more than the caps below.
      </p>
      <div class="caps">
        <label>
          <span>CPU cap (cores)</span>
          <input v-model="cpuCap" class="input" type="text" inputmode="decimal" :placeholder="`no cap (of ${s.total.cpus})`" data-testid="share-cap-cpus" />
        </label>
        <label>
          <span>Memory cap (GiB)</span>
          <input v-model="memCap" class="input" type="text" inputmode="decimal" :placeholder="`no cap (of ${bytes(s.total.memory)})`" data-testid="share-cap-memory" />
        </label>
        <button type="button" class="btn btn-sm" :disabled="!capsChanged || busy" data-testid="share-save" @click="saveCaps">Save caps</button>
      </div>
      <dl class="facts tabular">
        <dt>Free now</dt>
        <dd>{{ s.free.cpus.toFixed(1) }} CPU · {{ bytes(s.free.memory) }}</dd>
        <dt>Offered</dt>
        <dd>{{ on ? offerText : 'nothing (sharing is off)' }}</dd>
        <dt>Last report</dt>
        <dd>
          <template v-if="!s.member">none (this machine is no approved member of a pool)</template>
          <template v-else>{{ reported ?? 'not yet' }}</template>
          <span v-if="s.error" class="err"> · {{ s.error }}</span>
        </dd>
      </dl>
    </template>
  </div>
</template>

<style scoped>
.share.compact {
  padding: 8px;
  border-radius: 8px;
  background: var(--surface-hover);
}
.share.compact.on {
  background: color-mix(in srgb, var(--ok) 14%, transparent);
}
.share.compact.hosting {
  background: var(--info-soft);
}
.guests {
  display: flex;
  flex-direction: column;
  gap: 1px;
  margin-top: 6px;
  padding-top: 6px;
  border-top: 1px solid color-mix(in srgb, var(--info) 25%, transparent);
  color: var(--info-text);
  font-size: 12px;
  text-decoration: none;
  min-width: 0;
}
.guests:hover {
  text-decoration: none;
  opacity: 0.85;
}
.truncate {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
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
.switch[aria-checked='true'] {
  background: var(--ok);
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
.caps input {
  width: 170px;
}
.facts {
  display: grid;
  grid-template-columns: max-content 1fr;
  gap: 4px 16px;
  margin: 0;
  font-size: 13px;
}
.facts dt {
  color: var(--text-muted);
}
.facts dd {
  margin: 0;
}
.err {
  color: var(--danger);
}
</style>
