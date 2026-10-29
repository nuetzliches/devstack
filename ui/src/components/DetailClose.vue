<script setup lang="ts">
// The × of a service's or a container's detail: back to the overview with
// nothing picked. Escape does the same (OverviewView).
import { useStack } from '@/stores/stack';
import Icon from './icons/Icon.vue';

const stack = useStack();
</script>

<template>
  <RouterLink
    :to="{ name: 'overview', params: { stack: stack.id ?? '' } }"
    class="btn btn-ghost btn-icon detail-close"
    title="Close (Esc): back to the overview"
    aria-label="Close the detail"
    data-testid="detail-close"
  >
    <Icon name="x" :size="16" />
  </RouterLink>
</template>
