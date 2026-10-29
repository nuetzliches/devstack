<script setup lang="ts">
// One machine of the pool: its membership (role, tags, what an admin granted
// it) and its node side (what it reports, its policy, its capacity over time,
// the lanes on it), with every admin action on it and its part of the admin
// log. The coordinator checks each action; this page hides what the role does
// not allow.
import { computed, ref } from 'vue';
import { useRoute } from 'vue-router';
import { ago, bytes, duration, useNow } from '@/lib/format';
import { actionText, kindText, kindTitle, policyText, releaseClass, releaseTitle, updateText } from '@/lib/poolText';
import { usePool } from '@/stores/pool';
import { useHub } from '@/stores/hub';
import Icon from '@/components/icons/Icon.vue';
import CapacityChart from '@/components/CapacityChart.vue';
import PoolLeases from './PoolLeases.vue';

const pool = usePool();
const hub = useHub();
const route = useRoute();
const now = useNow();

const id = computed(() => route.params.id as string);
const m = computed(() => pool.machines.find((x) => x.id === id.value));
const member = computed(() => m.value?.member);
const node = computed(() => m.value?.node);
const report = computed(() => node.value?.report);
const isMe = computed(() => pool.overview?.me?.id === id.value);
const leases = computed(() => pool.overview?.leases ?? []);
const on = computed(() => leases.value.filter((l) => l.node === id.value));
const of = computed(() => leases.value.filter((l) => l.member === id.value && l.node !== id.value));
const log = computed(() => (pool.overview?.audit ?? []).filter((e) => e.member === id.value));
const provisioner = computed(() => pool.overview?.provisioning?.provisioner);
const admins = computed(() => pool.machines.filter((x) => x.member?.role === 'admin').length);
/** The pool's only admin, who must stay one (the coordinator refuses otherwise). */
const lastAdmin = computed(() => member.value?.role === 'admin' && admins.value === 1);
const lastAdminTitle = 'The pool needs an admin: make another member admin first';

// Tags (ADR-0033), edited in place.
const editTags = ref(false);
const tagsText = ref('');
function startTags() {
  tagsText.value = (member.value?.tags ?? []).join(', ');
  editTags.value = true;
}
async function saveTags() {
  if (!m.value) return;
  await pool.setTags(m.value.id, m.value.name, tagsText.value.split(',').map((t) => t.trim()).filter(Boolean));
  editTags.value = false;
}

// The node's policy (ADR-0033), edited in place.
const editPolicy = ref(false);
const pol = ref({ allow: '', repos: '', headroom: '', cpus: '', share: '', network: 'lan' as 'lan' | 'internet' | 'none' });
function startPolicy() {
  const p = member.value?.policy;
  pol.value = {
    allow: (p?.allow ?? []).join(', '),
    repos: (p?.repos ?? []).join(', '),
    headroom: p?.memory_headroom !== undefined ? String(p.memory_headroom) : '',
    cpus: p?.max_cpus !== undefined ? String(p.max_cpus) : '',
    share: p?.share_percent !== undefined ? String(p.share_percent) : '',
    network: p?.network ?? 'lan',
  };
  editPolicy.value = true;
}
const cpusOf = (t: string) => {
  const c = Number(t.replace(',', '.'));
  return t.trim() && Number.isFinite(c) && c > 0 ? c : undefined;
};
const list = (t: string) => t.split(',').map((x) => x.trim()).filter(Boolean);
async function savePolicy(clear = false) {
  if (!m.value) return;
  const h = Number(pol.value.headroom.replace(',', '.'));
  const share = Math.round(Number(pol.value.share));
  await pool.setPolicy(
    m.value.id,
    m.value.name,
    clear
      ? null
      : {
          // What the form does not edit stays as it is.
          ...member.value?.policy,
          allow: list(pol.value.allow),
          repos: list(pol.value.repos),
          memory_headroom: pol.value.headroom.trim() && Number.isFinite(h) && h > 0 ? h : undefined,
          max_cpus: cpusOf(pol.value.cpus),
          share_percent: pol.value.share.trim() && share >= 1 && share <= 100 ? share : undefined,
          network: pol.value.network,
        },
  );
  editPolicy.value = false;
}
</script>

<template>
  <RouterLink :to="{ name: 'pool-machines', params: { pool: pool.current } }" class="back small"><Icon name="chevronLeft" :size="13" /> Machines</RouterLink>

  <p v-if="!m" class="muted" data-testid="pool-machine-missing">No machine {{ id }} in this pool.</p>
  <template v-else>
    <section class="card" data-testid="pool-machine">
      <header class="card-header">
        <Icon name="cpu" :size="18" />
        <h2>{{ m.name }}</h2>
        <span v-if="isMe" class="pill sm outline">this machine</span>
        <span class="pill sm" :class="node?.kind === 'server' ? 'accent' : 'outline'" :title="kindTitle[node?.kind ?? 'unknown']" data-testid="pool-kind">{{ kindText[node?.kind ?? 'unknown'] }}</span>
        <span class="pill sm" :class="node?.online ? 'ok' : 'outline'">{{ node?.online ? 'online' : 'offline' }}</span>
        <span v-if="node?.paused" class="pill sm warn" title="Paused by an admin: no new lanes, running ones stay">paused</span>
        <span v-if="node?.provisioned?.unconfirmed" class="pill sm warn" data-testid="pool-unconfirmed">unconfirmed</span>
      </header>
      <div class="card-body">
        <dl class="facts">
          <dt>Id</dt>
          <dd class="mono">{{ m.id }}</dd>
          <template v-if="member">
            <dt>Role</dt>
            <dd><span class="pill sm" :class="member.role === 'admin' ? 'accent' : 'outline'">{{ member.role }}</span></dd>
          </template>
          <template v-if="pool.isAdmin && member?.address">
            <dt>From</dt>
            <dd class="mono">{{ member.address }}</dd>
          </template>
          <dt>Last report</dt>
          <dd>{{ node?.seen === undefined ? 'never' : ago(now - node.seen * 1000, now) }}</dd>
          <template v-if="report">
            <dt>devstack</dt>
            <dd data-testid="pool-node-version">
              <span class="pill sm" :class="releaseClass(report.devstack, true, hub.version)" :title="releaseTitle(report.devstack, m.name, hub.version)">{{ report.devstack?.version ?? 'older' }}</span>
              <span v-if="node && updateText(node)" class="pill sm" :class="['failed', 'refused'].includes(report.update!.state) ? 'danger' : 'info'" :title="report.update!.error ?? ''" data-testid="pool-node-update">{{ updateText(node) }}</span>
            </dd>
            <template v-if="report.total">
              <dt>Has</dt>
              <dd class="tabular">{{ report.total.cpus }} cores · {{ bytes(report.total.memory) }}</dd>
            </template>
            <dt>Offers</dt>
            <dd class="tabular">
              <template v-if="report.sharing">{{ report.cpus.toFixed(1) }} CPU · {{ bytes(report.memory) }}</template>
              <span v-else class="faint">not shared</span>
            </dd>
            <template v-if="report.used">
              <dt>Lanes use</dt>
              <dd class="tabular">{{ report.used.cpus.toFixed(1) }} CPU · {{ bytes(report.used.memory) }}</dd>
            </template>
            <template v-if="report.gpus?.length">
              <dt>GPUs</dt>
              <dd>
                <span v-for="g in report.gpus" :key="g.name" class="gpu">{{ g.name }}, {{ bytes(g.memory) }}<span class="faint">{{ g.offered ? ' · offered' : ' · not offered' }}</span></span>
              </dd>
            </template>
            <template v-if="report.kept?.lanes || report.kept?.store_bytes">
              <dt>Keeps</dt>
              <dd class="tabular" data-testid="pool-node-kept">
                <template v-if="report.kept.lanes">{{ report.kept.lanes }} stopped lane(s), {{ bytes(report.kept.bytes - (report.kept.store_bytes ?? 0)) }}</template>
                <span
                  v-if="report.kept.store_bytes"
                  class="faint"
                  title="The members' stores of what prepare makes there (ADR-0062): a lane with the same inputs copies an entry instead of running prepare."
                  data-testid="pool-node-store"
                > · store {{ bytes(report.kept.store_bytes) }}</span>
              </dd>
            </template>
          </template>
          <template v-if="member">
            <dt>Grants</dt>
            <dd>
              <span v-if="member.node" class="pill sm accent" title="An admin let it run the pool's lanes" data-testid="pool-node-granted">runs lanes</span>
              <span v-if="member.provision" class="pill sm info" title="An admin let its lanes have the pool make a node (ADR-0051)" data-testid="pool-provision-granted">may provision</span>
              <span v-if="member.no_provisioned" class="pill sm outline" title="It keeps its lanes off provisioned nodes">not on provisioned nodes</span>
              <span v-if="!member.node && !member.provision && !member.no_provisioned" class="faint">none</span>
            </dd>
            <dt>Tags</dt>
            <dd>
              <form v-if="editTags" class="stack-form" @submit.prevent="saveTags">
                <input v-model="tagsText" class="input" placeholder="team:backend, customer:acme" data-testid="pool-tags-input" />
                <button type="submit" class="btn btn-xs btn-primary">Save</button>
                <button type="button" class="btn btn-xs btn-ghost" @click="editTags = false">Cancel</button>
              </form>
              <template v-else>
                <span v-for="t in member.tags ?? []" :key="t" class="pill sm info" data-testid="pool-tag">{{ t }}</span>
                <span v-if="!member.tags?.length" class="faint">none</span>
                <button v-if="pool.isAdmin" type="button" class="btn btn-xs btn-ghost" title="Tags for nodes' policies (ADR-0033)" data-testid="pool-tags" @click="startTags">Edit</button>
              </template>
            </dd>
          </template>
        </dl>
      </div>
    </section>

    <section v-if="node?.provisioned" class="card" data-testid="pool-provisioned-row">
      <header class="card-header">
        <Icon name="server" :size="18" />
        <h2>Provisioned</h2>
        <span class="faint small">made by the pool's provisioner (ADR-0051)</span>
      </header>
      <div class="card-body">
        <dl class="facts">
          <dt>Machine</dt>
          <dd class="mono">{{ node.provisioned.machine }}</dd>
          <dt>Address</dt>
          <dd class="mono">{{ node.provisioned.address }}</dd>
          <template v-if="node.provisioned.size">
            <dt>Size</dt>
            <dd class="tabular">{{ node.provisioned.size.cpus }} CPU, {{ bytes(node.provisioned.size.memory) }}</dd>
          </template>
          <dt>Since</dt>
          <dd>{{ ago(node.provisioned.since * 1000, now) }}</dd>
          <template v-if="node.provisioned.idle_left !== undefined">
            <dt>Given back</dt>
            <dd>in {{ duration(node.provisioned.idle_left * 1000) }} without a lane</dd>
          </template>
          <template v-if="node.provisioned.repo">
            <dt>Runs</dt>
            <dd>lanes of {{ node.provisioned.repo }} only</dd>
          </template>
          <template v-if="node.provisioned.key">
            <dt>Key</dt>
            <dd class="mono">{{ node.provisioned.key }}</dd>
          </template>
        </dl>
        <p v-if="node.provisioned.unconfirmed" class="warn-line" data-testid="pool-confirm-wait">
          <Icon name="hourglass" :size="14" /> It admits no lane until an admin checks its machine, address and key against the provider and confirms it.
        </p>
        <div v-if="pool.isAdmin" class="buttons">
          <button
            v-if="node.provisioned.unconfirmed"
            type="button"
            class="btn btn-sm btn-primary"
            data-testid="pool-confirm"
            @click="pool.confirmNode(node.id, node.name, node.provisioned.machine, node.provisioned.address, node.provisioned.key)"
          >
            <Icon name="check" :size="14" /> Confirm
          </button>
          <button type="button" class="btn btn-sm btn-danger" title="Delete the machine at its provider now" data-testid="pool-give-back" @click="pool.giveBack(node.id, node.name)">
            <Icon name="trash" :size="14" /> Give back
          </button>
        </div>
      </div>
    </section>

    <section v-if="report" class="card" data-testid="pool-node-history">
      <header class="card-header">
        <Icon name="activity" :size="18" />
        <h2>Capacity over time</h2>
        <span class="faint small">what it had, offered and its lanes used</span>
      </header>
      <div class="card-body"><CapacityChart :pool="pool.current" :node="m.id" /></div>
    </section>

    <section class="card" data-testid="pool-machine-lanes">
      <header class="card-header">
        <Icon name="gitBranch" :size="18" />
        <h2>Lanes</h2>
      </header>
      <div class="card-body flush table-wrap">
        <p class="sub faint small">On it</p>
        <PoolLeases :leases="on" empty="No lane runs on it." />
        <template v-if="of.length">
          <p class="sub faint small">Its lanes elsewhere</p>
          <PoolLeases :leases="of" empty="" />
        </template>
      </div>
    </section>

    <section v-if="report?.api && (pool.isAdmin || member?.policy)" class="card" data-testid="pool-policy-card">
      <header class="card-header">
        <Icon name="shield" :size="18" />
        <h2>Policy</h2>
        <span class="faint small">who may run lanes here, which repositories, limits, network (ADR-0033)</span>
      </header>
      <div class="card-body">
        <form v-if="editPolicy" class="stack-form" data-testid="pool-policy-form" @submit.prevent="savePolicy()">
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
          <button type="button" class="btn btn-sm" @click="savePolicy(true)">Lift</button>
          <button type="button" class="btn btn-sm btn-ghost" @click="editPolicy = false">Cancel</button>
          <span class="faint small">A repository is named by the requester: a guard against mistakes, not against someone who lies.</span>
        </form>
        <p v-else class="policy">
          Runs lanes of: {{ policyText(member?.policy) }}
          <button v-if="pool.isAdmin" type="button" class="btn btn-xs" data-testid="pool-policy" @click="startPolicy"><Icon name="shield" :size="13" /> Edit</button>
        </p>
      </div>
    </section>

    <section v-if="pool.isAdmin && member" class="card" data-testid="pool-machine-admin">
      <header class="card-header">
        <Icon name="wrench" :size="18" />
        <h2>Manage</h2>
      </header>
      <div class="card-body manage">
        <label class="consent" :title="member.node ? 'The coordinator places no new lanes on it once off' : 'Let it run the pool\'s lanes as a node'">
          <input type="checkbox" :checked="!!member.node" data-testid="pool-node" @change="pool.setNode(m.id, m.name, !member.node)" />
          <span>Runs the pool's lanes</span>
        </label>
        <label v-if="provisioner || member.provision" class="consent" title="Its lanes have the pool make a node when none has room (ADR-0051)">
          <input type="checkbox" :checked="!!member.provision" data-testid="pool-provision" @change="pool.setProvision(m.id, m.name, !member.provision)" />
          <span>Its lanes may have the pool make a node</span>
        </label>
        <label class="consent" :title="lastAdmin ? lastAdminTitle : 'An admin approves, removes and manages members and nodes'">
          <input
            type="checkbox"
            :checked="member.role === 'admin'"
            :disabled="lastAdmin"
            :data-testid="member.role === 'admin' ? 'pool-make-member' : 'pool-make-admin'"
            @change="pool.setRole(m.id, m.name, member.role === 'admin' ? 'member' : 'admin')"
          />
          <span>Admin of the pool</span>
        </label>

        <div v-if="report?.api || (node?.kind === 'server' && node.online) || report?.update?.state === 'draining' || (node?.online && report?.kept?.lanes)" class="buttons">
          <button
            v-if="report?.api"
            type="button"
            class="btn btn-sm"
            :title="node?.paused ? 'Let it take lanes again' : 'No new lanes on it; running ones stay'"
            data-testid="pool-pause"
            @click="pool.setPaused(m.id, m.name, !node?.paused)"
          >
            <Icon :name="node?.paused ? 'play' : 'pause'" :size="14" /> {{ node?.paused ? 'Resume' : 'Pause' }}
          </button>
          <button
            v-if="node?.kind === 'server' && node.online"
            type="button"
            class="btn btn-sm"
            :class="{ 'btn-primary': !report?.no_update && (hub.newer || releaseClass(report?.devstack, true, hub.version) === 'warn') }"
            :disabled="!!report?.no_update"
            :title="report?.no_update ? `It does not update itself: ${report.no_update}` : 'Have this server install the latest devstack once its lanes have ended (ADR-0039)'"
            data-testid="pool-update"
            @click="pool.updateNode(m.id, m.name)"
          >
            <Icon name="download" :size="14" /> Update devstack
          </button>
          <button
            v-if="report?.update?.state === 'draining'"
            type="button"
            class="btn btn-sm"
            title="Withdraw the update: it stops waiting for its lanes and takes new ones again"
            data-testid="pool-update-cancel"
            @click="pool.cancelUpdate(m.id, m.name)"
          >
            Cancel update
          </button>
          <button
            v-if="node?.online && report?.kept?.lanes"
            type="button"
            class="btn btn-sm"
            :title="`Remove the ${report.kept.lanes} stopped lane(s) it keeps (${bytes(report.kept.bytes)})`"
            data-testid="pool-prune"
            @click="pool.pruneNode(m.id, m.name)"
          >
            <Icon name="trash" :size="14" /> Prune kept lanes
          </button>
        </div>

        <div class="danger-zone">
          <button
            type="button"
            class="btn btn-sm btn-danger"
            :disabled="lastAdmin"
            :title="lastAdmin ? lastAdminTitle : 'Its certificate is refused from now on; it can only join again with a new key'"
            data-testid="pool-remove"
            @click="pool.remove(m.id, m.name, false)"
          >
            <Icon name="trash" :size="14" /> Remove from the pool
          </button>
        </div>
      </div>
    </section>

    <section v-if="pool.isAdmin && log.length" class="card" data-testid="pool-machine-log">
      <header class="card-header">
        <Icon name="history" :size="18" />
        <h2>Admin log</h2>
        <span class="faint small">about {{ m.name }}</span>
      </header>
      <div class="card-body">
        <ul class="audit">
          <li v-for="(e, i) in log" :key="i">
            <span class="faint tabular when">{{ ago(e.at * 1000, now) }}</span>
            <span><strong>{{ e.by }}</strong> {{ actionText[e.action] ?? e.action }} <strong>{{ e.name }}</strong><template v-if="e.detail"> ({{ e.detail }})</template></span>
          </li>
        </ul>
      </div>
    </section>
  </template>
</template>

<style scoped>
.back {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  color: var(--text-muted);
  text-decoration: none;
  align-self: flex-start;
}
.back:hover {
  color: var(--text);
}
.facts dd .pill + .pill {
  margin-left: 4px;
}
.gpu {
  display: block;
}
.sub {
  margin: 0;
  padding: 10px 12px 4px;
}
.buttons {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  margin-top: 12px;
}
.manage {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.danger-zone {
  margin-top: 8px;
  padding-top: 12px;
  border-top: 1px solid var(--border);
}
.policy {
  margin: 0;
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
}
</style>
