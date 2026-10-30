<script setup lang="ts">
// MCP upstreams (ADR-0069): MCP servers configured once for this user, logged
// in once by the hub, whose tools `devstack mcp` offers in every coding
// agent. Per user, nothing per stack. The page never gets a secret back: a
// token or an OAuth app's client secret goes to the hub's keychain, an OAuth
// login runs in a browser tab and comes back to the hub. A pool may name
// upstreams too (option B): they are listed with their pool and used only
// once the user accepts them.
import { computed, onMounted, onUnmounted, ref } from 'vue';
import { api, paths } from '@/api';
import type { McpUpstream, McpUpstreamAuth, McpUpstreams } from '@/api/types';
import { act } from '@/lib/actions';
import { oauthAuth } from '@/lib/oauthAuth';
import { line, words } from '@/lib/commandLine';
import { useUi } from '@/stores/ui';
import Icon from '@/components/icons/Icon.vue';

const ui = useUi();
const state = ref<McpUpstreams | null>(null);
const problem = ref<string | null>(null);
const busy = ref<Record<string, string>>({});

async function load() {
  try {
    const r = await api().request<McpUpstreams>('GET', paths.upstreams);
    const b = r.body;
    if (r.ok && b && typeof b === 'object' && Array.isArray(b.upstreams)) {
      state.value = b;
      problem.value = null;
    } else problem.value = (b && typeof b === 'object' && 'error' in b ? String((b as { error: unknown }).error) : null) ?? `The hub answered ${r.status}.`;
  } catch {
    problem.value = 'The hub does not answer.';
  }
}
onMounted(load);

// The form: a new upstream, or one to change.
type Form = {
  name: string;
  /** A URL (Streamable HTTP), or a command the hub starts (stdio). */
  transport: 'url' | 'command';
  url: string;
  command: string;
  /** `KEY=value`, one per line: what the command gets besides a base. */
  vars: string;
  prefix: string;
  allow: string;
  kind: McpUpstreamAuth['kind'];
  client_id: string;
  /** A new client secret; empty keeps the one set. */
  client_secret: string;
  /** The upstream has one (the hub never shows it). */
  secret_set: boolean;
  drop_secret: boolean;
  scopes: string;
  env: string;
  token_var: string;
};
const empty = (): Form => ({
  name: '',
  transport: 'url',
  url: '',
  command: '',
  vars: '',
  prefix: '',
  allow: '',
  kind: 'oauth',
  client_id: '',
  client_secret: '',
  secret_set: false,
  drop_secret: false,
  scopes: '',
  env: '',
  token_var: '',
});
const form = ref<Form>(empty());
const editing = ref<string | null>(null);
const formOpen = ref(false);


function startAdd() {
  form.value = empty();
  editing.value = null;
  formOpen.value = true;
}

function startEdit(u: McpUpstream) {
  const isCommand = (u.command ?? []).length > 0;
  form.value = {
    name: u.name,
    transport: isCommand ? 'command' : 'url',
    url: u.url ?? '',
    command: line(u.command ?? []),
    vars: Object.entries(u.env ?? {})
      .map(([k, v]) => `${k}=${v}`)
      .join('\n'),
    prefix: u.prefix === u.name.replace(/-/g, '_') ? '' : u.prefix,
    allow: (u.allow ?? []).join(', '),
    kind: u.auth.kind,
    client_id: u.auth.kind === 'oauth' ? (u.auth.client_id ?? '') : '',
    client_secret: '',
    secret_set: u.auth.kind === 'oauth' && !!u.auth.client_secret,
    drop_secret: false,
    scopes: u.auth.kind === 'oauth' ? (u.auth.scopes ?? []).join(' ') : '',
    env: u.auth.kind === 'token' ? (u.auth.env ?? '') : '',
    token_var: u.token_var ?? '',
  };
  editing.value = u.name;
  formOpen.value = true;
}

const auth = computed<McpUpstreamAuth>(() => {
  const f = form.value;
  if (f.kind === 'oauth') return oauthAuth(f);
  if (f.kind === 'token') return { kind: 'token', ...(f.env.trim() ? { env: f.env.trim() } : {}) };
  return { kind: 'none' };
});

async function save() {
  const f = form.value;
  const isCommand = f.transport === 'command';
  const vars = Object.fromEntries(
    f.vars
      .split('\n')
      .map((l) => l.trim())
      .filter((l) => l.includes('='))
      .map((l) => [l.slice(0, l.indexOf('=')).trim(), l.slice(l.indexOf('=') + 1)]),
  );
  const body = {
    name: f.name.trim(),
    ...(isCommand ? { command: words(f.command.trim()), env: vars } : { url: f.url.trim() }),
    ...(isCommand && f.kind === 'token' && !f.env.trim() && f.token_var.trim() ? { token_var: f.token_var.trim() } : {}),
    ...(f.prefix.trim() ? { prefix: f.prefix.trim() } : {}),
    allow: f.allow
      .split(',')
      .map((t) => t.trim())
      .filter(Boolean),
    auth: auth.value,
  };
  busy.value.form = 'save';
  const r = await act({ label: editing.value ? `Change ${body.name}` : `Add ${body.name}`, path: paths.upstreams, body, success: `${body.name} saved` });
  delete busy.value.form;
  if (r.ok) {
    formOpen.value = false;
    await load();
  }
}

async function remove(u: McpUpstream) {
  const ok = await ui.confirm({
    title: `Remove ${u.name}`,
    text: 'Its configuration and its login go; coding agents no longer see its tools.',
    confirmLabel: 'Remove',
    danger: true,
  });
  if (!ok) return;
  const r = await act({ label: `Remove ${u.name}`, method: 'DELETE', path: paths.upstream(u.name), success: `${u.name} removed` });
  if (r.ok) await load();
}

async function login(u: McpUpstream) {
  // Opened before the request, as a click: a tab opened after an await is
  // blocked as a pop-up.
  const tab = window.open('', '_blank');
  busy.value[u.name] = 'login';
  const r = await act<{ url: string }>({ label: `Log in to ${u.name}`, path: paths.upstreamLogin(u.name), success: false });
  delete busy.value[u.name];
  if (r.ok && r.body?.url) {
    if (tab) tab.location.href = r.body.url;
    else window.open(r.body.url, '_blank');
    ui.toast('info', `Log in to ${u.name} in the new tab`, 'This list follows once the login is done.');
    watchLogin(u.name);
  } else tab?.close();
}

// A login ends in another tab, at the hub: the list asks the hub again until
// the upstream is logged in, and at once when the user comes back here.
const pending = new Set<string>();
let timer: ReturnType<typeof setInterval> | null = null;
let since = 0;

function watchLogin(name: string) {
  pending.add(name);
  since = Date.now();
  if (!timer) timer = setInterval(followLogins, 2000);
}

async function followLogins() {
  await load();
  for (const name of pending) {
    if (state.value?.upstreams.find((u) => u.name === name)?.logged_in) pending.delete(name);
  }
  // A login nobody finishes is not asked about for ever.
  if (pending.size === 0 || Date.now() - since > 10 * 60_000) stopWatching();
}

function stopWatching() {
  pending.clear();
  if (timer) clearInterval(timer);
  timer = null;
}

function onReturn() {
  if (pending.size > 0 && document.visibilityState === 'visible') void followLogins();
}

onMounted(() => {
  window.addEventListener('focus', onReturn);
  document.addEventListener('visibilitychange', onReturn);
});
onUnmounted(() => {
  window.removeEventListener('focus', onReturn);
  document.removeEventListener('visibilitychange', onReturn);
  stopWatching();
});

const tokenFor = ref<string | null>(null);
const tokenValue = ref('');
async function saveToken(u: McpUpstream) {
  const r = await act({ label: `Token for ${u.name}`, path: paths.upstreamToken(u.name), body: { token: tokenValue.value }, success: `Token for ${u.name} kept in the keychain` });
  tokenValue.value = '';
  if (r.ok) {
    tokenFor.value = null;
    await load();
  }
}

async function logout(u: McpUpstream) {
  const r = await act({ label: `Log out of ${u.name}`, path: paths.upstreamLogout(u.name), success: `Logged out of ${u.name}` });
  if (r.ok) await load();
}

/** A prefix of the user's for a pool's upstream whose own one is taken. */
const prefixFor = ref<Record<string, string>>({});

async function accept(u: McpUpstream) {
  const p = u.pool!;
  const prefix = prefixFor.value[u.name]?.trim();
  busy.value[u.name] = 'accept';
  const r = await act({ label: `Use ${p.name} of pool ${p.pool}`, path: paths.upstreamAccept(p.pool, p.name), body: prefix ? { prefix } : {}, success: `${p.name} is used now` });
  delete busy.value[u.name];
  if (r.ok) await load();
}

async function decline(u: McpUpstream) {
  const p = u.pool!;
  const r = await act({ label: `Do not use ${p.name} of pool ${p.pool}`, path: paths.upstreamDecline(p.pool, p.name), success: `${p.name} is not used` });
  if (r.ok) await load();
}

const statusText: Record<string, string> = {
  offered: 'offered, not used yet',
  declined: 'declined',
  changed: 'its URL changed: not used until you accept it again',
  conflict: 'not used',
};

function how(u: McpUpstream): string {
  if (u.auth.kind === 'none') return 'no login';
  if (u.auth.kind === 'token') return u.auth.env ? `token from $${u.auth.env}` : 'token';
  return u.auth.client_secret ? 'OAuth, with a client secret' : 'OAuth';
}
</script>

<template>
  <div class="page upstreams" data-testid="upstreams">
    <section class="card">
      <header class="card-header">
        <h2>MCP upstreams</h2>
        <button type="button" class="btn btn-primary btn-sm add" data-testid="upstream-add" @click="startAdd"><Icon name="plus" :size="14" /> Add</button>
      </header>
      <div class="card-body">
        <p class="muted">
          MCP servers you use in every coding agent. The hub logs in to each once and keeps the login in your keychain; <code>devstack mcp</code> offers their tools as
          <code>&lt;prefix&gt;__&lt;tool&gt;</code> in every agent that has it. They are yours, not a stack's. A pool can name some for its
          members: they are listed here and used once you accept them, with your own login.
        </p>
        <p v-if="state?.store === 'file'" class="warn-text small" data-testid="upstreams-file-store">
          This machine has no keychain (no Secret Service): logins are kept in a file only you can read, beside the hub's state.
        </p>
        <p v-if="problem" class="err">{{ problem }}</p>

        <form v-if="formOpen" class="form" data-testid="upstream-form" @submit.prevent="save">
          <label>Name <input v-model="form.name" class="input" :disabled="!!editing" placeholder="tracker" spellcheck="false" /></label>
          <label
            >Reached
            <select v-model="form.transport" class="input" data-testid="upstream-transport">
              <option value="url">at a URL</option>
              <option value="command">as a command the hub starts</option>
            </select>
          </label>
          <label v-if="form.transport === 'url'" class="wide">URL <input v-model="form.url" class="input" placeholder="https://mcp.example.com/mcp" spellcheck="false" /></label>
          <template v-else>
            <label class="wide"
              >Command <input v-model="form.command" class="input" placeholder="npx -y @example/mcp-server" spellcheck="false" />
              <span class="faint small">Started as you, with a base environment and only the variables below; never from a repository.</span>
            </label>
            <label class="wide">Variables <textarea v-model="form.vars" class="input" rows="2" placeholder="KEY=value, one per line; no secret here" spellcheck="false" /></label>
          </template>
          <label>Prefix <input v-model="form.prefix" class="input" :placeholder="form.name.replace(/-/g, '_') || 'tracker'" spellcheck="false" /></label>
          <label>Tools <input v-model="form.allow" class="input" placeholder="all; or: search, get_issue" spellcheck="false" /></label>
          <label
            >Login
            <select v-model="form.kind" class="input">
              <option v-if="form.transport === 'url'" value="oauth">OAuth (log in in the browser)</option>
              <option value="token">A token</option>
              <option value="none">None</option>
            </select>
          </label>
          <label v-if="form.kind === 'oauth'">Client id <input v-model="form.client_id" class="input" placeholder="only where the server registers none itself" spellcheck="false" /></label>
          <label v-if="form.kind === 'oauth' && form.client_id.trim()"
            >Client secret
            <input
              v-model="form.client_secret"
              type="password"
              autocomplete="off"
              class="input"
              :placeholder="form.secret_set ? 'set; empty keeps it' : 'only where the server wants one (GitHub)'"
              :disabled="form.drop_secret"
              data-testid="upstream-secret"
          /></label>
          <label v-if="form.kind === 'oauth' && form.secret_set" class="check"><input v-model="form.drop_secret" type="checkbox" /> Remove the client secret</label>
          <label v-if="form.kind === 'oauth'">Scopes <input v-model="form.scopes" class="input" placeholder="the server's default; or: repo read:org" spellcheck="false" /></label>
          <label v-if="form.kind === 'token'">From variable <input v-model="form.env" class="input" placeholder="empty: you give it the hub" spellcheck="false" /></label>
          <label v-if="form.kind === 'token' && form.transport === 'command' && !form.env.trim()"
            >Given to it as <input v-model="form.token_var" class="input" placeholder="API_TOKEN" spellcheck="false"
          /></label>
          <div class="row">
            <button
              type="submit"
              class="btn btn-primary btn-sm"
              :disabled="!form.name.trim() || !(form.transport === 'url' ? form.url.trim() : form.command.trim()) || (form.transport === 'command' && form.kind === 'oauth') || !!busy.form"
            >
              Save
            </button>
            <button type="button" class="btn btn-sm" @click="formOpen = false">Cancel</button>
          </div>
        </form>

        <p v-if="state && !state.upstreams.length && !formOpen" class="muted" data-testid="upstreams-empty">None yet.</p>
        <ul v-if="state?.upstreams.length" class="list">
          <li v-for="u in state.upstreams" :key="u.name" class="item" data-testid="upstream" :data-name="u.name">
            <div class="main">
              <strong
                >{{ u.pool ? u.pool.name : u.name }}
                <span v-if="u.pool" class="pill sm outline" data-testid="upstream-pool">from pool {{ u.pool.pool }}</span></strong
              >
              <span class="faint small">{{ u.command?.length ? line(u.command) : u.url }}</span>
              <span class="small">
                <code>{{ u.prefix }}__…</code> · {{ how(u) }} ·
                <span v-if="!u.pool || u.pool.status === 'accepted'" :class="u.logged_in ? 'ok-text' : 'warn-text'">{{ u.logged_in ? 'logged in' : 'logged out' }}</span>
                <span v-else :class="u.pool.status === 'declined' ? 'faint' : 'warn-text'" data-testid="upstream-status">{{ statusText[u.pool.status] }}</span>
                <template v-if="u.allow?.length"> · {{ u.allow.length }} tool(s)</template>
              </span>
              <span v-if="u.pool?.why" class="faint small">{{ u.pool.why }}</span>
            </div>
            <div v-if="u.pool && u.pool.status !== 'accepted'" class="actions">
              <input
                v-if="u.pool.status === 'conflict'"
                v-model="prefixFor[u.name]"
                class="input prefix"
                placeholder="another prefix"
                spellcheck="false"
                data-testid="upstream-prefix"
              />
              <button type="button" class="btn btn-primary btn-sm" :disabled="!!busy[u.name]" data-testid="upstream-accept" @click="accept(u)">
                {{ u.pool.status === 'changed' ? 'Use the new URL' : 'Use' }}
              </button>
              <button v-if="u.pool.status !== 'declined'" type="button" class="btn btn-ghost btn-sm" data-testid="upstream-decline" @click="decline(u)">Decline</button>
            </div>
            <div v-else class="actions">
              <button v-if="u.auth.kind === 'oauth'" type="button" class="btn btn-sm" :disabled="!!busy[u.name]" data-testid="upstream-login" @click="login(u)">
                <span v-if="busy[u.name]" class="spinner" /><Icon v-else name="key" :size="14" /> {{ u.logged_in ? 'Log in again' : 'Log in' }}
              </button>
              <button v-if="u.auth.kind === 'token' && !u.auth.env" type="button" class="btn btn-sm" data-testid="upstream-token" @click="tokenFor = tokenFor === u.name ? null : u.name">
                <Icon name="key" :size="14" /> {{ u.logged_in ? 'New token' : 'Give token' }}
              </button>
              <button v-if="u.logged_in && u.auth.kind !== 'none' && !(u.auth.kind === 'token' && u.auth.env)" type="button" class="btn btn-sm" @click="logout(u)">Log out</button>
              <button v-if="u.pool" type="button" class="btn btn-ghost btn-sm" data-testid="upstream-decline" @click="decline(u)">Stop using</button>
              <template v-else>
                <button type="button" class="btn btn-ghost btn-sm" @click="startEdit(u)">Change</button>
                <button type="button" class="btn btn-ghost btn-sm" @click="remove(u)">Remove</button>
              </template>
            </div>
            <form v-if="tokenFor === u.name" class="token" @submit.prevent="saveToken(u)">
              <input v-model="tokenValue" class="input grow" type="password" autocomplete="off" placeholder="the token; kept in your keychain, never shown again" />
              <button type="submit" class="btn btn-primary btn-sm" :disabled="!tokenValue.trim()">Keep</button>
            </form>
          </li>
        </ul>
      </div>
    </section>
  </div>
</template>

<style scoped>
.upstreams {
  max-width: 900px;
}
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
  flex-wrap: wrap;
}
.ok-text {
  color: var(--ok-text);
}
.warn-text {
  color: var(--warn-text);
}
.prefix {
  width: 140px;
}
.token {
  display: flex;
  gap: 8px;
  width: 100%;
}
</style>
