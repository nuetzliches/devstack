<script setup lang="ts">
// A pool at a glance: its coordinator, how another machine joins, what waits
// for an admin, its capacity (ADR-0036), and what this machine set for this
// pool alone: provisioned nodes (ADR-0051) and telemetry (ADR-0037).
import { computed } from 'vue';
import { bytes } from '@/lib/format';
import { releaseClass, releaseTitle } from '@/lib/poolText';
import { usePool } from '@/stores/pool';
import { useHub } from '@/stores/hub';
import { useUi } from '@/stores/ui';
import Icon from '@/components/icons/Icon.vue';
import PoolCapacityChart from '@/components/PoolCapacityChart.vue';
import NodeLoad from '@/components/NodeLoad.vue';

const pool = usePool();
const hub = useHub();
const ui = useUi();
const o = computed(() => pool.overview);
const me = computed(() => o.value?.me);
const nodes = computed(() => o.value?.nodes ?? []);
const approved = computed(() => me.value?.state === 'approved');

async function copyJoin() {
  const c = o.value?.join?.command;
  if (!c) return;
  await navigator.clipboard.writeText(c);
  ui.toast('info', 'Copied: run it on the machine that is to join');
}

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

/** The nodes that report, online first, as the lane start shows them. */
const loads = computed(() => [...nodes.value.filter((n) => n.report?.total)].sort((a, b) => Number(b.online) - Number(a.online)));
const lanesOn = (id: string) => (o.value?.leases ?? []).filter((l) => l.node === id).length;

/** What waits for an admin, each with where it is handled. */
const due = computed(() => {
  if (!pool.isAdmin) return [];
  const d: { text: string; to: object; testid: string }[] = [];
  const admin = { name: 'pool-admin', params: { pool: pool.current } };
  if (pool.pending.length) d.push({ text: `${pool.pending.length} request(s) to join`, to: admin, testid: 'pool-due-requests' });
  const alarms = (o.value?.provisioning?.machines ?? []).filter((m) => m.alarm).length;
  if (alarms) d.push({ text: `${alarms} provisioning alarm(s)`, to: admin, testid: 'pool-due-alarms' });
  for (const n of nodes.value.filter((x) => x.provisioned?.unconfirmed))
    d.push({ text: `${n.name} waits for an admin to confirm it`, to: { name: 'pool-machine', params: { pool: pool.current, id: n.id } }, testid: 'pool-due-confirm' });
  return d;
});

const myMember = computed(() => (o.value?.members ?? []).find((m) => m.id === me.value?.id));
/** Whether this member's lanes may run on provisioned nodes (the inverse of `no_provisioned`). */
const onProvisioned = computed(() => !(me.value?.no_provisioned ?? myMember.value?.no_provisioned));
</script>

<template>
  <section class="card">
    <header class="card-header">
      <Icon name="server" :size="18" />
      <h2>Pool<template v-if="pool.current !== 'default'"> {{ pool.current }}</template></h2>
      <span v-if="me?.role" class="pill sm" :class="me.role === 'admin' ? 'accent' : 'outline'" data-testid="pool-role">{{ me.role }}</span>
    </header>
    <div v-if="me" class="card-body">
      <dl class="facts">
        <dt>Coordinator</dt>
        <dd>
          <code>{{ me.coordinator }}</code>
          <span
            v-if="pool.share?.reported_at"
            class="pill sm"
            :class="releaseClass(pool.share.coordinator, true, hub.version)"
            :title="releaseTitle(pool.share.coordinator, 'The coordinator', hub.version)"
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
      <div v-if="approved && o?.join" class="join-cmd" data-testid="pool-join-command">
        <span class="muted small">Another machine joins with</span>
        <code class="mono">{{ o.join.command }}</code>
        <button type="button" class="btn btn-xs" title="Copy the command" data-testid="pool-join-copy" @click="copyJoin">
          <Icon name="copy" :size="13" /> Copy
        </button>
      </div>
      <p v-if="pool.error" class="err small"><Icon name="alert" :size="14" /> {{ pool.error }} (showing the last answer)</p>
      <p v-else-if="approved && pool.live?.live === 'old'" class="muted small" data-testid="pool-not-live">
        <Icon name="info" :size="14" /> Not live: the pool's coordinator runs an older devstack. What shows here was read when the page opened.
      </p>
      <p v-else-if="approved && pool.live?.live === 'lost'" class="muted small" data-testid="pool-not-live">
        <Icon name="alert" :size="14" /> Not live: {{ pool.live.error }}. Connecting again; what shows here may be behind.
      </p>
    </div>
  </section>

  <section v-if="due.length" class="card" data-testid="pool-due">
    <header class="card-header">
      <Icon name="bell" :size="18" />
      <h2>Waits for an admin</h2>
    </header>
    <ul class="due">
      <li v-for="d in due" :key="d.text">
        <Icon name="alert" :size="14" />
        <RouterLink :to="d.to" :data-testid="d.testid">{{ d.text }}</RouterLink>
      </li>
    </ul>
  </section>

  <section v-if="approved" class="card" data-testid="pool-capacity">
    <header class="card-header">
      <Icon name="gauge" :size="18" />
      <h2>Capacity</h2>
    </header>
    <div class="card-body">
      <div class="tiles tabular">
        <div class="tile">
          <span class="faint small">In all</span>
          <strong>{{ capacity.cpus.toFixed(0) }} cores · {{ bytes(capacity.memory) }}</strong>
          <span v-if="capacity.gpus" class="faint small">{{ capacity.gpus }} GPU</span>
        </div>
        <div class="tile">
          <span class="faint small">Offered</span>
          <strong>{{ capacity.offeredCpus.toFixed(1) }} CPU · {{ bytes(capacity.offeredMemory) }}</strong>
          <span v-if="capacity.gpus" class="faint small">{{ capacity.gpusOffered }} GPU</span>
        </div>
        <div class="tile">
          <span class="faint small">Lanes use</span>
          <strong>{{ capacity.usedCpus.toFixed(1) }} CPU · {{ bytes(capacity.usedMemory) }}</strong>
        </div>
      </div>
      <div v-if="loads.length" class="loads" data-testid="pool-capacity-nodes">
        <RouterLink
          v-for="n in loads"
          :key="n.id"
          :to="{ name: 'pool-machine', params: { pool: pool.current, id: n.id } }"
          class="load-card"
          :class="{ off: !n.online || n.paused }"
          data-testid="pool-capacity-node"
        >
          <span class="lname">
            <Icon name="server" :size="14" /> <strong>{{ n.name }}</strong>
            <span v-if="!n.online" class="pill sm outline">offline</span>
            <span v-else-if="n.paused" class="pill sm warn">paused</span>
            <span v-if="!n.report?.sharing" class="pill sm outline">not shared</span>
          </span>
          <NodeLoad :pool="pool.current" :node="n" :lanes="lanesOn(n.id)" />
        </RouterLink>
      </div>
      <p class="sub faint small">Over time, machine by machine</p>
      <PoolCapacityChart :pool="pool.current" />
    </div>
  </section>

  <section v-if="approved" class="card" data-testid="pool-mine">
    <header class="card-header">
      <Icon name="monitor" :size="18" />
      <h2>This machine in this pool</h2>
    </header>
    <div class="card-body">
      <label class="consent">
        <input
          type="checkbox"
          :checked="onProvisioned"
          data-testid="pool-on-provisioned"
          @change="pool.setProvisioned(($event.target as HTMLInputElement).checked)"
        />
        <span>My lanes may run on provisioned nodes</span>
      </label>
      <p class="muted small">
        Nodes the pool's provisioner makes run on a provider's machines: whoever runs the provisioner (and the provider) can read a lane
        there. Off: this machine's lanes go to the pool's other nodes only.
      </p>
      <template v-if="pool.telemetry">
        <label class="consent" data-testid="pool-telemetry">
          <input
            type="checkbox"
            :checked="pool.telemetry.consent"
            data-testid="pool-telemetry-consent"
            @change="pool.setTelemetry(($event.target as HTMLInputElement).checked)"
          />
          <span>Send this pool's collector telemetry from this machine</span>
          <span class="faint small">{{ pool.telemetry.collects ? 'this pool collects it' : 'this pool collects none' }}</span>
        </label>
        <p class="muted small">
          Off until you turn it on, for this pool only; it goes to the pool's coordinator, which passes it on with this machine's
          identity. <template v-if="!pool.telemetry.collects">Its coordinator has no collector now: nothing is sent.</template>
        </p>
        <ul class="small muted what">
          <li v-for="w in pool.telemetry.what" :key="w">{{ w }}</li>
        </ul>
      </template>
      <p class="muted small">
        Where this machine's lanes start, and the lanes of others it runs, hold in every pool:
        <RouterLink :to="{ name: 'pool-here' }">This machine</RouterLink>.
      </p>
    </div>
  </section>
</template>

<style scoped>
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
.due {
  list-style: none;
  margin: 0;
  padding: 10px 16px 12px;
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.due li {
  display: flex;
  align-items: center;
  gap: 8px;
  color: var(--warn);
}
.tiles {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
  gap: 10px;
  margin-bottom: 12px;
}
.tile {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 10px 12px;
  border: 1px solid var(--border);
  border-radius: 8px;
}
.loads {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
  gap: 8px;
  margin-bottom: 14px;
}
.load-card {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 10px 12px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--surface);
  color: inherit;
  text-decoration: none;
}
.load-card:hover {
  border-color: var(--accent);
  background: var(--surface-hover);
  text-decoration: none;
}
.load-card.off {
  opacity: 0.7;
}
.lname {
  display: flex;
  align-items: center;
  gap: 6px;
}
.sub {
  margin: 0 0 4px;
}
.what {
  margin: 6px 0 0;
  padding-left: 18px;
}
.card-body > .consent + .muted,
.card-body > .what {
  margin-bottom: 12px;
}
</style>
