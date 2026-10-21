<script setup lang="ts">
// Lanes of other members run on this machine: said on every page, below the
// top bar, until it is dismissed; a lane that comes or goes shows it again.
import { computed, ref } from 'vue';
import { useRoute } from 'vue-router';
import { guestLabel, guestsReserved, guestsSignature } from '@/lib/guests';
import { persist } from '@/lib/persist';
import { useHub } from '@/stores/hub';
import Icon from './icons/Icon.vue';

const DISMISSED = 'guestsDismissed';

const hub = useHub();
const route = useRoute();
const dismissed = ref(persist.get<string>(DISMISSED));
const signature = computed(() => guestsSignature(hub.guests));
const shown = computed(() => hub.guests.length > 0 && dismissed.value !== signature.value);
const running = computed(() => hub.guests.filter((g) => g.state === 'running'));
const names = computed(() => {
  const list = hub.guests.slice(0, 3).map(guestLabel);
  const more = hub.guests.length - list.length;
  return more > 0 ? `${list.join(', ')} and ${more} more` : list.join(', ');
});

function dismiss() {
  dismissed.value = signature.value;
  persist.set(DISMISSED, signature.value);
}
</script>

<template>
  <div v-if="shown" class="guest-banner" role="status" data-testid="guest-banner">
    <div class="inner">
      <Icon name="server" :size="16" />
      <div class="grow">
        <strong>
          {{ hub.guests.length === 1 ? 'A lane of another member' : `${hub.guests.length} lanes of other members` }}
          {{ running.length === hub.guests.length ? (hub.guests.length === 1 ? 'runs' : 'run') : 'run or start' }} on this machine:
        </strong>
        {{ names }} <span class="faint">({{ guestsReserved(hub.guests) }} reserved)</span>
      </div>
      <RouterLink v-if="route.name !== 'pool'" :to="{ name: 'pool' }" class="btn btn-sm" data-testid="guest-banner-show">Show</RouterLink>
      <button type="button" class="btn btn-ghost btn-sm btn-icon" title="Hide until a lane comes or goes" aria-label="Hide" data-testid="guest-banner-dismiss" @click="dismiss">
        <Icon name="x" :size="14" />
      </button>
    </div>
  </div>
</template>

<style scoped>
.guest-banner {
  background: var(--info-soft);
  border-bottom: 1px solid color-mix(in srgb, var(--info) 30%, transparent);
  color: var(--info-text);
}
.inner {
  max-width: 1280px;
  margin: 0 auto;
  padding: 8px var(--space-5);
  display: flex;
  align-items: center;
  gap: 12px;
  font-size: 13px;
}
@media (max-width: 640px) {
  .inner {
    padding: 8px var(--space-3);
  }
}
.faint {
  opacity: 0.8;
}
</style>
