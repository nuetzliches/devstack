<script setup lang="ts">
// Lanes of the pool (its leases), with where they run and for whom; an admin
// ends one here (ADR-0032 point 4).
import type { PoolLease } from '@/api/types';
import { usePool } from '@/stores/pool';
import Icon from '@/components/icons/Icon.vue';

defineProps<{ leases: PoolLease[]; empty: string }>();
const pool = usePool();
const isMe = (id: string) => pool.overview?.me?.id === id;
const machine = (id: string) => ({ name: 'pool-machine', params: { pool: pool.current, id } });
const what = (l: PoolLease) => `lane ${l.repo}/${l.lane} on ${pool.nameOf(l.node)}`;
</script>

<template>
  <table v-if="leases.length" class="table">
    <thead>
      <tr><th>Lane</th><th>On</th><th>For</th><th>State</th><th v-if="pool.isAdmin" /></tr>
    </thead>
    <tbody>
      <tr v-for="l in leases" :key="l.id" :data-lease="l.id">
        <td><strong>{{ l.lane }}</strong> <span class="faint">{{ l.repo }}</span></td>
        <td><RouterLink :to="machine(l.node)">{{ pool.nameOf(l.node) }}</RouterLink></td>
        <td>
          <RouterLink :to="machine(l.member)">{{ pool.nameOf(l.member) }}</RouterLink>
          <span v-if="isMe(l.member)" class="pill sm outline">this machine</span>
        </td>
        <td><span class="pill sm" :class="l.state === 'running' ? 'ok' : 'outline'">{{ l.state }}</span></td>
        <td v-if="pool.isAdmin" class="actions">
          <button type="button" class="btn btn-xs" title="Stop this lane on its node" data-testid="pool-end" @click="pool.endLease(l.id, what(l))">
            <Icon name="stop" :size="13" /> End
          </button>
        </td>
      </tr>
    </tbody>
  </table>
  <p v-else class="hint muted">{{ empty }}</p>
</template>
