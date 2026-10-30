<script setup lang="ts">
// The MCP upstreams a pool names for its members (ADR-0069 option B): a list
// of URLs, never a user's credential. Every member sees it; an admin adds and
// removes entries (the coordinator checks the role and audits each change).
// Each member's hub offers them on the MCP upstreams page and uses one only
// once its user accepts it, with that user's own login. An OAuth app's client
// secret goes to the members' hubs with the list, but is shown here only as
// set.
import { onMounted, ref, watch } from 'vue';
import { api, paths } from '@/api';
import type { PoolUpstream } from '@/api/types';
import { act } from '@/lib/actions';
import { oauthAuth } from '@/lib/oauthAuth';
import { DEFAULT_POOL, usePool } from '@/stores/pool';
import Icon from '@/components/icons/Icon.vue';

const pool = usePool();
const list = ref<PoolUpstream[] | null>(null);
const problem = ref<string | null>(null);
const at = (path: string) => (pool.current === DEFAULT_POOL ? path : `${path}?pool=${encodeURIComponent(pool.current)}`);

async function load() {
  try {
    const r = await api().request<PoolUpstream[]>('GET', at(paths.poolUpstreams));
    if (r.ok && Array.isArray(r.body)) {
      list.value = r.body;
      problem.value = null;
    } else problem.value = (r.body && typeof r.body === 'object' && 'error' in r.body ? String((r.body as { error: unknown }).error) : null) ?? `The hub answered ${r.status}.`;
  } catch {
    problem.value = 'The hub does not answer.';
  }
}
onMounted(load);
watch(() => pool.current, load);

type Form = {
  name: string;
  url: string;
  prefix: string;
  allow: string;
  kind: 'oauth' | 'token' | 'none';
  client_id: string;
  /** A new client secret; empty keeps the one set. */
  client_secret: string;
  /** The entry has one (the hub never shows it). */
  secret_set: boolean;
  drop_secret: boolean;
  scopes: string;
};
const empty = (): Form => ({ name: '', url: '', prefix: '', allow: '', kind: 'oauth', client_id: '', client_secret: '', secret_set: false, drop_secret: false, scopes: '' });
const form = ref<Form>(empty());
const formOpen = ref(false);
const busy = ref(false);

function startAdd() {
  form.value = empty();
  formOpen.value = true;
}

function startEdit(u: PoolUpstream) {
  form.value = {
    name: u.name,
    url: u.url,
    prefix: u.prefix ?? '',
    allow: (u.allow ?? []).join(', '),
    kind: u.auth.kind,
    client_id: u.auth.kind === 'oauth' ? (u.auth.client_id ?? '') : '',
    client_secret: '',
    secret_set: u.auth.kind === 'oauth' && !!u.auth.client_secret,
    drop_secret: false,
    scopes: u.auth.kind === 'oauth' ? (u.auth.scopes ?? []).join(' ') : '',
  };
  formOpen.value = true;
}

async function save() {
  const f = form.value;
  const auth: PoolUpstream['auth'] = f.kind === 'oauth' ? oauthAuth(f) : { kind: f.kind };
  const body: PoolUpstream = {
    name: f.name.trim(),
    url: f.url.trim(),
    ...(f.prefix.trim() ? { prefix: f.prefix.trim() } : {}),
    allow: f.allow
      .split(',')
      .map((t) => t.trim())
      .filter(Boolean),
    auth,
  };
  busy.value = true;
  const r = await act<PoolUpstream[]>({ label: `Name ${body.name} for the pool`, path: at(paths.poolUpstreams), body, success: `The pool names ${body.name}` });
  busy.value = false;
  if (r.ok) {
    formOpen.value = false;
    await load();
  }
}

async function remove(u: PoolUpstream) {
  const r = await act({
    label: `Take ${u.name} off the pool's list`,
    method: 'DELETE',
    path: at(paths.poolUpstream(u.name)),
    confirmText: "Members' hubs stop using it and forget their login to it.",
    confirmLabel: 'Take it off',
    success: `The pool no longer names ${u.name}`,
  });
  if (r.ok) await load();
}

function how(u: PoolUpstream): string {
  if (u.auth.kind === 'none') return 'no login';
  if (u.auth.kind === 'token') return 'a token each member gives';
  return u.auth.client_secret ? 'OAuth, with a client secret' : 'OAuth';
}
</script>

<template>
  <section class="card" data-testid="pool-upstreams">
    <header class="card-header">
      <Icon name="link" :size="18" />
      <h2>MCP upstreams</h2>
      <span class="faint small">{{ list ? `${list.length} named` : '' }}</span>
      <button v-if="pool.isAdmin" type="button" class="btn btn-sm add" data-testid="pool-upstream-add" @click="startAdd"><Icon name="plus" :size="14" /> Add</button>
    </header>
    <div class="card-body">
      <p class="muted small">
        MCP servers this pool names for its members. Each member's hub offers them on its <router-link to="/upstreams">MCP upstreams</router-link> page
        (use or log in there) and uses one only once its user accepts it, with that user's own login. No member's credential is kept here, only an
        OAuth app's client secret where a server needs one, never shown. A URL over https only, never a command.
      </p>
      <p v-if="problem" class="err">{{ problem }}</p>

      <form v-if="formOpen" class="form" data-testid="pool-upstream-form" @submit.prevent="save">
        <label>Name <input v-model="form.name" class="input" placeholder="tracker" spellcheck="false" /></label>
        <label>Prefix <input v-model="form.prefix" class="input" :placeholder="form.name.replace(/-/g, '_') || 'tracker'" spellcheck="false" /></label>
        <label class="wide">URL <input v-model="form.url" class="input" placeholder="https://mcp.example.com/mcp" spellcheck="false" /></label>
        <label>Tools <input v-model="form.allow" class="input" placeholder="all; or: search, get_issue" spellcheck="false" /></label>
        <label
          >Login
          <select v-model="form.kind" class="input">
            <option value="oauth">OAuth (each member, in the browser)</option>
            <option value="token">A token each member gives</option>
            <option value="none">None</option>
          </select>
        </label>
        <label v-if="form.kind === 'oauth'" class="wide"
          >Client id <input v-model="form.client_id" class="input" placeholder="only where the server registers none itself" spellcheck="false"
        /></label>
        <label v-if="form.kind === 'oauth' && form.client_id.trim()" class="wide"
          >Client secret
          <input
            v-model="form.client_secret"
            type="password"
            autocomplete="off"
            class="input"
            :placeholder="form.secret_set ? 'set; empty keeps it' : 'only where the server wants one (GitHub); members get it'"
            :disabled="form.drop_secret"
            data-testid="pool-upstream-secret"
        /></label>
        <label v-if="form.kind === 'oauth' && form.secret_set" class="check"><input v-model="form.drop_secret" type="checkbox" /> Remove the client secret</label>
        <label v-if="form.kind === 'oauth'" class="wide">Scopes <input v-model="form.scopes" class="input" placeholder="the server's default; or: repo read:org" spellcheck="false" /></label>
        <div class="row">
          <button type="submit" class="btn btn-primary btn-sm" :disabled="!form.name.trim() || !form.url.trim() || busy">Save</button>
          <button type="button" class="btn btn-sm" @click="formOpen = false">Cancel</button>
        </div>
      </form>

      <p v-if="list && !list.length && !formOpen" class="muted small" data-testid="pool-upstreams-empty">The pool names none.</p>
      <ul v-if="list?.length" class="list">
        <li v-for="u in list" :key="u.name" class="item" data-testid="pool-upstream" :data-name="u.name">
          <div class="main">
            <strong>{{ u.name }}</strong>
            <span class="faint small">{{ u.url }}</span>
            <span class="small">
              <code>{{ u.prefix || u.name.replace(/-/g, '_') }}__…</code> · {{ how(u) }}<template v-if="u.allow?.length"> · {{ u.allow.length }} tool(s)</template>
            </span>
          </div>
          <div v-if="pool.isAdmin" class="actions">
            <button type="button" class="btn btn-ghost btn-sm" @click="startEdit(u)">Change</button>
            <button type="button" class="btn btn-ghost btn-sm" data-testid="pool-upstream-remove" @click="remove(u)">Remove</button>
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
.form label.check {
  grid-column: 1 / -1;
  flex-direction: row;
  align-items: center;
  gap: 6px;
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
