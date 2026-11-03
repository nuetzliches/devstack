<script setup lang="ts">
// Public links through the pool (ADR-0092). Every member sees what the pool
// offers this machine and takes its tunnel here (its links then go through
// the pool's Cloudflare account, so it is the member's choice). An admin
// also sets whether and how the pool offers them, sees whether the
// coordinator has its Cloudflare token (never the token) and whether Access
// covers the domain, and can close any open link.
import { computed, onMounted, ref, watch } from 'vue';
import { api, paths } from '@/api';
import type { PoolPublicLinksAdmin, PublicLinksPoolOffer, PublicLinksState } from '@/api/types';
import { act } from '@/lib/actions';
import { ago, useNow } from '@/lib/format';
import { DEFAULT_POOL, usePool } from '@/stores/pool';
import Icon from '@/components/icons/Icon.vue';

const pool = usePool();
const now = useNow();
const at = (path: string) => (pool.current === DEFAULT_POOL ? path : `${path}?pool=${encodeURIComponent(pool.current)}`);

const offers = ref<PublicLinksPoolOffer[]>([]);
const mine = ref<PublicLinksState | null>(null);
const admin = ref<PoolPublicLinksAdmin | null>(null);
const offer = computed(() => offers.value.find((o) => o.pool === pool.current) ?? null);
const usesThis = computed(() => mine.value?.pool === pool.current);
const ownTunnel = computed(() => mine.value?.configured && !mine.value.pool);

async function load() {
  try {
    const [o, m] = await Promise.all([
      api().request<PublicLinksPoolOffer[]>('GET', paths.publicLinksPools),
      api().request<PublicLinksState>('GET', paths.publicLinks),
    ]);
    if (o.ok && Array.isArray(o.body)) offers.value = o.body;
    if (m.ok && m.body && typeof m.body === 'object') mine.value = m.body;
    if (pool.isAdmin) {
      const a = await api().request<PoolPublicLinksAdmin>('GET', at(paths.poolPublicLinks));
      admin.value = a.ok && a.body && typeof a.body === 'object' ? a.body : null;
    }
  } catch {
    /* the card shows what it has */
  }
}
onMounted(load);
watch(() => [pool.current, pool.isAdmin], load);

async function take() {
  const o = offer.value?.offer;
  const res = await act<PublicLinksState>({
    label: `Public links through pool ${pool.current}`,
    method: 'PUT',
    path: paths.publicLinksPool,
    body: { pool: pool.current },
    confirmText: `Public links you open from now on answer below ${o?.domain}, through the Cloudflare account of pool ${pool.current}: its admins see which are open and can close them, and whoever holds that account could read what passes. Your own named tunnel, if you set one up, goes first.`,
    confirmLabel: 'Use the pool',
    danger: false,
    success: 'Public links go through the pool now',
  });
  if (res.ok && res.body) mine.value = res.body;
}

async function leave() {
  const res = await act<PublicLinksState>({ label: 'Stop using the pool for public links', method: 'DELETE', path: paths.publicLinksTunnel, success: 'Public links are Quick Tunnels again' });
  if (res.ok && res.body) mine.value = res.body;
}

// The admin's form.
const form = ref({ domain: '', mode: 'access', tag: '' });
watch(admin, (a) => {
  if (a) form.value = { domain: a.settings.domain ?? '', mode: a.settings.mode, tag: a.settings.tag ?? '' };
});
async function save() {
  const f = form.value;
  const res = await act<PoolPublicLinksAdmin>({
    label: 'Public links of the pool',
    path: at(paths.poolPublicLinks),
    body: { ...(admin.value?.settings ?? {}), domain: f.domain.trim() || null, mode: f.mode, tag: f.tag.trim() || null },
    success: 'Saved',
  });
  if (res.ok && res.body) admin.value = res.body;
}
async function close(host: string) {
  const res = await act<PoolPublicLinksAdmin>({
    label: `Close ${host}`,
    path: at(paths.poolPublicLinkClose),
    body: { host },
    confirmText: `${host} stops answering at once; its member cannot open it again until they close it on their side.`,
    confirmLabel: 'Close it',
    success: `${host} closed`,
  });
  if (res.ok && res.body) admin.value = res.body;
}
</script>

<template>
  <section v-if="offer?.offer?.domain || pool.isAdmin" class="card" data-testid="pool-public-links">
    <header class="card-header">
      <Icon name="globe" :size="18" />
      <h2>Public links</h2>
    </header>
    <div class="card-body settings">
      <div v-if="offer?.offer?.domain" class="setting" data-testid="pool-public-links-offer">
        <p class="small">
          Links below <span class="mono">*.{{ offer.offer.domain }}</span>,
          {{ offer.offer.mode === 'access' ? 'behind the pool’s Cloudflare Access login' : 'without a login in front' }};
          this machine’s end in <span class="mono">--{{ offer.offer.suffix }}</span>.
        </p>
        <p v-if="!offer.offer.allowed" class="muted small" data-testid="pool-public-links-why">{{ offer.offer.why }}</p>
        <template v-else>
          <button v-if="usesThis" type="button" class="btn btn-sm" data-testid="pool-public-links-leave" @click="leave">Stop using it</button>
          <button v-else type="button" class="btn btn-sm btn-primary" :disabled="ownTunnel" :title="ownTunnel ? 'Your own named tunnel goes first: remove it with devstack tunnel --remove-setup' : ''" data-testid="pool-public-links-use" @click="take">
            Use for my public links
          </button>
          <p v-if="usesThis" class="muted small">In use: your links answer below this domain.</p>
        </template>
      </div>
      <p v-else-if="offer?.error" class="muted small">{{ offer.error }}</p>

      <template v-if="pool.isAdmin && admin">
        <form class="setting public-form" data-testid="pool-public-links-settings" @submit.prevent="save">
          <label>Domain <input v-model="form.domain" class="input" placeholder="dev-team.link" /></label>
          <label>
            Mode
            <select v-model="form.mode" class="input">
              <option value="access">Only with Cloudflare Access</option>
              <option value="open">Open, without a login</option>
              <option value="off">Off</option>
            </select>
          </label>
          <label>Members tagged <input v-model="form.tag" class="input" placeholder="every member" /></label>
          <button type="submit" class="btn btn-sm btn-primary">Save</button>
        </form>
        <p v-if="!admin.token" class="muted small" data-testid="pool-public-links-no-token">
          The coordinator has no Cloudflare token: put one (Tunnel: Edit, DNS: Edit for the zone, Access: Read) into <span class="mono">cloudflare.token</span> in its directory, or <span class="mono">DEVSTACK_CLOUDFLARE_TOKEN</span>.
        </p>
        <p v-else-if="admin.access" class="small" :class="admin.access.Ok ? 'muted' : 'warn-text'" data-testid="pool-public-links-access">
          {{ admin.access.Ok ? `Cloudflare Access covers *.${admin.settings.domain}.` : admin.access.Ok === false ? `No Cloudflare Access application covers *.${admin.settings.domain}: members get no links until one does.` : admin.access.Err }}
        </p>
        <div class="table-wrap">
          <table v-if="admin.links.length" class="table" data-testid="pool-public-links-open">
            <thead>
              <tr><th>Address</th><th>Machine</th><th>Since</th><th /></tr>
            </thead>
            <tbody>
              <tr v-for="l in admin.links" :key="l.host">
                <td class="mono">{{ l.host }}</td>
                <td>{{ l.name || l.member }}</td>
                <td>{{ ago(l.since * 1000, now) }}</td>
                <td class="actions"><button type="button" class="btn btn-sm btn-danger" @click="close(l.host)"><Icon name="x" :size="14" /> Close</button></td>
              </tr>
            </tbody>
          </table>
          <p v-else class="muted small">No public link is open in the pool.</p>
        </div>
      </template>
    </div>
  </section>
</template>

<style scoped>
.public-form {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  align-items: end;
}
.public-form label {
  display: flex;
  flex-direction: column;
  gap: 2px;
  font-size: 12.5px;
}
.warn-text {
  color: var(--warn-text);
}
</style>
