<script setup lang="ts">
// The pool (ADR-0031, ADR-0032): this machine's membership (and joining one),
// the requests to join, the nodes with their lanes, the members. An admin
// approves, rejects and removes members, changes roles, pauses nodes and ends
// leases here; the coordinator checks the role, this page only hides what the
// role does not allow.
import { computed, onBeforeUnmount, ref } from 'vue';
import { ago, bytes } from '@/lib/format';
import { guestsReserved } from '@/lib/guests';
import { connectedTo, usePool } from '@/stores/pool';
import { sharedId } from '@/api';
import Icon from '@/components/icons/Icon.vue';
import CapacityChart from '@/components/CapacityChart.vue';
import type { PoolLease, PoolMember, PoolRelease } from '@/api/types';
import { useUi } from '@/stores/ui';
import { useHub } from '@/stores/hub';

const pool = usePool();
const stop = pool.watch(10_000);
onBeforeUnmount(stop);

const now = ref(Date.now());
const tick = setInterval(() => (now.value = Date.now()), 15_000);
onBeforeUnmount(() => clearInterval(tick));

const o = computed(() => pool.overview);
const me = computed(() => o.value?.me);
const members = computed(() => (o.value?.members ?? []).filter((m) => m.state === 'approved'));
const removed = computed(() => (o.value?.members ?? []).filter((m) => m.state === 'removed').length);
const nodes = computed(() => o.value?.nodes ?? []);
/** Lanes of other members on this machine, every pool (the hub's `guests`). */
const guests = computed(() => hub.guests);
const guestPools = computed(() => new Set(guests.value.map((g) => g.pool)).size > 1);
const audit = computed(() => (o.value?.audit ?? []).slice(0, 50));
const showRemoved = ref(false);
const removedList = computed(() => (o.value?.members ?? []).filter((m) => m.state === 'removed'));

const asked = (m: PoolMember) => ago(m.requested_at * 1000, now.value);
const isMe = (id: string) => me.value?.id === id;
/** The pool's only admin, who must stay one (the coordinator refuses otherwise). */
const lastAdmin = (m: PoolMember) => m.role === 'admin' && members.value.filter((x) => x.role === 'admin').length === 1;
const lastAdminTitle = 'The pool needs an admin: make another member admin first';

const actionText: Record<string, string> = {
  approve: 'approved',
  reject: 'rejected the request of',
  remove: 'removed',
  role: 'changed the role of',
  pause: 'paused',
  resume: 'resumed',
  end: 'ended a lane on',
  tags: 'changed the tags of',
  policy: 'changed the policy of',
  node: 'changed whether lanes run on',
};

// Joining a pool from here, as `devstack join` does; another one beside
// the default pool under a name of its own (ADR-0034).
const joinUrl = ref('');
const joinFp = ref('');
const joinName = ref('');
const joinPool = ref('');
const joining = ref(false);
const addingPool = ref(false);
const poolNameOk = computed(() => /^[a-z0-9_-]{1,32}$/.test(joinPool.value.trim()) && !pool.pools.some((p) => p.name === joinPool.value.trim() && p.joined));
async function join() {
  joining.value = true;
  const as = addingPool.value ? joinPool.value.trim() : pool.current;
  if (await pool.join(joinUrl.value.trim(), joinFp.value.trim(), joinName.value.trim(), as)) addingPool.value = false;
  joining.value = false;
}
const joinedPools = computed(() => pool.pools.filter((p) => p.joined));
// What the pool has, offers and uses now (ADR-0036), over the nodes online.
const capacity = computed(() => {
  const on = nodes.value.filter((n) => n.online && n.report);
  const sum = (f: (n: (typeof on)[number]) => number) => on.reduce((a, n) => a + f(n), 0);
  return {
    cpus: sum((n) => n.report!.total?.cpus ?? 0),
    memory: sum((n) => n.report!.total?.memory ?? 0),
    offeredCpus: sum((n) => (n.report!.sharing ? n.report!.cpus : 0)),
    offeredMemory: sum((n) => (n.report!.sharing ? n.report!.memory : 0)),
    usedCpus: sum((n) => n.report!.used?.cpus ?? 0),
    usedMemory: sum((n) => n.report!.used?.memory ?? 0),
    gpus: sum((n) => n.report!.gpus?.length ?? 0),
    gpusOffered: sum((n) => n.report!.gpus?.filter((g) => g.offered).length ?? 0),
  };
});
/** The node whose history is open. */
const openNode = ref<string | null>(null);
/** Lanes others share with this machine in the shown pool (ADR-0035). */
const sharedHere = computed(() => pool.shared.filter((l) => l.pool === pool.current));

const ui = useUi();
async function copyJoin() {
  const c = o.value?.join?.command;
  if (!c) return;
  await navigator.clipboard.writeText(c);
  ui.toast('info', 'Copied: run it on the machine that is to join');
}

const leases = computed(() => o.value?.leases ?? []);
const nameOf = (id: string) => (o.value?.members ?? []).find((m) => m.id === id)?.name ?? nodes.value.find((n) => n.id === id)?.name ?? id;
const lanesOn = (node: string) => leases.value.filter((l) => l.node === node).length;
// Which devstack a node or the coordinator runs, beside this machine's (the
// hub's): another protocol does not work together, another version may
// behave differently (ADR-0031, amended 2026-10-19).
const hub = useHub();
function releaseClass(r: PoolRelease | undefined, reported: boolean) {
  if (!r) return reported ? 'warn' : 'outline';
  if (hub.version && r.version !== hub.version) return 'warn';
  return 'outline';
}
function releaseTitle(r: PoolRelease | undefined, what: string) {
  const here = hub.version ? `this machine runs ${hub.version}` : '';
  if (!r) return `${what} does not say which devstack it runs: an older one. ${here}`.trim();
  const same = !hub.version || r.version === hub.version;
  return `${what} runs devstack ${r.version} (pool protocol ${r.protocol})${same ? '' : `; ${here}`}`;
}

const kindOf = (id: string) => nodes.value.find((n) => n.id === id)?.kind ?? 'unknown';
const kindText: Record<string, string> = { server: 'server', node: 'node', machine: 'developer machine', unknown: 'not seen yet' };
const kindTitle: Record<string, string> = {
  server: 'Runs lanes for the pool and never reclaims them (a dedicated node)',
  node: 'Runs lanes for the pool; its owner may reclaim them',
  machine: 'A developer machine: asks for lanes, may share',
  unknown: 'Has not reported to the coordinator yet',
};
const leaseWhat = (l: PoolLease) => `lane ${l.repo}/${l.lane} on ${nameOf(l.node)}`;

// Tags of a member and the policy of a node (ADR-0033), edited inline.
const memberOf = (id: string) => (o.value?.members ?? []).find((m) => m.id === id);
const editTags = ref<string | null>(null);
const tagsText = ref('');
function startTags(m: PoolMember) {
  editTags.value = m.id;
  tagsText.value = (m.tags ?? []).join(', ');
}
async function saveTags(m: PoolMember) {
  const tags = tagsText.value.split(',').map((t) => t.trim()).filter(Boolean);
  await pool.setTags(m.id, m.name, tags);
  editTags.value = null;
}
const editPolicy = ref<string | null>(null);
const pol = ref({ allow: '', repos: '', headroom: '', cpus: '', share: '', network: 'lan' as 'lan' | 'internet' | 'none' });
function startPolicy(id: string) {
  const p = memberOf(id)?.policy;
  pol.value = {
    allow: (p?.allow ?? []).join(', '),
    repos: (p?.repos ?? []).join(', '),
    headroom: p?.memory_headroom !== undefined ? String(p.memory_headroom) : '',
    cpus: p?.max_cpus !== undefined ? String(p.max_cpus) : '',
    share: p?.share_percent !== undefined ? String(p.share_percent) : '',
    network: p?.network ?? 'lan',
  };
  editPolicy.value = id;
}
const cpusOf = (t: string) => {
  const c = Number(t.replace(',', '.'));
  return t.trim() && Number.isFinite(c) && c > 0 ? c : undefined;
};
const list = (t: string) => t.split(',').map((x) => x.trim()).filter(Boolean);
async function savePolicy(id: string, name: string, clear = false) {
  const h = Number(pol.value.headroom.replace(',', '.'));
  const share = Math.round(Number(pol.value.share));
  await pool.setPolicy(
    id,
    name,
    clear
      ? null
      : {
          // What the form does not edit stays as it is.
          ...memberOf(id)?.policy,
          allow: list(pol.value.allow),
          repos: list(pol.value.repos),
          memory_headroom: pol.value.headroom.trim() && Number.isFinite(h) && h > 0 ? h : undefined,
          max_cpus: cpusOf(pol.value.cpus),
          share_percent: pol.value.share.trim() && share >= 1 && share <= 100 ? share : undefined,
          network: pol.value.network,
        },
  );
  editPolicy.value = null;
}
function policyText(id: string) {
  const p = memberOf(id)?.policy;
  if (!p) return 'everyone, any repository';
  const who = p.allow?.length ? p.allow.join(', ') : 'everyone';
  const what = p.repos?.length ? p.repos.join(', ') : 'any repository';
  const net =
    p.network === 'none'
      ? ' · no network outside the lane'
      : p.network === 'internet'
        ? ' · internet, not the local network'
        : '';
  const mem =
    (p.memory_headroom ? ` · memory need ×${p.memory_headroom}` : '') +
    (p.max_cpus ? ` · at most ${p.max_cpus} CPU per lane` : '') +
    (p.share_percent ? ` · offers ${p.share_percent}% of what is free` : '');
  return `${who} · ${what}${mem}${net}`;
}
</script>

<template>
  <div class="page pool" data-testid="pool">
    <nav v-if="joinedPools.length > 1 || addingPool || pool.overview?.joined" class="pool-tabs" data-testid="pool-tabs">
      <button
        v-for="p in joinedPools"
        :key="p.name"
        type="button"
        class="tab"
        :class="{ on: p.name === pool.current && !addingPool }"
        :title="p.coordinator ?? ''"
        data-testid="pool-tab"
        @click="
          addingPool = false;
          pool.select(p.name);
        "
      >
        {{ p.name }}<span v-if="p.state === 'pending'" class="faint"> · waiting</span>
      </button>
      <button type="button" class="tab" :class="{ on: addingPool }" title="Join another pool, e.g. a customer's (ADR-0034)" data-testid="pool-add" @click="addingPool = true">
        <Icon name="plus" :size="13" /> Another pool
      </button>
    </nav>

    <section v-if="addingPool" class="card" data-testid="pool-join-another">
      <header class="card-header">
        <Icon name="key" :size="18" />
        <h2>Join another pool</h2>
      </header>
      <div class="card-body">
        <p class="muted small">
          A pool of another organisation (a customer's servers, say), beside this machine's other pools. Each pool decides alone whom it
          admits; they do not trust each other. The name is yours: it is how lanes name it (<code>--on name:node</code>).
        </p>
        <form class="join" @submit.prevent="join">
          <label>
            <span>Name here</span>
            <input v-model="joinPool" class="input" placeholder="customer" required data-testid="pool-join-name" />
          </label>
          <label>
            <span>Coordinator</span>
            <input v-model="joinUrl" class="input" placeholder="https://10.0.0.5:4260" required />
          </label>
          <label class="wide">
            <span>Fingerprint</span>
            <input v-model="joinFp" class="input mono" placeholder="64 hex digits" required />
          </label>
          <label>
            <span>This machine's name (optional)</span>
            <input v-model="joinName" class="input" placeholder="this machine's name" />
          </label>
          <button type="submit" class="btn btn-primary btn-sm" :disabled="joining || !joinUrl || !joinFp || !poolNameOk">
            <span v-if="joining" class="spinner" /><Icon v-else name="key" :size="14" /> Ask to join
          </button>
          <button type="button" class="btn btn-sm btn-ghost" @click="addingPool = false">Cancel</button>
        </form>
      </div>
    </section>

    <section v-if="!addingPool" class="card">
      <header class="card-header">
        <Icon name="server" :size="18" />
        <h2>Pool<template v-if="pool.current !== 'default'"> {{ pool.current }}</template></h2>
        <span v-if="me?.role" class="pill sm" :class="me.role === 'admin' ? 'accent' : 'outline'" data-testid="pool-role">{{ me.role }}</span>
      </header>
      <div class="card-body">
        <p v-if="pool.error && !o" class="err" data-testid="pool-error"><Icon name="alert" :size="14" /> {{ pool.error }}</p>
        <template v-else-if="o && !o.joined">
          <p>This machine is in no pool. A pool lets machines run lanes for each other (remote lanes).</p>
          <p class="muted small">
            A member of the pool copies its join command from this page (or ask its admin for the coordinator's address and
            fingerprint). The fingerprint proves the coordinator is the one it claims; an admin then approves this machine.
          </p>
          <form class="join" data-testid="pool-join" @submit.prevent="join">
            <label>
              <span>Coordinator</span>
              <input v-model="joinUrl" class="input" placeholder="https://10.0.0.5:4260" required data-testid="pool-join-url" />
            </label>
            <label class="wide">
              <span>Fingerprint</span>
              <input v-model="joinFp" class="input mono" placeholder="64 hex digits" required data-testid="pool-join-fp" />
            </label>
            <label>
              <span>Name (optional)</span>
              <input v-model="joinName" class="input" placeholder="this machine's name" />
            </label>
            <button type="submit" class="btn btn-primary btn-sm" :disabled="joining || !joinUrl || !joinFp" data-testid="pool-join-submit">
              <span v-if="joining" class="spinner" /><Icon v-else name="key" :size="14" /> Ask to join
            </button>
          </form>
        </template>
        <template v-else-if="me">
          <dl class="facts">
            <dt>Coordinator</dt>
            <dd>
              <code>{{ me.coordinator }}</code>
              <span
                v-if="pool.share?.reported_at"
                class="pill sm"
                :class="releaseClass(pool.share.coordinator, true)"
                :title="releaseTitle(pool.share.coordinator, 'The coordinator')"
                data-testid="pool-coordinator-version"
              >{{ pool.share.coordinator?.version ?? 'older' }}</span>
            </dd>
            <dt>This machine</dt>
            <dd>{{ me.name }} <span class="faint mono">{{ me.id }}</span></dd>
          </dl>
          <p v-if="me.state === 'pending'" class="warn-line" data-testid="pool-waiting">
            <Icon name="hourglass" :size="14" /> Waiting for an admin to approve this machine.
          </p>
          <p v-else-if="me.state === 'removed'" class="err"><Icon name="xCircle" :size="14" /> This machine was removed from the pool.</p>
          <div v-if="me.state === 'approved' && o?.join" class="join-cmd" data-testid="pool-join-command">
            <span class="muted small">Another machine joins with</span>
            <code class="mono">{{ o.join.command }}</code>
            <button type="button" class="btn btn-xs" title="Copy the command" data-testid="pool-join-copy" @click="copyJoin">
              <Icon name="copy" :size="13" /> Copy
            </button>
          </div>
          <p v-if="pool.error" class="err small"><Icon name="alert" :size="14" /> {{ pool.error }} (showing the last answer)</p>
        </template>
        <p v-else class="muted">Reading the pool…</p>
      </div>
    </section>

    <section v-if="!addingPool && guests.length" class="card guests-card" data-testid="pool-guests">
      <header class="card-header">
        <Icon name="server" :size="18" />
        <h2>On this machine</h2>
        <span class="faint small">{{ guests.length === 1 ? '1 lane of another member' : `${guests.length} lanes of other members` }} · {{ guestsReserved(guests) }} reserved</span>
      </header>
      <table class="table">
        <thead>
          <tr>
            <th>Lane</th>
            <th>Repository</th>
            <th>Of</th>
            <th v-if="guestPools">Pool</th>
            <th>State</th>
            <th>Since</th>
            <th>Reserved</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="g in guests" :key="`${g.pool}/${g.lease}`" :data-lease="g.lease">
            <td><strong>{{ g.lane }}</strong></td>
            <td>{{ g.repo }}</td>
            <td>{{ g.owner }}</td>
            <td v-if="guestPools">{{ g.pool }}</td>
            <td>
              <span class="pill sm" :class="g.state === 'running' ? 'ok' : 'outline'">{{ g.state === 'running' ? 'running' : 'starting' }}</span>
            </td>
            <td>{{ ago(g.since * 1000, now) }}</td>
            <td class="tabular">{{ g.cpus }} CPU · {{ bytes(g.memory) }}</td>
          </tr>
        </tbody>
      </table>
      <p class="card-foot muted small">
        They run in containers of this machine's Docker, as this node's policy allows. An admin's <em>Pause</em> on this node in the list below takes
        no new lanes; those running stay until their owners stop them.
      </p>
    </section>

    <section v-if="!addingPool && me?.state === 'approved'" class="card" data-testid="pool-default-on">
      <header class="card-header">
        <Icon name="server" :size="18" />
        <h2>Where lanes start</h2>
        <span class="faint small">without a place named (`devstack up`, an agent's `lane_start`)</span>
      </header>
      <div class="card-body">
        <label class="consent">
          <input type="radio" name="default-on" :checked="!pool.defaultOn" data-testid="pool-default-on-here" @change="pool.setDefaultOn(null)" />
          <span>On this machine</span>
        </label>
        <label v-for="p in pool.pools.filter((x) => x.joined)" :key="p.name" class="consent">
          <input
            type="radio"
            name="default-on"
            :checked="pool.defaultOn === (p.name === 'default' ? 'pool' : p.name)"
            :data-testid="`pool-default-on-${p.name}`"
            @change="pool.setDefaultOn(p.name === 'default' ? 'pool' : p.name)"
          />
          <span>In the pool{{ p.name === 'default' ? '' : ` ${p.name}` }}: its coordinator picks the node (warm first, then free capacity)</span>
        </label>
        <p class="muted small">
          Choosing the pool lets the CLI and agents send a worktree's files to a node of it without asking each time. A place named
          explicitly (`--on`, `on`) always wins; `local` keeps a lane here.
        </p>
      </div>
    </section>

    <section v-if="!addingPool && me?.state === 'approved' && pool.telemetry" class="card" data-testid="pool-telemetry">
      <header class="card-header">
        <Icon name="activity" :size="18" />
        <h2>Telemetry</h2>
        <span class="faint small">{{ pool.telemetry.collects ? 'this pool collects it' : 'this pool collects none' }}</span>
      </header>
      <div class="card-body">
        <label class="consent">
          <input
            type="checkbox"
            :checked="pool.telemetry.consent"
            data-testid="pool-telemetry-consent"
            @change="pool.setTelemetry(($event.target as HTMLInputElement).checked)"
          />
          <span>Send this pool's collector telemetry from this machine</span>
        </label>
        <p class="muted small">
          Off until you turn it on, for this pool only; it goes to the pool's coordinator, which passes it on with this machine's
          identity. <template v-if="!pool.telemetry.collects">Its coordinator has no collector now: nothing is sent.</template>
        </p>
        <ul class="small muted what">
          <li v-for="w in pool.telemetry.what" :key="w">{{ w }}</li>
        </ul>
      </div>
    </section>

    <section v-if="!addingPool && me?.state === 'approved'" class="card" data-testid="pool-capacity">
      <header class="card-header">
        <Icon name="gauge" :size="18" />
        <h2>Capacity</h2>
        <span class="faint small tabular">
          {{ capacity.cpus.toFixed(0) }} cores · {{ bytes(capacity.memory) }}<template v-if="capacity.gpus"> · {{ capacity.gpus }} GPU</template> in all ·
          offered {{ capacity.offeredCpus.toFixed(1) }} CPU, {{ bytes(capacity.offeredMemory) }}<template v-if="capacity.gpus">, {{ capacity.gpusOffered }} GPU</template> ·
          lanes use {{ capacity.usedCpus.toFixed(1) }} CPU, {{ bytes(capacity.usedMemory) }}
        </span>
      </header>
      <div class="card-body"><CapacityChart :pool="pool.current" /></div>
    </section>

    <section v-if="!addingPool && sharedHere.length" class="card" data-testid="pool-shared">
      <header class="card-header">
        <Icon name="globe" :size="18" />
        <h2>Shared with you</h2>
        <span class="faint small">{{ sharedHere.length }}</span>
      </header>
      <table class="table">
        <thead>
          <tr>
            <th>Lane</th>
            <th>On</th>
            <th>Shared by</th>
            <th>Services</th>
            <th>Here</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="l in sharedHere" :key="l.lease">
            <td>
              <RouterLink :to="{ name: 'overview', params: { stack: sharedId(l.pool, l.lease) } }" title="Its state, logs and events, read only" data-testid="pool-shared-view">
                <strong>{{ l.lane }}</strong>
              </RouterLink>
              <span class="faint">{{ l.repo }}</span>
            </td>
            <td>{{ l.node.name }}</td>
            <td>{{ l.owner_name }}</td>
            <td>
              <button
                v-for="s in l.services"
                :key="s.name"
                type="button"
                class="btn btn-xs"
                :title="s.forwarded ? `${s.scheme}://localhost:${s.forwarded}` : `${s.scheme}://localhost:${s.here} if free here; opening it connects the lane`"
                data-testid="pool-shared-open"
                @click="pool.openShared(l.pool, l.lease, s.name)"
              >
                <Icon name="external" :size="12" /> {{ s.name }}<span v-if="s.forwarded" class="faint"> :{{ s.forwarded }}</span>
              </button>
            </td>
            <td>
              <button
                type="button"
                class="btn btn-xs"
                :title="connectedTo(l) ? 'Close its forwards: its ports here are free again' : 'Forward each of its services here, on its owner\'s port if free (ADR-0053)'"
                data-testid="pool-shared-connect"
                @click="connectedTo(l) ? pool.disconnectShared(l.pool, l.lease) : pool.connectShared(l.pool, l.lease)"
              >
                {{ connectedTo(l) ? 'Disconnect' : 'Connect' }}
              </button>
            </td>
          </tr>
        </tbody>
      </table>
    </section>

    <section v-if="pool.isAdmin && pool.pending.length" class="card" data-testid="pool-requests">
      <header class="card-header">
        <Icon name="key" :size="18" />
        <h2>Requests to join</h2>
        <span class="count">{{ pool.pending.length }}</span>
      </header>
      <div class="card-body flush table-wrap">
        <table class="table">
          <thead>
            <tr><th>Machine</th><th>Id</th><th>From</th><th>Asked</th><th /></tr>
          </thead>
          <tbody>
            <tr v-for="m in pool.pending" :key="m.id" :data-member="m.id">
              <td><strong>{{ m.name }}</strong></td>
              <td class="mono faint">{{ m.id }}</td>
              <td class="mono">{{ m.address ?? '–' }}</td>
              <td>{{ asked(m) }}</td>
              <td class="actions">
                <button type="button" class="btn btn-sm btn-primary" data-testid="pool-approve" @click="pool.approve(m.id, m.name)">
                  <Icon name="check" :size="14" /> Approve
                </button>
                <button type="button" class="btn btn-sm" title="Approve, as an admin who manages the pool" data-testid="pool-approve-admin" @click="pool.approve(m.id, m.name, 'admin')">
                  <Icon name="shield" :size="14" /> As admin
                </button>
                <button type="button" class="btn btn-sm btn-danger" data-testid="pool-reject" @click="pool.remove(m.id, m.name, true)">
                  <Icon name="x" :size="14" /> Reject
                </button>
              </td>
            </tr>
          </tbody>
        </table>
        <p class="hint muted">Check the machine's name and where it asked from before approving: a request alone grants nothing.</p>
      </div>
    </section>

    <section v-if="me?.state === 'approved'" class="card" data-testid="pool-nodes">
      <header class="card-header">
        <Icon name="cpu" :size="18" />
        <h2>Nodes</h2>
        <span class="count">{{ nodes.filter((n) => n.online).length }}/{{ nodes.length }} online</span>
      </header>
      <div class="card-body flush table-wrap">
        <table class="table">
          <thead>
            <tr><th>Machine</th><th>State</th><th>devstack</th><th>Shares</th><th>Lanes</th><th>Last report</th><th v-if="pool.isAdmin" /></tr>
          </thead>
          <tbody>
            <template v-for="n in nodes" :key="n.id">
              <tr :data-node="n.id">
              <td>
                <strong>{{ n.name }}</strong> <span v-if="isMe(n.id)" class="pill sm outline">this machine</span>
                <span class="pill sm" :class="n.kind === 'server' ? 'accent' : 'outline'" :title="kindTitle[n.kind ?? 'unknown']" data-testid="pool-kind">{{ kindText[n.kind ?? 'unknown'] }}</span>
                <span v-for="g in n.report?.gpus ?? []" :key="g.name" class="pill sm info" :title="`${g.name}, ${bytes(g.memory)}`">GPU</span>
                <button
                  type="button"
                  class="btn btn-xs btn-ghost"
                  :title="openNode === n.id ? 'Hide its history' : 'What it had, offered and its lanes used, over time'"
                  data-testid="pool-node-history"
                  @click="openNode = openNode === n.id ? null : n.id"
                >
                  <Icon name="activity" :size="12" />
                </button>
              </td>
              <td>
                <span class="pill sm" :class="n.online ? 'ok' : 'outline'">{{ n.online ? 'online' : 'offline' }}</span>
                <span v-if="n.paused" class="pill sm warn" title="Paused by an admin: no new lanes, running ones stay">paused</span>
              </td>
              <td class="tabular" data-testid="pool-node-version">
                <span v-if="n.report" class="pill sm" :class="releaseClass(n.report.devstack, true)" :title="releaseTitle(n.report.devstack, n.name)">
                  {{ n.report.devstack?.version ?? 'older' }}
                </span>
                <span
                  v-if="n.report?.update && !['done', 'cancelled'].includes(n.report.update.state)"
                  class="pill sm"
                  :class="['failed', 'refused'].includes(n.report.update.state) ? 'danger' : 'info'"
                  :title="n.report.update.error ?? `update to ${n.report.update.target ?? 'the latest release'}`"
                  data-testid="pool-node-update"
                >
                  {{ n.report.update.state === 'draining' ? 'updating: waits for its lanes' : n.report.update.state === 'installing' ? 'updating…' : n.report.update.state === 'restarting' ? 'restarting…' : `update ${n.report.update.state}` }}
                </span>
                <span v-else class="faint">–</span>
              </td>
              <td class="tabular">
                <template v-if="n.report?.sharing">{{ n.report.cpus.toFixed(1) }} CPU · {{ bytes(n.report.memory) }}</template>
                <span v-else class="faint">not shared</span>
              </td>
              <td class="tabular">
                {{ lanesOn(n.id) || '–' }}
                <span
                  v-if="n.report?.kept?.lanes"
                  class="faint small"
                  :title="`${n.report.kept.lanes} stopped lane(s) kept there for their next start: mirrors, builds, caches and volumes`"
                  data-testid="pool-node-kept"
                >· {{ n.report.kept.lanes }} kept, {{ bytes(n.report.kept.bytes - (n.report.kept.store_bytes ?? 0)) }}</span>
                <span
                  v-if="n.report?.kept?.store_bytes"
                  class="faint small"
                  title="The members' stores of what prepare makes there (ADR-0062): a lane with the same inputs copies an entry instead of running prepare. Entries count with a member's kept lanes and go first over the room."
                  data-testid="pool-node-store"
                >· store {{ bytes(n.report.kept.store_bytes) }}</span>
              </td>
              <td class="faint">{{ n.seen === undefined ? 'never' : ago(now - n.seen * 1000, now) }}</td>
              <td v-if="pool.isAdmin" class="actions">
                <button
                  v-if="n.report?.api"
                  type="button"
                  class="btn btn-xs"
                  title="Who may run lanes here, which repositories, limits, network (ADR-0033)"
                  data-testid="pool-policy"
                  @click="editPolicy === n.id ? (editPolicy = null) : startPolicy(n.id)"
                >
                  <Icon name="shield" :size="13" /> Policy
                </button>
                <button
                  v-if="n.report?.api"
                  type="button"
                  class="btn btn-xs"
                  :title="n.paused ? 'Let it take lanes again' : 'No new lanes on it; running ones stay'"
                  data-testid="pool-pause"
                  @click="pool.setPaused(n.id, n.name, !n.paused)"
                >
                  <Icon :name="n.paused ? 'play' : 'pause'" :size="13" /> {{ n.paused ? 'Resume' : 'Pause' }}
                </button>
                <button
                  v-if="n.kind === 'server' && n.online"
                  type="button"
                  class="btn btn-xs"
                  :class="{ 'btn-primary': !n.report?.no_update && (hub.newer || releaseClass(n.report?.devstack, true) === 'warn') }"
                  :disabled="!!n.report?.no_update"
                  :title="n.report?.no_update ? `It does not update itself: ${n.report.no_update}` : 'Have this server install the latest devstack once its lanes have ended (ADR-0039)'"
                  data-testid="pool-update"
                  @click="pool.updateNode(n.id, n.name)"
                >
                  <Icon name="download" :size="13" /> Update
                </button>
                <button
                  v-if="n.report?.update?.state === 'draining'"
                  type="button"
                  class="btn btn-xs"
                  title="Withdraw the update: it stops waiting for its lanes and takes new ones again"
                  data-testid="pool-update-cancel"
                  @click="pool.cancelUpdate(n.id, n.name)"
                >
                  Cancel update
                </button>
                <button
                  v-if="n.online && n.report?.kept?.lanes"
                  type="button"
                  class="btn btn-xs"
                  :title="`Remove the ${n.report.kept.lanes} stopped lane(s) it keeps (${bytes(n.report.kept.bytes)})`"
                  data-testid="pool-prune"
                  @click="pool.pruneNode(n.id, n.name)"
                >
                  <Icon name="trash" :size="13" /> Prune
                </button>
              </td>
            </tr>
              <tr v-if="openNode === n.id" class="history-row">
                <td :colspan="pool.isAdmin ? 7 : 6"><CapacityChart :pool="pool.current" :node="n.id" /></td>
              </tr>
              <tr v-if="n.report?.api && (pool.isAdmin || memberOf(n.id)?.policy)" class="policy-row">
                <td :colspan="pool.isAdmin ? 7 : 6" class="faint small">
                  <template v-if="editPolicy !== n.id">Runs lanes of: {{ policyText(n.id) }}</template>
                  <form v-else class="policy-form" data-testid="pool-policy-form" @submit.prevent="savePolicy(n.id, n.name)">
                    <label><span>Who (member ids or tags)</span><input v-model="pol.allow" class="input" placeholder="everyone" /></label>
                    <label><span>Repositories</span><input v-model="pol.repos" class="input" placeholder="any" /></label>
                    <label><span>Memory limit (× need)</span><input v-model="pol.headroom" class="input" placeholder="none" /></label>
                    <label><span>CPU per lane (cores)</span><input v-model="pol.cpus" class="input" placeholder="no limit" /></label>
                    <label title="What of its free CPU and memory the node offers the pool; its own caps still hold">
                      <span>Offers of what is free (%)</span><input v-model="pol.share" class="input" inputmode="numeric" placeholder="50" />
                    </label>
                    <label>
                      <span>Network</span>
                      <select v-model="pol.network" class="input">
                        <option value="lan">internet and local network</option>
                        <option value="internet">the internet, not the local network</option>
                        <option value="none">only the lane's own containers</option>
                      </select>
                    </label>
                    <button type="submit" class="btn btn-sm btn-primary">Save</button>
                    <button type="button" class="btn btn-sm" @click="savePolicy(n.id, n.name, true)">Lift</button>
                    <button type="button" class="btn btn-sm btn-ghost" @click="editPolicy = null">Cancel</button>
                    <span class="faint">A repository is named by the requester: a guard against mistakes, not against someone who lies.</span>
                  </form>
                </td>
              </tr>
            </template>
          </tbody>
        </table>
      </div>
    </section>

    <section v-if="me?.state === 'approved' && leases.length" class="card" data-testid="pool-lanes">
      <header class="card-header">
        <Icon name="gitBranch" :size="18" />
        <h2>Lanes</h2>
        <span class="count">{{ leases.length }}</span>
      </header>
      <div class="card-body flush table-wrap">
        <table class="table">
          <thead>
            <tr><th>Lane</th><th>On</th><th>For</th><th>State</th><th v-if="pool.isAdmin" /></tr>
          </thead>
          <tbody>
            <tr v-for="l in leases" :key="l.id" :data-lease="l.id">
              <td><strong>{{ l.lane }}</strong> <span class="faint">{{ l.repo }}</span></td>
              <td>{{ nameOf(l.node) }}</td>
              <td>{{ nameOf(l.member) }} <span v-if="isMe(l.member)" class="pill sm outline">this machine</span></td>
              <td><span class="pill sm" :class="l.state === 'running' ? 'ok' : 'outline'">{{ l.state }}</span></td>
              <td v-if="pool.isAdmin" class="actions">
                <button type="button" class="btn btn-xs btn-danger" title="Stop this lane on its node" data-testid="pool-end" @click="pool.endLease(l.id, leaseWhat(l))">
                  <Icon name="stop" :size="13" /> End
                </button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </section>

    <section v-if="me?.state === 'approved'" class="card" data-testid="pool-members">
      <header class="card-header">
        <Icon name="layers" :size="18" />
        <h2>Members</h2>
        <span class="count">{{ members.length }}</span>
      </header>
      <div class="card-body flush table-wrap">
        <table class="table">
          <thead>
            <tr><th>Machine</th><th>Id</th><th>Role</th><th v-if="pool.isAdmin">From</th><th v-if="pool.isAdmin" /></tr>
          </thead>
          <tbody>
            <tr v-for="m in members" :key="m.id" :data-member="m.id">
              <td>
                <strong>{{ m.name }}</strong> <span v-if="isMe(m.id)" class="pill sm outline">this machine</span>
                <span class="pill sm" :class="kindOf(m.id) === 'server' ? 'accent' : 'outline'" :title="kindTitle[kindOf(m.id)]">{{ kindText[kindOf(m.id)] }}</span>
                <span v-if="m.node" class="pill sm accent" title="An admin let it run the pool's lanes" data-testid="pool-node-granted">runs lanes</span>
                <span v-for="t in m.tags ?? []" :key="t" class="pill sm info" data-testid="pool-tag">{{ t }}</span>
                <form v-if="editTags === m.id" class="tags-form" @submit.prevent="saveTags(m)">
                  <input v-model="tagsText" class="input" placeholder="team:backend, customer:acme" data-testid="pool-tags-input" />
                  <button type="submit" class="btn btn-xs btn-primary">Save</button>
                  <button type="button" class="btn btn-xs btn-ghost" @click="editTags = null">Cancel</button>
                </form>
              </td>
              <td class="mono faint">{{ m.id }}</td>
              <td><span class="pill sm" :class="m.role === 'admin' ? 'accent' : 'outline'">{{ m.role }}</span></td>
              <td v-if="pool.isAdmin" class="mono">{{ m.address ?? '–' }}</td>
              <td v-if="pool.isAdmin" class="actions">
                <button type="button" class="btn btn-sm" title="Tags for nodes' policies (ADR-0033)" data-testid="pool-tags" @click="startTags(m)">
                  Tags
                </button>
                <button
                  type="button"
                  class="btn btn-sm"
                  :title="m.node ? 'The coordinator places no new lanes on it' : 'Let it run the pool\'s lanes as a node'"
                  data-testid="pool-node"
                  @click="pool.setNode(m.id, m.name, !m.node)"
                >
                  {{ m.node ? 'No lanes' : 'Allow lanes' }}
                </button>
                <button
                  v-if="m.role === 'member'"
                  type="button"
                  class="btn btn-sm"
                  data-testid="pool-make-admin"
                  @click="pool.setRole(m.id, m.name, 'admin')"
                >
                  <Icon name="shield" :size="14" /> Make admin
                </button>
                <button
                  v-else
                  type="button"
                  class="btn btn-sm"
                  :disabled="lastAdmin(m)"
                  :title="lastAdmin(m) ? lastAdminTitle : undefined"
                  data-testid="pool-make-member"
                  @click="pool.setRole(m.id, m.name, 'member')"
                >
                  Make member
                </button>
                <button
                  type="button"
                  class="btn btn-sm btn-danger"
                  :disabled="lastAdmin(m)"
                  :title="lastAdmin(m) ? lastAdminTitle : undefined"
                  data-testid="pool-remove"
                  @click="pool.remove(m.id, m.name, false)"
                >
                  <Icon name="trash" :size="14" /> Remove
                </button>
              </td>
            </tr>
          </tbody>
        </table>
        <p v-if="pool.isAdmin && removed" class="hint">
          <button type="button" class="btn btn-xs btn-ghost" @click="showRemoved = !showRemoved">
            <Icon :name="showRemoved ? 'chevronDown' : 'chevronRight'" :size="13" /> {{ removed }} removed or rejected
          </button>
        </p>
        <ul v-if="showRemoved" class="removed">
          <li v-for="m in removedList" :key="m.id" class="faint">{{ m.name }} <span class="mono">{{ m.id }}</span></li>
        </ul>
      </div>
    </section>

    <section v-if="pool.isAdmin && audit.length" class="card" data-testid="pool-audit">
      <header class="card-header">
        <Icon name="history" :size="18" />
        <h2>Admin log</h2>
      </header>
      <div class="card-body">
        <ul class="audit">
          <li v-for="(e, i) in audit" :key="i">
            <span class="faint tabular when">{{ ago(e.at * 1000, now) }}</span>
            <span><strong>{{ e.by }}</strong> {{ actionText[e.action] ?? e.action }} <strong>{{ e.name }}</strong><template v-if="e.detail"> ({{ e.detail }})</template></span>
          </li>
        </ul>
      </div>
    </section>
  </div>
</template>

<style scoped>
.card-foot {
  margin: 0;
  padding: 10px var(--space-4, 16px) 12px;
}
.guests-card .card-header .small {
  margin-left: auto;
}
.pool {
  display: flex;
  flex-direction: column;
  gap: 16px;
  max-width: 1000px;
}
.facts {
  display: grid;
  grid-template-columns: max-content 1fr;
  gap: 4px 16px;
  margin: 0;
}
.facts dt {
  color: var(--text-muted);
}
.facts dd {
  margin: 0;
}
.mono {
  font-family: var(--font-mono, monospace);
  font-size: 12px;
}
.actions {
  text-align: right;
  white-space: nowrap;
}
.actions > .btn + .btn {
  margin-left: 6px;
}
.hint {
  margin: 0;
  padding: 8px 12px;
  font-size: 12px;
}
.err,
.warn-line {
  display: flex;
  align-items: center;
  gap: 6px;
  margin: 8px 0 0;
}
.err {
  color: var(--danger);
}
.warn-line {
  color: var(--warn);
}
.small {
  font-size: 12px;
}
.policy-row td {
  padding-top: 0;
}
.policy-form,
.tags-form {
  display: flex;
  flex-wrap: wrap;
  align-items: flex-end;
  gap: 8px;
  margin: 4px 0 6px;
}
.policy-form label {
  display: flex;
  flex-direction: column;
  gap: 3px;
  font-size: 12px;
}
.policy-form .input {
  width: 190px;
}
.tags-form .input {
  width: 220px;
}
.consent {
  display: flex;
  align-items: center;
  gap: 8px;
  font-weight: 600;
}
.what {
  margin: 6px 0 0;
  padding-left: 18px;
}
.pool-tabs {
  display: flex;
  gap: 4px;
  margin-bottom: 12px;
  flex-wrap: wrap;
}
.pool-tabs .tab {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 5px 12px;
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--surface);
  color: var(--text-muted);
  font-size: 13px;
  cursor: pointer;
}
.pool-tabs .tab.on {
  border-color: var(--accent);
  color: var(--text);
  background: var(--surface-hover);
}
.join {
  display: flex;
  flex-wrap: wrap;
  align-items: flex-end;
  gap: 10px;
  margin-top: 10px;
}
.join label {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 12px;
  color: var(--text-muted);
}
.join .wide input {
  width: 420px;
  max-width: 100%;
}
.join-cmd {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  margin-top: 10px;
}
.join-cmd code {
  font-size: 12px;
  word-break: break-all;
}
.audit,
.removed {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 6px;
  font-size: 13px;
}
.removed {
  padding: 0 12px 10px;
}
.audit li {
  display: flex;
  gap: 12px;
}
.when {
  min-width: 80px;
}
</style>
