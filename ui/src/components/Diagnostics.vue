<script setup lang="ts">
// Compiler output, readable: file(line,col) / file:line:col emphasised,
// "error" and "warning" coloured. Works for MSBuild/C#, tsc, rustc-style and
// eslint-style lines; anything else is shown as plain text.
import { computed } from 'vue';

const props = defineProps<{ text: string; maxHeight?: string }>();

type Seg = { t: string; cls?: string };
type Line = { segs: Seg[]; kind: 'error' | 'warning' | 'plain' };

// C#/MSBuild: path(12,5): error CS1002: message
const msbuild = /^(\s*)(.+?)\((\d+)(?:,(\d+))?\)\s*:\s*(error|warning)\s+([A-Z]+\d+)?\s*:?\s*(.*)$/i;
// tsc / gcc / eslint-ish: path:12:5: error TS1005: message  |  path:12:5  error  message
const colon = /^(\s*)((?:[A-Za-z]:)?[^\s:][^:]*?):(\d+)(?::(\d+))?:?\s+(error|warning)\b:?\s*([A-Z]+\d+)?:?\s*(.*)$/i;
// rustc: error[E0308]: message  /  --> src/main.rs:4:5
const rustHead = /^(\s*)(error|warning)(\[[A-Z]\d+\])?:\s*(.*)$/i;
const rustLoc = /^(\s*-->\s*)(.+?):(\d+):(\d+)(.*)$/;

const lines = computed<Line[]>(() =>
  props.text.split(/\r?\n/).map((raw) => {
    let m = msbuild.exec(raw) ?? colon.exec(raw);
    if (m) {
      const [, ws, file, line, col, sev, code, msg] = m;
      const kind = sev.toLowerCase() === 'error' ? 'error' : 'warning';
      const segs: Seg[] = [
        { t: ws },
        { t: file, cls: 'file' },
        { t: `:${line}${col ? `:${col}` : ''}`, cls: 'loc' },
        { t: '  ' },
        { t: sev.toLowerCase(), cls: kind },
      ];
      if (code) segs.push({ t: ` ${code}`, cls: 'code' });
      segs.push({ t: `  ${msg}`, cls: 'msg' });
      return { segs, kind };
    }
    m = rustHead.exec(raw);
    if (m) {
      const kind = m[2].toLowerCase() === 'error' ? 'error' : 'warning';
      return { kind, segs: [{ t: m[1] }, { t: m[2].toLowerCase(), cls: kind }, { t: m[3] ?? '', cls: 'code' }, { t: `: ${m[4]}`, cls: 'msg' }] };
    }
    m = rustLoc.exec(raw);
    if (m) return { kind: 'plain', segs: [{ t: m[1] }, { t: m[2], cls: 'file' }, { t: `:${m[3]}:${m[4]}`, cls: 'loc' }, { t: m[5] }] };
    return { kind: 'plain', segs: [{ t: raw }] };
  }),
);

const counts = computed(() => ({
  errors: lines.value.filter((l) => l.kind === 'error').length,
  warnings: lines.value.filter((l) => l.kind === 'warning').length,
}));

defineExpose({ counts });
</script>

<template>
  <div class="diag" :style="{ maxHeight: maxHeight ?? 'none' }" data-testid="diagnostics">
    <div v-for="(l, i) in lines" :key="i" class="diag-line" :class="`is-${l.kind}`">
      <template v-for="(s, j) in l.segs" :key="j"><span v-if="s.cls" :class="s.cls">{{ s.t }}</span><template v-else>{{ s.t }}</template></template>
    </div>
  </div>
</template>

<style scoped>
.diag {
  font-family: var(--font-mono);
  font-size: 12.5px;
  line-height: 1.6;
  background: var(--log-bg);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  padding: 10px 12px;
  overflow: auto;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  color: var(--text-2);
}
.diag-line {
  min-height: 1.6em;
  padding-left: 8px;
  border-left: 2px solid transparent;
  margin-left: -8px;
}
.diag-line.is-error {
  border-left-color: var(--danger);
  color: var(--text);
}
.diag-line.is-warning {
  border-left-color: var(--warn);
}
.file {
  font-weight: 700;
  color: var(--text);
}
.loc {
  font-weight: 700;
  color: var(--accent-text);
}
.error {
  color: var(--danger-text);
  font-weight: 700;
}
.warning {
  color: var(--warn-text);
  font-weight: 700;
}
.code {
  color: var(--text-muted);
}
</style>
