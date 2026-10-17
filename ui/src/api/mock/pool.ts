// A fake pool for the mock hub (ADR-0031, ADR-0032): this machine is an admin,
// two colleagues are members, a request waits. `?mock=nopool` is a machine in
// no pool, `?mock=poolmember` one without the admin role.
import type { PoolAuditEntry, PoolLease, PoolMember, PoolNode, PoolOverview, PoolRole, ShareSettings, ShareStatus } from '../types';

type R = { status: number; body: unknown };

export class MockPool {
  joined: boolean;
  myRole: PoolRole;
  members: PoolMember[];
  nodes: PoolNode[];
  audit: PoolAuditEntry[] = [];
  leases: PoolLease[] = [];
  private me = 'a1b2c3d4e5f60718';
  settings: ShareSettings = { enabled: false };

  /** `GET /hub/pool/share`: a machine with 16 cores and 64 GiB, about a third busy. */
  share(): ShareStatus {
    const free = { cpus: 10.4 + Math.random(), memory: 38 * 2 ** 30 };
    const half = { cpus: free.cpus / 2, memory: free.memory / 2 };
    const s = this.settings;
    const offer = s.enabled
      ? { cpus: Math.min(half.cpus, s.max_cpus ?? Infinity), memory: Math.min(half.memory, s.max_memory ?? Infinity) }
      : { cpus: 0, memory: 0 };
    return { settings: s, total: { cpus: 16, memory: 64 * 2 ** 30 }, free, offer, member: this.joined, reported_at: this.joined ? Date.now() - 4000 : null, error: null };
  }

  setShare(next: ShareSettings): ShareStatus {
    this.settings = next;
    const node = this.nodes.find((n) => n.id === this.me);
    const st = this.share();
    if (node) node.report = { sharing: next.enabled, cpus: st.offer.cpus, memory: st.offer.memory };
    return st;
  }

  constructor(q: string) {
    const s = Math.floor(Date.now() / 1000);
    this.joined = !q.includes('nopool');
    this.myRole = q.includes('poolmember') ? 'member' : 'admin';
    this.members = [
      { id: this.me, name: 'dev-pc', state: 'approved', role: this.myRole, requested_at: s - 86_400 * 3, address: '10.0.0.21', tags: ['team:backend'] },
      { id: '9f8e7d6c5b4a3921', name: 'build-server', state: 'approved', role: this.myRole === 'admin' ? 'member' : 'admin', requested_at: s - 86_400 * 5, address: '10.0.0.14', node: true, policy: { allow: ['team:backend'], repos: [], memory_headroom: 2, network: 'none' } },
      { id: '1122334455667788', name: 'laptop-anna', state: 'approved', role: 'member', requested_at: s - 86_400, address: '100.64.0.9' },
      { id: '5566778899aabbcc', name: 'laptop-ben', state: 'pending', role: 'member', requested_at: s - 240, address: '100.64.0.12' },
    ];
    this.nodes = [
      { id: this.me, name: 'dev-pc', online: true, seen: 4, report: { sharing: false, cpus: 0, memory: 0 }, kind: 'machine' },
      { id: '9f8e7d6c5b4a3921', name: 'build-server', online: true, seen: 7, report: { sharing: true, cpus: 12, memory: 48 * 2 ** 30, api: 'https://10.0.0.14:4261', total: { cpus: 32, memory: 128 * 2 ** 30 }, used: { cpus: 6.5, memory: 22 * 2 ** 30 }, gpus: [{ name: 'NVIDIA L4', memory: 24 * 2 ** 30, memory_used: 3 * 2 ** 30, load: 18, offered: true }] }, kind: 'server' },
      { id: '1122334455667788', name: 'laptop-anna', online: false, seen: 7200, kind: 'machine' },
    ];
    this.leases = [
      { id: '7c1e9a0b22d4f610', node: '9f8e7d6c5b4a3921', member: this.me, repo: 'shop', lane: 'review-cart', state: 'running', created: s - 1800, expires_in: 110 },
      { id: '0d3a5b6c7e8f9012', node: '9f8e7d6c5b4a3921', member: '1122334455667788', repo: 'crm', lane: 'agent-1242', state: 'running', created: s - 600, expires_in: 95 },
    ];
    this.audit = [{ at: s - 86_400, by: `${this.me} (dev-pc)`, action: 'approve', member: '1122334455667788', name: 'laptop-anna', detail: 'member' }];
  }

  private visible(m: PoolMember): PoolMember {
    if (this.myRole === 'admin') return m;
    const { address: _hidden, ...rest } = m;
    return rest;
  }

  overview(): PoolOverview {
    if (!this.joined) return { joined: false };
    const me = { id: this.me, name: 'dev-pc', state: 'approved' as const, role: this.myRole, coordinator: 'https://10.0.0.14:4260' };
    const v: PoolOverview = {
      joined: true,
      me,
      nodes: this.nodes.filter((n) => this.members.some((m) => m.id === n.id && m.state === 'approved')),
      members: this.members.filter((m) => this.myRole === 'admin' || m.state === 'approved').map((m) => this.visible(m)),
      leases: this.leases,
      join: {
        coordinator: me.coordinator,
        fingerprint: '1a93c4d2e5f60718293a4b5c6d7e8f901a2b3c4d5e6f708192a3b4c5d6e7f809',
        command: `devstack join ${me.coordinator} --fingerprint 1a93c4d2e5f60718293a4b5c6d7e8f901a2b3c4d5e6f708192a3b4c5d6e7f809`,
      },
    };
    if (this.myRole === 'admin') v.audit = this.audit;
    return v;
  }

  private log(action: PoolAuditEntry['action'], m: PoolMember, detail?: string) {
    this.audit.unshift({ at: Math.floor(Date.now() / 1000), by: `${this.me} (dev-pc)`, action, member: m.id, name: m.name, detail });
  }

  private lastAdmin(m: PoolMember) {
    return m.role === 'admin' && m.state === 'approved' && this.members.filter((x) => x.role === 'admin' && x.state === 'approved').length === 1;
  }

  /** `POST /hub/pool/nodes/{id}/pause|resume`. */
  pause(id: string, paused: boolean): R {
    if (this.myRole !== 'admin') return { status: 403, body: { error: 'only an admin of the pool may do this' } };
    const n = this.nodes.find((x) => x.id === id);
    const m = this.members.find((x) => x.id === id);
    if (!n || !m) return { status: 404, body: { error: `no member \`${id}\`` } };
    n.paused = paused;
    this.log(paused ? 'pause' : 'resume', m);
    return { status: 200, body: this.visible(m) };
  }

  /** `POST /hub/pool/leases/{id}/end`. */
  end(id: string): R {
    if (this.myRole !== 'admin') return { status: 403, body: { error: 'only an admin of the pool may do this' } };
    const l = this.leases.find((x) => x.id === id);
    const m = l && this.members.find((x) => x.id === l.node);
    if (!l || !m) return { status: 404, body: { error: `no lease \`${id}\`` } };
    this.leases = this.leases.filter((x) => x !== l);
    this.log('end', m, `${l.repo}/${l.lane} (${l.id})`);
    return { status: 202, body: null };
  }

  /** `POST /hub/pool/members/{id}/tags` and `.../nodes/{id}/policy`. */
  tags(id: string, tags: string[]): R {
    const m = this.members.find((x) => x.id === id);
    if (!m) return { status: 404, body: { error: 'no member' } };
    m.tags = tags;
    this.log('tags' as PoolAuditEntry['action'], m, tags.join(', '));
    return { status: 200, body: m };
  }

  /** `POST /hub/pool/members/{id}/node`. */
  grantNode(id: string, node: boolean): R {
    if (this.myRole !== 'admin') return { status: 403, body: { error: 'only an admin of the pool may do this' } };
    const m = this.members.find((x) => x.id === id);
    if (!m) return { status: 404, body: { error: 'no member' } };
    if (node && m.state !== 'approved') return { status: 409, body: { error: `${m.id} (${m.name}) is ${m.state}: only an approved member runs lanes` } };
    if (Boolean(m.node) !== node) this.log('node', m, node ? 'may run lanes' : 'runs no lanes');
    if (node) m.node = true;
    else delete m.node;
    return { status: 200, body: this.visible(m) };
  }

  policy(id: string, p: PoolMember['policy'] | null): R {
    const m = this.members.find((x) => x.id === id);
    if (!m) return { status: 404, body: { error: 'no member' } };
    m.policy = p ?? undefined;
    return { status: 200, body: m };
  }

  /** `POST /hub/pool/join`: the machine asks; it waits for an admin. */
  join(): R {
    this.joined = true;
    this.myRole = 'member';
    const mine = this.members.find((x) => x.id === this.me);
    if (mine) mine.state = 'pending';
    return { status: 200, body: { id: this.me, name: 'dev-pc', state: 'pending' } };
  }

  /** `POST /hub/pool/members/{id}/{action}`. */
  act(id: string, action: string, body: { role?: PoolRole } | undefined): R {
    if (this.myRole !== 'admin') return { status: 403, body: { error: 'only an admin of the pool may do this' } };
    const m = this.members.find((x) => x.id === id);
    if (!m) return { status: 404, body: { error: `no member \`${id}\`` } };
    if (action === 'approve') {
      if (m.state === 'removed') return { status: 409, body: { error: `${m.id} (${m.name}) was removed; it joins again with a new key` } };
      m.state = 'approved';
      m.role = body?.role ?? 'member';
      this.nodes.push({ id: m.id, name: m.name, online: false });
      this.log('approve', m, m.role);
    } else if (action === 'remove') {
      if (this.lastAdmin(m)) return { status: 409, body: { error: "the pool's last admin cannot be removed: make another member admin first" } };
      this.log(m.state === 'pending' ? 'reject' : 'remove', m);
      m.state = 'removed';
    } else if (action === 'role') {
      const role = body?.role;
      if (!role) return { status: 400, body: { error: 'name the role: member or admin' } };
      if (role === 'member' && this.lastAdmin(m)) return { status: 409, body: { error: "the pool's last admin stays one: make another member admin first" } };
      m.role = role;
      if (m.id === this.me) this.myRole = role;
      this.log('role', m, role);
    } else return { status: 404, body: { error: 'no route' } };
    return { status: 200, body: this.visible(m) };
  }
}
