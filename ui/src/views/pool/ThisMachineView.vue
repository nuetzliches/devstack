<script setup lang="ts">
// What holds for this machine in every pool: what it has and offers (the
// hub's share report, ADR-0031 point 3), how that went over time (ADR-0036),
// how each pool sees it (role, grants, policy, what it keeps), the lanes of
// other members it runs (the hub's `guests`), its own lanes on other
// machines, and where its lanes start without a place named (ADR-0025).
import { computed, onBeforeUnmount, ref, watch } from 'vue';
import { ago, bytes, useNow } from '@/lib/format';
import { guestsReserved } from '@/lib/guests';
import { kindText, kindTitle, policyText } from '@/lib/poolText';
import { usePool } from '@/stores/pool';
import { useHub } from '@/stores/hub';
import Icon from '@/components/icons/Icon.vue';
import CapacityChart from '@/components/CapacityChart.vue';
import LocalLoad from '@/components/LocalLoad.vue';
import PoolNav from './PoolNav.vue';
import './pool.css';

const pool = usePool();
onBeforeUnmount(pool.watch());
// The share report is read with the pool; its time is read fresh here.
void pool.loadShare();
const hub = useHub();
const now = useNow();
const guests = computed(() => hub.guests);
const guestPools = computed(() => new Set(guests.value.map((g) => g.pool)).size > 1);
const joined = computed(() => pool.pools.filter((p) => p.joined && p.state !== 'pending'));
const share = computed(() => pool.share);

/** This machine as each pool it is an approved member of sees it. */
const inPools = computed(() =>
  joined.value.flatMap((p) => {
    const o = pool.overviews[p.name];
    const id = o?.me?.id;
    if (!o || !id || o.me?.state !== 'approved') return [];
    const nameOf = (x: string) => o.members?.find((m) => m.id === x)?.name ?? o.nodes?.find((n) => n.id === x)?.name ?? x;
    return [
      {
        pool: p.name,
        id,
        name: o.me.name,
        member: o.members?.find((m) => m.id === id),
        node: o.nodes?.find((n) => n.id === id),
        mine: (o.leases ?? []).filter((l) => l.member === id && l.node !== id).map((l) => ({ ...l, on: nameOf(l.node) })),
      },
    ];
  }),
);
const kept = computed(() => inPools.value.map((x) => x.node?.report?.kept).find((k) => k?.lanes || k?.store_bytes));
const mine = computed(() => inPools.value.flatMap((x) => x.mine.map((l) => ({ ...l, pool: x.pool }))));

// The history the coordinator keeps of this machine, from one pool at a time.
const chartPool = ref<string | null>(null);
watch(
  inPools,
  (list) => {
    if (!list.some((x) => x.pool === chartPool.value)) chartPool.value = list.find((x) => x.pool === pool.current)?.pool ?? list[0]?.pool ?? null;
  },
  { immediate: true },
);
const chartId = computed(() => inPools.value.find((x) => x.pool === chartPool.value)?.id);
</script>

<template>
  <div class="page pool" data-testid="pool-here">
    <PoolNav />

    <section class="card" data-testid="pool-here-facts">
      <header class="card-header">
        <Icon name="monitor" :size="18" />
        <h2>This machine</h2>
      </header>
      <div class="card-body">
        <dl class="facts">
          <template v-if="hub.version">
            <dt>devstack</dt>
            <dd><span class="pill sm outline">{{ hub.version }}</span><span v-if="hub.newer" class="faint small"> · {{ hub.newer }} is out</span></dd>
          </template>
          <template v-if="share">
            <dt>Reports</dt>
            <dd data-testid="pool-here-report">
              <span v-if="share.error" class="warn-text">{{ share.error }}</span>
              <template v-else-if="share.reported_at">to the coordinator, last {{ ago(share.reported_at, now) }}</template>
              <span v-else class="faint">nothing: it is no approved member of a pool</span>
              <span v-if="share.agent" class="faint"> · a node agent here reports the machine</span>
            </dd>
            <template v-if="share.unavailable">
              <dt>Offers</dt>
              <dd class="muted small">{{ share.unavailable }}</dd>
            </template>
          </template>
          <template v-if="kept">
            <dt>Keeps</dt>
            <dd class="tabular">
              <template v-if="kept.lanes">{{ kept.lanes }} stopped lane(s) of others, {{ bytes(kept.bytes - (kept.store_bytes ?? 0)) }}</template>
              <span v-if="kept.store_bytes" class="faint"> · store {{ bytes(kept.store_bytes) }}</span>
            </dd>
          </template>
        </dl>
        <div class="local-load"><LocalLoad /></div>
      </div>
    </section>

    <section v-if="chartPool && chartId" class="card" data-testid="pool-here-history">
      <header class="card-header">
        <Icon name="activity" :size="18" />
        <h2>Capacity over time</h2>
        <span class="faint small">what it had, offered and lanes used here, as {{ chartPool === 'default' ? 'the pool' : `the pool ${chartPool}` }} recorded it</span>
        <span v-if="inPools.length > 1" class="end pick">
          <button
            v-for="x in inPools"
            :key="x.pool"
            type="button"
            class="btn btn-xs"
            :class="{ 'btn-primary': x.pool === chartPool }"
            :aria-pressed="x.pool === chartPool"
            @click="chartPool = x.pool"
          >{{ x.pool }}</button>
        </span>
      </header>
      <div class="card-body"><CapacityChart :pool="chartPool" :node="chartId" /></div>
    </section>

    <section v-if="inPools.length" class="card" data-testid="pool-here-pools">
      <header class="card-header">
        <Icon name="server" :size="18" />
        <h2>In its pools</h2>
      </header>
      <div class="card-body flush table-wrap">
        <table class="table">
          <thead>
            <tr><th>Pool</th><th>As</th><th>Role</th><th>Kind</th><th>Others' lanes here</th><th /></tr>
          </thead>
          <tbody>
            <tr v-for="x in inPools" :key="x.pool" :data-pool="x.pool">
              <td><strong>{{ x.pool }}</strong></td>
              <td>{{ x.name }} <span class="faint mono">{{ x.id }}</span></td>
              <td>
                <span class="pill sm" :class="x.member?.role === 'admin' ? 'accent' : 'outline'">{{ x.member?.role ?? 'member' }}</span>
                <span v-for="t in x.member?.tags ?? []" :key="t" class="pill sm info"> {{ t }}</span>
              </td>
              <td><span class="pill sm outline" :title="kindTitle[x.node?.kind ?? 'unknown']">{{ kindText[x.node?.kind ?? 'unknown'] }}</span></td>
              <td class="small">
                <template v-if="x.member?.node">{{ x.node?.paused ? 'paused · ' : '' }}{{ policyText(x.member.policy) }}</template>
                <span v-else class="faint">not granted: an admin has not let it run the pool's lanes</span>
              </td>
              <td class="actions">
                <RouterLink :to="{ name: 'pool-machine', params: { pool: x.pool, id: x.id } }" class="btn btn-xs" data-testid="pool-here-page">Its page there</RouterLink>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </section>

    <section class="card" data-testid="pool-guests">
      <header class="card-header">
        <Icon name="server" :size="18" />
        <h2>Lanes of others on this machine</h2>
        <span v-if="guests.length" class="faint small end">{{ guests.length }} · {{ guestsReserved(guests) }} reserved</span>
      </header>
      <div class="card-body flush table-wrap">
        <table v-if="guests.length" class="table">
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
              <td class="tabular nowrap">{{ g.cpus }} CPU · {{ bytes(g.memory) }}</td>
            </tr>
          </tbody>
        </table>
        <p class="hint muted">
          <template v-if="!guests.length">No lane of another member runs here. </template>
          They run in containers of this machine's Docker, as this node's policy allows. An admin's <em>Pause</em> on this machine takes no new
          lanes; those running stay until their owners stop them.
        </p>
      </div>
    </section>

    <section v-if="mine.length" class="card" data-testid="pool-here-mine">
      <header class="card-header">
        <Icon name="gitBranch" :size="18" />
        <h2>Its lanes on other machines</h2>
        <span class="count">{{ mine.length }}</span>
      </header>
      <div class="card-body flush table-wrap">
        <table class="table">
          <thead>
            <tr><th>Lane</th><th>On</th><th v-if="inPools.length > 1">Pool</th><th>State</th></tr>
          </thead>
          <tbody>
            <tr v-for="l in mine" :key="`${l.pool}/${l.id}`" :data-lease="l.id">
              <td><strong>{{ l.lane }}</strong> <span class="faint">{{ l.repo }}</span></td>
              <td><RouterLink :to="{ name: 'pool-machine', params: { pool: l.pool, id: l.node } }">{{ l.on }}</RouterLink></td>
              <td v-if="inPools.length > 1">{{ l.pool }}</td>
              <td><span class="pill sm" :class="l.state === 'running' ? 'ok' : 'outline'">{{ l.state }}</span></td>
            </tr>
          </tbody>
        </table>
      </div>
    </section>

    <section class="card" data-testid="pool-default-on">
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
        <label v-for="p in joined" :key="p.name" class="consent">
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
          <template v-if="!joined.length">This machine is in no pool yet: <RouterLink :to="{ name: 'pool' }">join one</RouterLink> to start lanes elsewhere. </template>
          Choosing a pool lets the CLI and agents send a worktree's files to a node of it without asking each time. A place named
          explicitly (`--on`, `on`) always wins; `local` keeps a lane here.
        </p>
      </div>
    </section>
  </div>
</template>

<style scoped>
.pool {
  display: flex;
  flex-direction: column;
  gap: 16px;
  max-width: 1100px;
}
.local-load {
  max-width: 520px;
  margin-top: 12px;
}
.pick {
  display: inline-flex;
  gap: 4px;
}
</style>
