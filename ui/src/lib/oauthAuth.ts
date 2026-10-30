// The OAuth part of an MCP upstream's form (ADR-0069), the user's own or a
// pool's: the client id, a client secret the hub never shows back, scopes.
import type { OauthClientSecret } from '@/api/types';

export type OauthFields = {
  client_id: string;
  /** A new client secret; empty keeps the one set. */
  client_secret: string;
  /** Take the one set away. */
  drop_secret: boolean;
  /** Space- or comma-separated. */
  scopes: string;
};

/** The `auth` an OAuth upstream's form sends. */
export function oauthAuth(f: OauthFields): { kind: 'oauth'; client_id?: string; client_secret?: OauthClientSecret; scopes?: string[] } {
  const id = f.client_id.trim();
  const secret = f.client_secret.trim();
  const scopes = f.scopes.split(/[\s,]+/).filter(Boolean);
  return {
    kind: 'oauth',
    ...(id ? { client_id: id } : {}),
    // Without a client id there is none to keep.
    ...(id && secret && !f.drop_secret ? { client_secret: secret } : f.drop_secret || !id ? { client_secret: '' } : {}),
    ...(scopes.length ? { scopes } : {}),
  };
}
