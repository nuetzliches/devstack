<script setup lang="ts">
// Setting up the share switch (ADR-0113): what sharing does, then what the
// node agent needs, checked before the switch goes on
// (`POST /hub/pool/share/check`), then the start, and whether an admin still
// has to grant the machine. Opened by the switch while it is off, and by its
// tile while its agent is not ready.
import { computed, ref, watch } from 'vue';
import type { SharePreflight, ShareSwitch } from '@/api/types';
import { bytes } from '@/lib/format';
import { usePool } from '@/stores/pool';
import Modal from './Modal.vue';
import ShareChecks from './ShareChecks.vue';
import Icon from './icons/Icon.vue';

const pool = usePool();
const s = computed(() => pool.share);
const sw = computed<ShareSwitch>(() => s.value?.switch ?? { enabled: false });
const host = computed(() => s.value?.host);

type Step = 'about' | 'setup' | 'start';
const steps: { id: Step; label: string }[] = [
  { id: 'about', label: 'What it does' },
  { id: 'setup', label: 'Set up' },
  { id: 'start', label: 'Start' },
];
const step = ref<Step>('about');

const GIB = 2 ** 30;
const cpuCap = ref('');
const memCap = ref('');
const distro = ref('');
const pre = ref<SharePreflight | null>(null);
const checking = ref(false);
/** The hub could not check (an older one): the start says what is missing. */
const unchecked = ref(false);
const busy = ref(false);
const grant = ref(true);
/** "Start sharing" went through: the last step shows what came of it. */
const sent = ref(false);

function num(v: string): number | undefined {
  const n = Number(v.replace(',', '.'));
  return v.trim() && Number.isFinite(n) && n > 0 ? n : undefined;
}
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

/** The check asked last: an earlier one's answer, slower, is dropped. */
let asked = 0;
/** The distributions the last answer listed, kept while the next check runs. */
const listed = ref<string[] | null>(null);

async function check() {
  const n = ++asked;
  checking.value = true;
  const p = await pool.checkShare(form(true));
  if (n !== asked) return;
  checking.value = false;
  unchecked.value = p === null;
  pre.value = p;
  if (p?.distros) listed.value = p.distros;
}

const distros = computed(() => {
  const d = pre.value?.distros ?? listed.value ?? host.value?.distros ?? [];
  return sw.value.distro && !d.includes(sw.value.distro) ? [...d, sw.value.distro] : d;
});
const windows = computed(() => distros.value.length > 0);
const ready = computed(() => unchecked.value || !!pre.value?.ready);

// Opened: the form from what is saved, checked at once; already switched on
// (the agent not ready), straight to what is missing.
watch(
  () => pool.shareSetup,
  (open) => {
    if (!open) return;
    const v = sw.value;
    cpuCap.value = v.max_cpus !== undefined ? String(v.max_cpus) : '';
    memCap.value = v.max_memory !== undefined ? String(Math.round((v.max_memory / GIB) * 10) / 10) : '';
    pre.value = null;
    unchecked.value = false;
    grant.value = true;
    sent.value = false;
    step.value = v.enabled ? 'setup' : 'about';
    const before = distro.value;
    distro.value = v.distro ?? '';
    // A changed distribution is checked by its watch.
    if (before === distro.value) void check();
  },
  { immediate: true },
);
// Another distribution: what was found in the one before is not its.
watch(distro, () => {
  pre.value = null;
  if (pool.shareSetup) void check();
});

const me = computed(() => pool.overview?.me);
const granted = computed(() => {
  const id = me.value?.id;
  return !!pool.overview?.members?.find((m) => m.id === id)?.node;
});
const started = computed(() => sent.value && step.value === 'start');

async function start() {
  busy.value = true;
  const ok = await pool.setShare(form(true));
  if (ok && grant.value && pool.isAdmin && !granted.value && me.value) await pool.setNode(me.value.id, me.value.name, true);
  busy.value = false;
  sent.value = ok;
}

const outcome = computed(() => {
  const h = host.value;
  if (!h || h.state === 'off' || h.state === 'starting') return { cls: 'muted', text: 'Starting the node agent…' };
  if (h.state === 'running') {
    return h.granted === false
      ? { cls: 'muted', text: `Waiting for an admin of the pool ${pool.current} to let this machine run the pool's lanes.` }
      : { cls: 'ok', text: "This machine shares: other members' lanes can run here." };
  }
  if (h.state === 'blocked' || h.state === 'failed') return { cls: 'warn', text: h.detail ?? 'The node agent did not start.' };
  return { cls: 'muted', text: h.detail ?? '' };
});

function close() {
  pool.shareSetup = false;
}
</script>

<template>
  <Modal :open="pool.shareSetup" title="Share this machine with the pool" size="md" testid="share-setup" @close="close">
    <template #icon><Icon name="power" :size="20" /></template>
    <ol class="steps">
      <li v-for="(x, i) in steps" :key="x.id" :class="{ now: x.id === step, done: steps.findIndex((y) => y.id === step) > i }">
        <span class="num">{{ i + 1 }}</span>{{ x.label }}
      </li>
    </ol>

    <div v-if="step === 'about'" data-testid="share-setup-about" class="body">
      <p>Sharing lets other members of the pool <strong>{{ pool.current }}</strong> start their lanes on this machine.</p>
      <ul class="points">
        <li>A node agent runs on this machine and offers half of the CPU and memory that is free right now, never more than the caps you set.</li>
        <li v-if="windows">On Windows it runs in a WSL distribution with Docker Engine of its own, not Docker Desktop's. The next step checks what it needs and gives the commands for what is missing.</li>
        <li>
          An admin of the pool also has to let this machine run the pool's lanes.
          <template v-if="pool.isAdmin">You are one: the last step can do that.</template>
          <template v-else>Until then nothing runs here.</template>
        </li>
        <li>Turned off, it takes no new lanes; those running go on until their owners stop them.</li>
      </ul>
    </div>

    <div v-else-if="step === 'setup'" data-testid="share-setup-checks" class="body">
      <div class="caps">
        <label v-if="windows">
          <span>WSL distribution</span>
          <select v-model="distro" class="input" data-testid="share-setup-distro">
            <option value="">choose one</option>
            <option v-for="d in distros" :key="d" :value="d">{{ d }}</option>
          </select>
        </label>
        <label>
          <span>CPU cap (cores)</span>
          <input v-model="cpuCap" class="input" type="text" inputmode="decimal" :placeholder="`no cap (of ${s?.total.cpus ?? '?'})`" />
        </label>
        <label>
          <span>Memory cap (GiB)</span>
          <input v-model="memCap" class="input" type="text" inputmode="decimal" :placeholder="`no cap (of ${s ? bytes(s.total.memory) : '?'})`" />
        </label>
      </div>
      <p v-if="checking && !pre" class="muted small"><span class="spinner" /> Checking what the node agent needs…</p>
      <p v-else-if="unchecked" class="muted small">This hub cannot check before the start: what is missing shows once sharing is on.</p>
      <template v-else-if="pre">
        <p v-if="pre.ready" class="ok small" data-testid="share-setup-ready"><Icon name="check" :size="14" /> Everything the node agent needs is there.</p>
        <p v-else-if="pre.problem" class="warn small" data-testid="share-setup-problem">{{ pre.problem }}</p>
        <ShareChecks :checks="pre.checks ?? []" :prompt="pre.prompt" />
      </template>
    </div>

    <div v-else data-testid="share-setup-start" class="body">
      <template v-if="!started">
        <dl class="summary">
          <template v-if="distro">
            <dt>WSL distribution</dt>
            <dd>{{ distro }}</dd>
          </template>
          <dt>CPU</dt>
          <dd>{{ num(cpuCap) !== undefined ? `at most ${num(cpuCap)} cores` : 'half of what is free, no cap' }}</dd>
          <dt>Memory</dt>
          <dd>{{ num(memCap) !== undefined ? `at most ${num(memCap)} GiB` : 'half of what is free, no cap' }}</dd>
        </dl>
        <label v-if="pool.isAdmin && me && !granted" class="grant">
          <input v-model="grant" type="checkbox" data-testid="share-setup-grant" />
          <span>Let this machine run the pool's lanes (you are an admin of {{ pool.current }})</span>
        </label>
        <p v-else-if="!granted" class="muted small">An admin of the pool {{ pool.current }} still has to let this machine run the pool's lanes; until then nothing runs here.</p>
      </template>
      <p v-else class="outcome" :class="outcome.cls" data-testid="share-setup-outcome">{{ outcome.text }}</p>
    </div>

    <template #footer>
      <button v-if="step !== 'about' && !started" type="button" class="btn" @click="step = step === 'start' ? 'setup' : 'about'">Back</button>
      <span class="grow" />
      <button v-if="step === 'about'" type="button" class="btn btn-primary" data-testid="share-setup-next" @click="step = 'setup'">Next</button>
      <template v-else-if="step === 'setup'">
        <button type="button" class="btn" :disabled="checking" data-testid="share-setup-recheck" @click="check"><span v-if="checking" class="spinner" /> Check again</button>
        <button type="button" class="btn btn-primary" :disabled="!ready || checking" data-testid="share-setup-next" @click="step = 'start'">Next</button>
      </template>
      <button v-else-if="!started" type="button" class="btn btn-primary" :disabled="busy" data-testid="share-setup-go" @click="start">Start sharing</button>
      <button v-else type="button" class="btn btn-primary" data-testid="share-setup-done" @click="close">Done</button>
    </template>
  </Modal>
</template>

<style scoped>
.steps {
  display: flex;
  gap: 16px;
  list-style: none;
  padding: 0;
  margin: 0 0 14px;
  font-size: 12px;
  color: var(--text-faint);
}
.steps li {
  display: flex;
  align-items: center;
  gap: 6px;
}
.steps li.now {
  color: var(--text);
  font-weight: 600;
}
.steps li.done {
  color: var(--text-muted);
}
.num {
  display: inline-grid;
  place-items: center;
  width: 18px;
  height: 18px;
  border-radius: 50%;
  border: 1px solid currentColor;
  font-size: 11px;
}
.body {
  font-size: 13px;
}
.points {
  margin: 8px 0 0;
  padding-left: 18px;
  display: flex;
  flex-direction: column;
  gap: 6px;
  color: var(--text-muted);
}
.caps {
  display: flex;
  flex-wrap: wrap;
  gap: 12px;
  margin-bottom: 10px;
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
  width: 160px;
}
.summary {
  display: grid;
  grid-template-columns: max-content 1fr;
  gap: 4px 12px;
  margin: 0 0 12px;
}
.summary dt {
  color: var(--text-muted);
}
.summary dd {
  margin: 0;
}
.grant {
  display: flex;
  gap: 8px;
  align-items: baseline;
}
.small {
  font-size: 12px;
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
.grow {
  flex: 1;
}
</style>
