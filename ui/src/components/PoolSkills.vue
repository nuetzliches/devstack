<script setup lang="ts">
// The skill sets a pool hands out (ADR-0074), in the pool area, for admins:
// a GitHub repository (read with the coordinator's own token) or a folder on
// the coordinator, given to every member, member ids or tags. The
// coordinator fetches a set when it is saved, when asked here, and when its
// source tells it changed (a signed refresh); each member's hub offers it on
// the Skills page.
import { onMounted, ref, watch } from 'vue';
import { api, paths } from '@/api';
import type { PoolSkillSet, PoolSkillsAdmin } from '@/api/types';
import { act } from '@/lib/actions';
import { ago, useNow } from '@/lib/format';
import { DEFAULT_POOL, usePool } from '@/stores/pool';
import Icon from '@/components/icons/Icon.vue';

const pool = usePool();
const now = useNow();
const view = ref<PoolSkillsAdmin | null>(null);
const problem = ref<string | null>(null);
const at = (path: string) => (pool.current === DEFAULT_POOL ? path : `${path}${path.includes('?') ? '&' : '?'}pool=${encodeURIComponent(pool.current)}`);

async function load() {
  if (!pool.isAdmin) return;
  try {
    const r = await api().request<PoolSkillsAdmin>('GET', at(paths.poolSkills));
    if (r.ok && r.body && typeof r.body === 'object' && Array.isArray(r.body.sets)) {
      view.value = r.body;
      problem.value = null;
    } else if (r.status === 404) problem.value = "The pool's coordinator runs an older devstack: it hands out no skills.";
    else problem.value = (r.body && typeof r.body === 'object' && 'error' in r.body ? String((r.body as { error: unknown }).error) : null) ?? `The hub answered ${r.status}.`;
  } catch {
    problem.value = 'The hub does not answer.';
  }
}
onMounted(load);
watch(() => [pool.current, pool.isAdmin], load);
// The coordinator tells when a fetch changed the sets (ADR-0073).
watch(() => pool.feeds[pool.current]?.versions.skills, load);

type Form = { name: string; kind: 'github' | 'folder'; repo: string; ref: string; path: string; to: string };
const empty = (): Form => ({ name: '', kind: 'github', repo: '', ref: 'main', path: 'skills', to: '*' });
const form = ref<Form>(empty());
const formOpen = ref(false);
const busy = ref(false);

function startAdd() {
  form.value = empty();
  formOpen.value = true;
}

function startEdit(s: PoolSkillSet) {
  form.value =
    s.source.kind === 'github'
      ? { name: s.name, kind: 'github', repo: s.source.repo, ref: s.source.ref, path: s.source.path, to: s.to.join(', ') }
      : { name: s.name, kind: 'folder', repo: '', ref: 'main', path: s.source.path, to: s.to.join(', ') };
  formOpen.value = true;
}

async function save() {
  const f = form.value;
  const body: PoolSkillSet = {
    name: f.name.trim(),
    source: f.kind === 'github' ? { kind: 'github', repo: f.repo.trim(), ref: f.ref.trim() || 'main', path: f.path.trim() } : { kind: 'folder', path: f.path.trim() },
    to: f.to
      .split(',')
      .map((t) => t.trim())
      .filter(Boolean),
  };
  busy.value = true;
  const r = await act<PoolSkillsAdmin>({ label: `Hand out ${body.name}`, path: at(paths.poolSkills), body, success: `The pool hands out ${body.name}; the coordinator fetches it now` });
  busy.value = false;
  if (r.ok) {
    formOpen.value = false;
    await load();
  }
}

async function remove(name: string) {
  const r = await act({
    label: `Stop handing out ${name}`,
    method: 'DELETE',
    path: at(paths.poolSkill(name)),
    confirmText: "Members' hubs remove its skills from their agents' folders (but ones a member changed).",
    confirmLabel: 'Stop it',
    success: `The pool no longer hands out ${name}`,
  });
  if (r.ok) await load();
}

async function refresh(name?: string) {
  await act({
    label: name ? `Fetch ${name} again` : 'Fetch every set again',
    path: at(name ? `${paths.poolSkillsRefresh}?set=${encodeURIComponent(name)}` : paths.poolSkillsRefresh),
    success: 'The coordinator fetches again',
  });
}

const sourceText = (s: PoolSkillSet) => (s.source.kind === 'github' ? `${s.source.repo}@${s.source.ref}${s.source.path ? `/${s.source.path}` : ''}` : `folder ${s.source.path}`);
</script>

<template>
  <section v-if="pool.isAdmin" class="card" data-testid="pool-skills">
    <header class="card-header">
      <Icon name="layers" :size="18" />
      <h2>Skill sets</h2>
      <span class="faint small">{{ view ? `${view.sets.length} handed out` : '' }}</span>
      <button v-if="view?.sets.length" type="button" class="btn btn-ghost btn-sm add" @click="refresh()">Fetch again</button>
      <button type="button" class="btn btn-sm" :class="{ add: !view?.sets.length }" data-testid="pool-skill-add" @click="startAdd"><Icon name="plus" :size="14" /> Add</button>
    </header>
    <div class="card-body">
      <p class="muted small">
        Agent skills this pool hands out. The coordinator fetches them with its own access and packs them; each member's hub offers them on its
        <router-link to="/skills">Skills</router-link> page and installs a set once its user accepts it.
      </p>
      <p v-if="view && !view.github_token" class="faint small">No <code>DEVSTACK_SKILLS_GITHUB_TOKEN</code> on the coordinator: public repositories only.</p>
      <p v-if="view && !view.refresh_signed" class="faint small">
        No <code>DEVSTACK_SKILLS_REFRESH_SECRET</code> on the coordinator: a push is not told, fetch again by hand.
      </p>
      <p v-if="problem" class="err">{{ problem }}</p>

      <form v-if="formOpen" class="form" data-testid="pool-skill-form" @submit.prevent="save">
        <label>Name <input v-model="form.name" class="input" placeholder="core" spellcheck="false" /></label>
        <label
          >Source
          <select v-model="form.kind" class="input">
            <option value="github">a GitHub repository</option>
            <option value="folder">a folder on the coordinator</option>
          </select>
        </label>
        <template v-if="form.kind === 'github'">
          <label>Repository <input v-model="form.repo" class="input" placeholder="org/skills" spellcheck="false" /></label>
          <label>Branch, tag or commit <input v-model="form.ref" class="input" placeholder="main" spellcheck="false" /></label>
          <label class="wide">Folder in it <input v-model="form.path" class="input" placeholder="skills; empty: the top" spellcheck="false" /></label>
        </template>
        <label v-else class="wide">Folder below skill-sources/ <input v-model="form.path" class="input" placeholder="core" spellcheck="false" /></label>
        <label class="wide">Given to <input v-model="form.to" class="input" placeholder="*, or member ids and tags" spellcheck="false" /></label>
        <div class="row">
          <button type="submit" class="btn btn-primary btn-sm" :disabled="!form.name.trim() || !(form.kind === 'github' ? form.repo.trim() : form.path.trim()) || busy">Save</button>
          <button type="button" class="btn btn-sm" @click="formOpen = false">Cancel</button>
        </div>
      </form>

      <p v-if="view && !view.sets.length && !formOpen" class="muted small" data-testid="pool-skills-empty">The pool hands out none.</p>
      <ul v-if="view?.sets.length" class="list">
        <li v-for="s in view.sets" :key="s.name" class="item" data-testid="pool-skill" :data-name="s.name">
          <div class="main">
            <strong>{{ s.name }}</strong>
            <span class="faint small">{{ sourceText(s) }} · to {{ s.to.join(', ') || 'nobody' }}</span>
            <span class="small">
              {{ s.manifest.skills.length }} skill(s)<template v-if="s.manifest.commit"> · {{ s.manifest.commit }}</template
              ><template v-if="s.manifest.fetched"> · fetched {{ ago(s.manifest.fetched * 1000, now) }}</template>
            </span>
            <span v-if="s.manifest.error" class="err small">Last fetch failed: {{ s.manifest.error }}</span>
            <span v-for="k in s.manifest.skipped ?? []" :key="k" class="faint small">left out: {{ k }}</span>
          </div>
          <div class="actions">
            <button type="button" class="btn btn-ghost btn-sm" @click="refresh(s.name)">Fetch</button>
            <button type="button" class="btn btn-ghost btn-sm" @click="startEdit(s)">Change</button>
            <button type="button" class="btn btn-ghost btn-sm" data-testid="pool-skill-remove" @click="remove(s.name)">Remove</button>
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
