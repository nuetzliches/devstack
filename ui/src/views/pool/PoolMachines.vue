<script setup lang="ts">
// The pool's machines, one row each: a member and, once it reports, its node
// side (ADR-0032, amended 2026-10-17). The row tells the state; what can be
// done with a machine is on its page.
import { computed, ref } from 'vue';
import { ago, bytes, useNow } from '@/lib/format';
import { kindText, kindTitle, releaseClass, releaseTitle, updateText } from '@/lib/poolText';
import { usePool } from '@/stores/pool';
import { useHub } from '@/stores/hub';
import Icon from '@/components/icons/Icon.vue';

const pool = usePool();
const hub = useHub();
const now = useNow();
const o = computed(() => pool.overview);
const isMe = (id: string) => o.value?.me?.id === id;
const lanesOn = (id: string) => (o.value?.leases ?? []).filter((l) => l.node === id).length;
const removed = computed(() => (o.value?.members ?? []).filter((m) => m.state === 'removed'));
const showRemoved = ref(false);
</script>

<template>
  <section class="card" data-testid="pool-nodes">
    <div class="card-body flush table-wrap">
      <table class="table">
        <thead>
          <tr><th>Machine</th><th>State</th><th>Role</th><th>devstack</th><th>Shares</th><th>Lanes</th><th>Last report</th></tr>
        </thead>
        <tbody>
          <tr v-for="m in pool.machines" :key="m.id" :data-node="m.id" :data-member="m.id">
            <td>
              <RouterLink :to="{ name: 'pool-machine', params: { pool: pool.current, id: m.id } }" data-testid="pool-machine-link"><strong>{{ m.name }}</strong></RouterLink>
              <div class="pills">
                <span v-if="isMe(m.id)" class="pill sm outline">this machine</span>
                <span class="pill sm" :class="m.node?.kind === 'server' ? 'accent' : 'outline'" :title="kindTitle[m.node?.kind ?? 'unknown']" data-testid="pool-kind">{{ kindText[m.node?.kind ?? 'unknown'] }}</span>
                <span v-if="m.node?.provisioned" class="pill sm info" data-testid="pool-provisioned">provisioned</span>
                <span v-if="m.node?.report?.gpus?.length" class="pill sm info" :title="m.node.report.gpus.map((g) => `${g.name}, ${bytes(g.memory)}`).join('; ')">GPU</span>
              </div>
            </td>
            <td>
              <div class="pills">
                <span class="pill sm" :class="m.node?.online ? 'ok' : 'outline'">{{ m.node?.online ? 'online' : 'offline' }}</span>
                <span v-if="m.node?.paused" class="pill sm warn" title="Paused by an admin: no new lanes, running ones stay">paused</span>
                <span v-if="m.node?.provisioned?.unconfirmed" class="pill sm warn" title="It admits no lane until an admin confirms it (ADR-0051 point 6)" data-testid="pool-unconfirmed">unconfirmed</span>
              </div>
            </td>
            <td>
              <div class="pills">
                <span v-if="m.member" class="pill sm" :class="m.member.role === 'admin' ? 'accent' : 'outline'">{{ m.member.role }}</span>
                <span v-if="m.member?.node" class="pill sm accent" title="An admin let it run the pool's lanes" data-testid="pool-node-granted">runs lanes</span>
              </div>
            </td>
            <td data-testid="pool-node-version">
              <div class="pills">
                <span v-if="m.node?.report" class="pill sm" :class="releaseClass(m.node.report.devstack, true, hub.version)" :title="releaseTitle(m.node.report.devstack, m.name, hub.version)">
                  {{ m.node.report.devstack?.version ?? 'older' }}
                </span>
                <span v-else class="faint">–</span>
                <span
                  v-if="m.node && updateText(m.node)"
                  class="pill sm"
                  :class="['failed', 'refused'].includes(m.node.report!.update!.state) ? 'danger' : 'info'"
                  :title="m.node.report!.update!.error ?? `update to ${m.node.report!.update!.target ?? 'the latest release'}`"
                  data-testid="pool-node-update"
                >{{ updateText(m.node) }}</span>
              </div>
            </td>
            <td class="tabular nowrap">
              <template v-if="m.node?.report?.sharing">{{ m.node.report.cpus.toFixed(1) }} CPU · {{ bytes(m.node.report.memory) }}</template>
              <span v-else class="faint">not shared</span>
            </td>
            <td class="tabular nowrap">
              {{ lanesOn(m.id) || '–' }}
              <span v-if="m.node?.report?.kept?.lanes" class="faint small" :title="`${m.node.report.kept.lanes} stopped lane(s) kept there for their next start`" data-testid="pool-node-kept">
                · {{ m.node.report.kept.lanes }} kept
              </span>
            </td>
            <td class="faint nowrap">{{ m.node?.seen === undefined ? 'never' : ago(now - m.node.seen * 1000, now) }}</td>
          </tr>
        </tbody>
      </table>
      <template v-if="pool.isAdmin && removed.length">
        <p class="hint">
          <button type="button" class="btn btn-xs btn-ghost" @click="showRemoved = !showRemoved">
            <Icon :name="showRemoved ? 'chevronDown' : 'chevronRight'" :size="13" /> {{ removed.length }} removed or rejected
          </button>
        </p>
        <ul v-if="showRemoved" class="removed">
          <li v-for="m in removed" :key="m.id" class="faint">{{ m.name }} <span class="mono">{{ m.id }}</span></li>
        </ul>
      </template>
    </div>
  </section>
</template>

<style scoped>
.pills {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
  margin-top: 2px;
}
.removed {
  list-style: none;
  margin: 0;
  padding: 0 12px 10px;
  display: flex;
  flex-direction: column;
  gap: 6px;
  font-size: 13px;
}
</style>
