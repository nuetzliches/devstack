<script setup lang="ts">
// What only an admin sees (ADR-0032): the requests to join, the nodes the
// provisioner made (ADR-0051), the MCP upstreams the pool names (ADR-0069),
// the skill sets it hands out (ADR-0074) and the admin log. The coordinator checks the role on every action.
import { computed, watchEffect } from 'vue';
import { useRouter } from 'vue-router';
import { ago, bytes, useNow } from '@/lib/format';
import { actionText } from '@/lib/poolText';
import { usePool } from '@/stores/pool';
import Icon from '@/components/icons/Icon.vue';
import PoolUpstreams from '@/components/PoolUpstreams.vue';
import PoolSkills from '@/components/PoolSkills.vue';

const pool = usePool();
const now = useNow();
const o = computed(() => pool.overview);
const provisioning = computed(() => o.value?.provisioning);
const machines = computed(() => [...(provisioning.value?.machines ?? [])].sort((a, b) => Number(!!b.alarm) - Number(!!a.alarm)));
const alarms = computed(() => machines.value.filter((m) => m.alarm).length);
const audit = computed(() => (o.value?.audit ?? []).slice(0, 50));
const machine = (id: string) => ({ name: 'pool-machine', params: { pool: pool.current, id } });
// Another pool's tab keeps this one; where this machine is no admin, its overview.
const router = useRouter();
watchEffect(() => {
  if (o.value?.me && !pool.isAdmin) void router.replace({ name: 'pool-overview', params: { pool: pool.current } });
});
</script>

<template>
  <p v-if="!pool.isAdmin" class="muted">Only an admin of the pool sees this.</p>
  <template v-else>
    <section class="card" data-testid="pool-requests">
      <header class="card-header">
        <Icon name="key" :size="18" />
        <h2>Requests to join</h2>
        <span class="count">{{ pool.pending.length }}</span>
      </header>
      <div class="card-body flush table-wrap">
        <table v-if="pool.pending.length" class="table">
          <thead>
            <tr><th>Machine</th><th>Id</th><th>From</th><th>Asked</th><th /></tr>
          </thead>
          <tbody>
            <tr v-for="m in pool.pending" :key="m.id" :data-member="m.id">
              <td><strong>{{ m.name }}</strong></td>
              <td class="mono faint">{{ m.id }}</td>
              <td class="mono">{{ m.address ?? '–' }}</td>
              <td>{{ ago(m.requested_at * 1000, now) }}</td>
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
        <p class="hint muted">
          {{ pool.pending.length ? "Check the machine's name and where it asked from before approving: a request alone grants nothing." : 'No machine asks to join.' }}
        </p>
      </div>
    </section>

    <div class="cols">
      <div class="col">
        <PoolUpstreams />
        <PoolSkills />

      </div>
      <div class="col">
        <section v-if="provisioning" class="card" data-testid="pool-provisioning">
          <header class="card-header">
            <Icon name="server" :size="18" />
            <h2>Provisioning</h2>
            <span v-if="alarms" class="pill sm danger" data-testid="pool-provision-alarms">{{ alarms }} alarm{{ alarms === 1 ? '' : 's' }}</span>
            <span class="faint small">
              <template v-if="provisioning.provisioner">{{ provisioning.provisioner }}</template>
              <template v-else>no provisioner: the pool makes no nodes</template>
            </span>
          </header>
          <div class="card-body flush table-wrap">
            <p class="hint muted" data-testid="pool-provision-settings">
              One node {{ provisioning.settings.per === 'repo' ? 'per repository' : 'for the whole pool' }} ·
              {{ provisioning.settings.confirm === 'admin' ? 'an admin confirms each' : 'no confirmation' }} ·
              given back after {{ provisioning.settings.idle_minutes }} min without a lane · at most {{ provisioning.settings.max_nodes }} node(s)<template
                v-if="provisioning.settings.max_cpus"
              >, {{ provisioning.settings.max_cpus }} CPU</template><template v-if="provisioning.settings.max_memory">, {{ bytes(provisioning.settings.max_memory) }}</template>
            </p>
            <table v-if="machines.length" class="table">
              <thead>
                <tr><th>Request</th><th>Machine</th><th>Address</th><th>State</th><th>Node</th><th>For</th></tr>
              </thead>
              <tbody>
                <template v-for="m in machines" :key="m.request">
                  <tr :data-request="m.request" :class="{ alarm: m.alarm }">
                    <td class="mono faint">{{ m.request }}</td>
                    <td class="mono">{{ m.machine || '–' }}</td>
                    <td class="mono">{{ m.address || '–' }}</td>
                    <td class="nowrap">
                      <span class="pill sm" :class="m.state === 'ready' ? 'ok' : m.state === 'returning' ? 'warn' : 'outline'">{{ m.state }}</span>
                      <span v-if="m.size" class="faint small tabular"> {{ m.size.cpus }} CPU · {{ bytes(m.size.memory) }}</span>
                    </td>
                    <td><RouterLink v-if="m.member" :to="machine(m.member)">{{ pool.nameOf(m.member) }}</RouterLink><template v-else>–</template></td>
                    <td>{{ pool.nameOf(m.requester) }}<span v-if="m.repo" class="faint"> · {{ m.repo }}</span></td>
                  </tr>
                  <tr v-if="m.alarm || m.error" class="note-row">
                    <td colspan="6">
                      <p v-if="m.alarm" class="err" data-testid="pool-provision-alarm"><Icon name="alert" :size="14" /> <strong>Alarm:</strong> {{ m.alarm }}</p>
                      <p v-if="m.error" class="warn-line small" data-testid="pool-provision-error"><Icon name="alert" :size="14" /> Giving it back failed: {{ m.error }}</p>
                    </td>
                  </tr>
                </template>
              </tbody>
            </table>
            <p v-else class="hint muted">The provisioner has made no machine.</p>
          </div>
        </section>

        <section v-if="audit.length" class="card" data-testid="pool-audit">
          <header class="card-header">
            <Icon name="history" :size="18" />
            <h2>Admin log</h2>
          </header>
          <div class="card-body">
            <ul class="audit">
              <li v-for="(e, i) in audit" :key="i">
                <span class="faint tabular when">{{ ago(e.at * 1000, now) }}</span>
                <span>
                  <strong>{{ e.by }}</strong> {{ actionText[e.action] ?? e.action }}
                  <template v-if="!e.action.startsWith('upstream')">
                    <RouterLink v-if="pool.machines.some((x) => x.id === e.member)" :to="machine(e.member)"><strong>{{ e.name }}</strong></RouterLink>
                    <strong v-else>{{ e.name }}</strong>
                  </template>
                  <template v-if="e.detail"> ({{ e.detail }})</template>
                </span>
              </li>
            </ul>
          </div>
        </section>
      </div>
    </div>
  </template>
</template>

<style scoped>
tr.alarm td {
  background: color-mix(in srgb, var(--danger) 10%, transparent);
}
.note-row td {
  padding-top: 0;
}
</style>
