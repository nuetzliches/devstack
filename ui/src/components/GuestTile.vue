<script setup lang="ts">
// The sidebar's tile for lanes of other members on this machine: how many,
// whose, what they reserved; a click opens the pool area. Shown only while
// there are any.
import { computed } from 'vue';
import { guestLabel, guestsReserved } from '@/lib/guests';
import { useHub } from '@/stores/hub';

const hub = useHub();
const guests = computed(() => hub.guests);
const shown = computed(() => guests.value.slice(0, 3));
</script>

<template>
  <RouterLink v-if="guests.length" :to="{ name: 'pool-here' }" class="guests" title="Lanes of other members on this machine" data-testid="sidebar-guests">
    <strong>{{ guests.length === 1 ? '1 guest lane' : `${guests.length} guest lanes` }}</strong>
    <span class="small tabular">{{ guestsReserved(guests) }} reserved</span>
    <span v-for="g in shown" :key="`${g.pool}/${g.lease}`" class="small truncate">
      {{ guestLabel(g) }}<template v-if="g.state !== 'running'"> (starting)</template>
    </span>
    <span v-if="guests.length > shown.length" class="small">and {{ guests.length - shown.length }} more</span>
  </RouterLink>
</template>

<style scoped>
.guests {
  display: flex;
  flex-direction: column;
  gap: 1px;
  padding: 8px;
  border-radius: 8px;
  background: var(--info-soft);
  color: var(--info-text);
  font-size: 12px;
  text-decoration: none;
  min-width: 0;
}
.guests:hover {
  text-decoration: none;
  opacity: 0.85;
}
.small {
  font-size: 11px;
}
.truncate {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
</style>
