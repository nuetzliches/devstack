<script setup lang="ts">
// What the share switch's node agent needs (ADR-0113), each with the command
// that sets it up: the command alone is copied, where it runs stands above
// it. Shown on the page and in the setup dialog.
import type { ShareCheck } from '@/api/types';
import { useUi } from '@/stores/ui';

defineProps<{ checks: ShareCheck[]; prompt?: string }>();
const ui = useUi();

async function copy(text: string, what: string) {
  try {
    await navigator.clipboard.writeText(text);
    ui.toast('success', `${what} copied`, '');
  } catch {
    ui.toast('error', 'Could not copy', 'The browser did not allow it.');
  }
}
</script>

<template>
  <ul v-if="checks.length" class="checks" data-testid="share-checks">
    <li v-for="c in checks" :key="c.name" :class="c.ok ? 'ok' : c.optional ? 'muted' : 'warn'">
      <span class="mark">{{ c.ok ? '✓' : c.optional ? '–' : '✗' }}</span>
      <span>{{ c.detail }}<span v-if="!c.ok && c.optional" class="faint"> (optional)</span></span>
      <span v-if="!c.ok && c.fix" class="fix">
        <span v-if="c.fix_note" class="note" data-testid="share-fix-note">{{ c.fix_note }}:</span>
        <span class="cmd">
          <code data-testid="share-fix">{{ c.fix }}</code>
          <button type="button" class="btn btn-xs" @click="copy(c.fix, 'Command')">Copy</button>
        </span>
      </span>
    </li>
  </ul>
  <p v-if="prompt && checks.some((c) => !c.ok)" class="small">
    <button type="button" class="btn btn-sm" data-testid="share-prompt" @click="copy(prompt, 'Prompt')">Copy a setup prompt for a coding agent</button>
    <span class="faint"> It sets up what is missing, asking before each step; what needs an elevated shell stays with you.</span>
  </p>
</template>

<style scoped>
.small {
  font-size: 11px;
}
.ok {
  color: var(--ok);
}
.warn {
  color: var(--warn);
}
.muted {
  color: var(--text-muted);
}
.checks {
  list-style: none;
  margin: 8px 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 6px;
  font-size: 13px;
}
.checks li {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  align-items: baseline;
}
.mark {
  width: 1em;
  flex: none;
}
.fix {
  display: flex;
  flex-direction: column;
  gap: 4px;
  flex-basis: 100%;
  padding-left: 1.4em;
  min-width: 0;
}
.note {
  font-size: 12px;
  color: var(--text-muted);
}
.cmd {
  display: flex;
  gap: 6px;
  align-items: baseline;
  min-width: 0;
}
.cmd code {
  font-size: 11px;
  overflow-wrap: anywhere;
  color: var(--text);
}
</style>
