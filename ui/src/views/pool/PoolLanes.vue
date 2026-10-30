<script setup lang="ts">
// The pool's lanes, where they run, and the lanes others share with this
// machine in it (ADR-0035), to watch, open and connect to (ADR-0053).
import { computed } from 'vue';
import { sharedId } from '@/api';
import { connectedTo, usePool } from '@/stores/pool';
import Icon from '@/components/icons/Icon.vue';
import PoolLeases from './PoolLeases.vue';
import { laneView } from '@/lib/stackRoutes';

const pool = usePool();
const leases = computed(() => pool.overview?.leases ?? []);
const sharedHere = computed(() => pool.shared.filter((l) => l.pool === pool.current));
</script>

<template>
  <section class="card" data-testid="pool-lanes">
    <header class="card-header">
      <Icon name="gitBranch" :size="18" />
      <h2>Lanes</h2>
      <span class="count">{{ leases.length }}</span>
    </header>
    <div class="card-body flush table-wrap">
      <PoolLeases :leases="leases" empty="No lane runs in the pool." />
    </div>
  </section>

  <section v-if="sharedHere.length" class="card" data-testid="pool-shared">
    <header class="card-header">
      <Icon name="globe" :size="18" />
      <h2>Shared with you</h2>
      <span class="count">{{ sharedHere.length }}</span>
    </header>
    <div class="card-body flush table-wrap">
      <table class="table">
        <thead>
          <tr><th>Lane</th><th>On</th><th>Shared by</th><th>Services</th><th>Here</th></tr>
        </thead>
        <tbody>
          <tr v-for="l in sharedHere" :key="l.lease">
            <td>
              <RouterLink :to="laneView(sharedId(l.pool, l.lease), null)" title="Its state, logs and events, read only" data-testid="pool-shared-view">
                <strong>{{ l.lane }}</strong>
              </RouterLink>
              <span class="faint repo">{{ l.repo }}</span>
            </td>
            <td>{{ l.node.name }}</td>
            <td>{{ l.owner_name }}</td>
            <td>
              <button
                v-for="s in l.services"
                :key="s.name"
                type="button"
                class="btn btn-xs svc"
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
    </div>
  </section>
</template>

<style scoped>
.repo {
  margin-left: 6px;
}
.svc + .svc {
  margin-left: 4px;
}
</style>
