<script setup lang="ts">
// Filters on an upstream tool's result (ADR-0069, addendum 2026-11-10): a
// local command the hub runs on fields of a result before an agent reads it,
// such as veilq on a ticket's description. The user's alone; a pool hands out
// none. A failing command fails the tool call: the field is never passed on.
import { computed, onMounted, ref, watch } from 'vue';
import { api, paths } from '@/api';
import type { FilterAnswer, McpUpstream, UpstreamFilter } from '@/api/types';
import { act } from '@/lib/actions';
import { type FilterForm, filterForm, filterFrom, filterSummary, veilqLinear } from '@/lib/filters';
import { useUi } from '@/stores/ui';
import Icon from '@/components/icons/Icon.vue';

const props = defineProps<{ upstreams: McpUpstream[] }>();
const ui = useUi();

/** The upstreams a filter can be on: the user's own, and a pool's in use. */
const usable = computed(() => props.upstreams.filter((u) => !u.pool || u.pool.status === 'accepted'));
const label = (name: string) => {
  const u = props.upstreams.find((x) => x.name === name);
  return u?.pool ? `${u.pool.name} (pool ${u.pool.pool})` : name;
};

const filters = ref<UpstreamFilter[] | null>(null);
const problem = ref<string | null>(null);

async function load() {
  try {
    const r = await api().request<{ filters: UpstreamFilter[] }>('GET', paths.upstreamFilters);
    const b = typeof r.body === 'object' ? r.body : null;
    if (r.ok && b && Array.isArray(b.filters)) {
      filters.value = b.filters;
      problem.value = null;
    } else problem.value = `The hub answered ${r.status}.`;
  } catch {
    problem.value = 'The hub does not answer.';
  }
}
onMounted(load);
// Removing an upstream takes its filters with it.
watch(
  () => props.upstreams.length,
  () => void load(),
);

const form = ref<FilterForm | null>(null);
/** The filter being changed, by its place in the list; null: a new one. */
const editing = ref<number | null>(null);
const sample = ref('');
const answer = ref<FilterAnswer | null>(null);
const failed = ref<string | null>(null);
const busy = ref(false);

function startAdd() {
  form.value = filterForm(undefined, usable.value[0]?.name ?? '');
  editing.value = null;
  answer.value = null;
  failed.value = null;
}

function startEdit(i: number) {
  form.value = filterForm(filters.value![i]);
  editing.value = i;
  answer.value = null;
  failed.value = null;
}

async function put(next: UpstreamFilter[], label: string, success: string) {
  busy.value = true;
  const r = await act({ label, method: 'PUT', path: paths.upstreamFilters, body: { filters: next }, success });
  busy.value = false;
  if (r.ok) await load();
  return r.ok;
}

async function save() {
  const f = filterFrom(form.value!);
  const next = [...(filters.value ?? [])];
  if (editing.value === null) next.push(f);
  else next[editing.value] = f;
  if (await put(next, `Filter on ${f.tool}`, `Filter on ${f.tool} saved`)) form.value = null;
}

async function remove(i: number) {
  const f = filters.value![i];
  const ok = await ui.confirm({
    title: `Remove the filter on ${f.tool}`,
    text: 'Agents then read its result as the upstream sends it.',
    confirmLabel: 'Remove',
    danger: true,
  });
  if (!ok) return;
  await put(
    filters.value!.filter((_, j) => j !== i),
    `Remove the filter on ${f.tool}`,
    'Filter removed',
  );
}

async function test() {
  answer.value = null;
  failed.value = null;
  busy.value = true;
  try {
    const r = await api().request<FilterAnswer | { error: string }>('POST', paths.upstreamFilterTest, { filter: filterFrom(form.value!), text: sample.value });
    const b = typeof r.body === 'object' ? r.body : null;
    if (r.ok && b && 'text' in b) answer.value = b;
    else failed.value = b && 'error' in b ? b.error : `The hub answered ${r.status}.`;
  } catch {
    failed.value = 'The hub does not answer.';
  } finally {
    busy.value = false;
  }
}

const ready = computed(() => {
  const f = form.value;
  return !!f && !!f.upstream && !!f.tool.trim() && !!f.paths.trim() && !!f.command.trim() && !busy.value;
});
</script>

<template>
  <section class="card" data-testid="upstream-filters">
    <header class="card-header">
      <h2>Filters on results</h2>
      <button type="button" class="btn btn-sm add" :disabled="!usable.length" data-testid="filter-add" @click="startAdd"><Icon name="plus" :size="14" /> Add</button>
    </header>
    <div class="card-body">
      <p class="muted">
        A command on this machine that the hub runs on fields of a tool's result before an agent reads it, such as <code>veilq mark -</code> on a ticket's
        description. It gets the field on stdin and answers JSON with <code>text</code>, and <code>write</code> to store back. If it fails, the tool call
        fails: the field is never passed on.
      </p>
      <p v-if="problem" class="err">{{ problem }}</p>

      <form v-if="form" class="form" data-testid="filter-form" @submit.prevent="save">
        <label
          >Upstream
          <select v-model="form.upstream" class="input">
            <option v-for="u in usable" :key="u.name" :value="u.name">{{ label(u.name) }}</option>
          </select>
        </label>
        <label>Tool <input v-model="form.tool" class="input" placeholder="get_issue" spellcheck="false" /></label>
        <label class="wide"
          >Fields <input v-model="form.paths" class="input" placeholder="description, or: issues[*].description" spellcheck="false" />
          <span class="faint small">Paths in the result's JSON, apart by commas; <code>[*]</code> for every item of a list. A field the result does not have is left alone.</span>
        </label>
        <label class="wide">Command <input v-model="form.command" class="input" placeholder="veilq mark -" spellcheck="false" /></label>
        <label>Store back with <input v-model="form.writeTool" class="input" placeholder="none; or: save_issue" spellcheck="false" /></label>
        <label v-if="form.writeTool.trim()"
          >Its arguments <textarea v-model="form.writeArgs" class="input" rows="3" placeholder="id=$args.id&#10;description=$write" spellcheck="false" />
          <span class="faint small"><code>$write</code>: the command's answer; <code>$args.id</code>: an argument of the call.</span>
        </label>
        <label class="wide"
          >Try it on <textarea v-model="sample" class="input" rows="3" placeholder="A text as the field would hold it; it stays on this machine." spellcheck="false" data-testid="filter-sample"
        /></label>
        <div v-if="answer" class="wide answer" data-testid="filter-answer">
          <span class="small faint">An agent reads:</span>
          <pre>{{ answer.text }}</pre>
          <template v-if="answer.write !== undefined"
            ><span class="small faint">Stored back:</span>
            <pre>{{ answer.write }}</pre></template
          >
        </div>
        <p v-if="failed" class="wide err small" data-testid="filter-failed">{{ failed }}</p>
        <div class="row">
          <button type="submit" class="btn btn-primary btn-sm" :disabled="!ready">Save</button>
          <button type="button" class="btn btn-sm" :disabled="!ready" data-testid="filter-test" @click="test">Try</button>
          <button v-if="editing === null" type="button" class="btn btn-ghost btn-sm" @click="form = veilqLinear(form.upstream)">veilq for Linear</button>
          <button type="button" class="btn btn-ghost btn-sm" @click="form = null">Cancel</button>
        </div>
      </form>

      <p v-if="filters && !filters.length && !form" class="muted" data-testid="filters-empty">None.</p>
      <ul v-if="filters?.length" class="list">
        <li v-for="(f, i) in filters" :key="i" class="item" data-testid="filter">
          <div class="main">
            <strong>{{ label(f.upstream) }}</strong>
            <span class="small">{{ filterSummary(f) }}</span>
          </div>
          <div class="actions">
            <button type="button" class="btn btn-ghost btn-sm" @click="startEdit(i)">Change</button>
            <button type="button" class="btn btn-ghost btn-sm" :disabled="busy" @click="remove(i)">Remove</button>
          </div>
        </li>
      </ul>
    </div>
  </section>
</template>

<style scoped>
.add {
  margin-left: auto;
}
.form {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 12px 16px;
  margin: 16px 0 4px;
}
.form label {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 13px;
}
.form .wide {
  grid-column: 1 / -1;
}
.form .row {
  grid-column: 1 / -1;
  display: flex;
  gap: 8px;
}
.answer pre {
  margin: 2px 0 8px;
  white-space: pre-wrap;
  font-size: 12px;
}
.list {
  list-style: none;
  padding: 0;
  margin: 12px 0 0;
}
.item {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  padding: 10px 0;
  border-top: 1px solid var(--border);
}
.main {
  display: flex;
  flex-direction: column;
  gap: 2px;
  flex: 1;
  min-width: 240px;
}
.actions {
  display: flex;
  gap: 6px;
}
</style>
