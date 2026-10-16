<script setup lang="ts">
// The pool (ADR-0031, ADR-0032): this machine's membership (and joining one),
// the requests to join, the nodes with their lanes, the members. An admin
// approves, rejects and removes members, changes roles, pauses nodes and ends
// leases here; the coordinator checks the role, this page only hides what the
// role does not allow.
import { computed, onBeforeUnmount, ref } from 'vue';
import { ago, bytes } from '@/lib/format';
import { usePool } from '@/stores/pool';
import Icon from '@/components/icons/Icon.vue';
import ShareSwitch from '@/components/ShareSwitch.vue';
import type { PoolLease, PoolMember } from '@/api/types';
import { useUi } from '@/stores/ui';

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
};

// Joining a pool from here, as `devstack join` does.
const joinUrl = ref('');
const joinFp = ref('');
const joinName = ref('');
const joining = ref(false);
async function join() {
  joining.value = true;
  await pool.join(joinUrl.value.trim(), joinFp.value.trim(), joinName.value.trim());
  joining.value = false;
}

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
const kindOf = (id: string) => nodes.value.find((n) => n.id === id)?.kind ?? 'unknown';
const kindText: Record<string, string> = { server: 'server', node: 'node', machine: 'developer machine', unknown: 'not seen yet' };
const kindTitle: Record<string, string> = {
  server: 'Runs lanes for the pool and never reclaims them (a dedicated node)',
  node: 'Runs lanes for the pool; its owner may reclaim them',
  machine: 'A developer machine: asks for lanes, may share',
  unknown: 'Has not reported to the coordinator yet',
};
const leaseWhat = (l: PoolLease) => `lane ${l.repo}/${l.lane} on ${nameOf(l.node)}`;
</script>

<template>
  <div class="page pool" data-testid="pool">
    <section class="card">
      <header class="card-header">
        <Icon name="server" :size="18" />
        <h2>Pool</h2>
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
            <dd><code>{{ me.coordinator }}</code></dd>
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

    <section v-if="me?.state === 'approved'" class="card" data-testid="pool-share">
      <header class="card-header">
        <Icon name="power" :size="18" />
        <h2>This machine shares</h2>
      </header>
      <div class="card-body"><ShareSwitch /></div>
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
            <tr><th>Machine</th><th>State</th><th>Shares</th><th>Lanes</th><th>Last report</th><th v-if="pool.isAdmin" /></tr>
          </thead>
          <tbody>
            <tr v-for="n in nodes" :key="n.id" :data-node="n.id">
              <td>
                <strong>{{ n.name }}</strong> <span v-if="isMe(n.id)" class="pill sm outline">this machine</span>
                <span class="pill sm" :class="n.kind === 'server' ? 'accent' : 'outline'" :title="kindTitle[n.kind ?? 'unknown']" data-testid="pool-kind">{{ kindText[n.kind ?? 'unknown'] }}</span>
              </td>
              <td>
                <span class="pill sm" :class="n.online ? 'ok' : 'outline'">{{ n.online ? 'online' : 'offline' }}</span>
                <span v-if="n.paused" class="pill sm warn" title="Paused by an admin: no new lanes, running ones stay">paused</span>
              </td>
              <td class="tabular">
                <template v-if="n.report?.sharing">{{ n.report.cpus.toFixed(1) }} CPU · {{ bytes(n.report.memory) }}</template>
                <span v-else class="faint">not shared</span>
              </td>
              <td class="tabular">{{ lanesOn(n.id) || '–' }}</td>
              <td class="faint">{{ n.seen === undefined ? 'never' : ago(now - n.seen * 1000, now) }}</td>
              <td v-if="pool.isAdmin" class="actions">
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
              </td>
            </tr>
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
              </td>
              <td class="mono faint">{{ m.id }}</td>
              <td><span class="pill sm" :class="m.role === 'admin' ? 'accent' : 'outline'">{{ m.role }}</span></td>
              <td v-if="pool.isAdmin" class="mono">{{ m.address ?? '–' }}</td>
              <td v-if="pool.isAdmin" class="actions">
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
