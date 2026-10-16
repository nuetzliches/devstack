// A fake pool for the mock hub (ADR-0031, ADR-0032): this machine is an admin,
// two colleagues are members, a request waits. `?mock=nopool` is a machine in
// no pool, `?mock=poolmember` one without the admin role.
import type { PoolAuditEntry, PoolMember, PoolNode, PoolOverview, PoolRole } from '../types';

type R = { status: number; body: unknown };

export class MockPool {
  joined: boolean;
  myRole: PoolRole;
  members: PoolMember[];
  nodes: PoolNode[];
  audit: PoolAuditEntry[] = [];
  private me = 'a1b2c3d4e5f60718';

  constructor(q: string) {
    const s = Math.floor(Date.now() / 1000);
    this.joined = !q.includes('nopool');
    this.myRole = q.includes('poolmember') ? 'member' : 'admin';
    this.members = [
      { id: this.me, name: 'dev-pc', state: 'approved', role: this.myRole, requested_at: s - 86_400 * 3, address: '10.0.0.21' },
      { id: '9f8e7d6c5b4a3921', name: 'build-server', state: 'approved', role: this.myRole === 'admin' ? 'member' : 'admin', requested_at: s - 86_400 * 5, address: '10.0.0.14' },
      { id: '1122334455667788', name: 'laptop-anna', state: 'approved', role: 'member', requested_at: s - 86_400, address: '100.64.0.9' },
      { id: '5566778899aabbcc', name: 'laptop-ben', state: 'pending', role: 'member', requested_at: s - 240, address: '100.64.0.12' },
    ];
    this.nodes = [
      { id: this.me, name: 'dev-pc', online: true, seen: 4, report: { sharing: false, cpus: 0, memory: 0 } },
      { id: '9f8e7d6c5b4a3921', name: 'build-server', online: true, seen: 7, report: { sharing: true, cpus: 12, memory: 48 * 2 ** 30 } },
      { id: '1122334455667788', name: 'laptop-anna', online: false, seen: 7200 },
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
