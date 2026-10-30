import { describe, expect, it } from 'vitest';
import { oauthAuth } from './oauthAuth';

const base = { client_id: '', client_secret: '', drop_secret: false, scopes: '' };

describe('oauthAuth', () => {
  it('sends a new secret, keeps the one set when left empty, and drops it on request', () => {
    expect(oauthAuth({ ...base, client_id: ' app ', client_secret: ' s3cret ' })).toEqual({ kind: 'oauth', client_id: 'app', client_secret: 's3cret' });
    expect(oauthAuth({ ...base, client_id: 'app' })).toEqual({ kind: 'oauth', client_id: 'app' });
    expect(oauthAuth({ ...base, client_id: 'app', client_secret: 'typed', drop_secret: true })).toEqual({ kind: 'oauth', client_id: 'app', client_secret: '' });
  });

  it('takes a secret away with the client id, and splits scopes', () => {
    expect(oauthAuth({ ...base, client_secret: 'orphan', scopes: 'repo, read:org  gist' })).toEqual({
      kind: 'oauth',
      client_secret: '',
      scopes: ['repo', 'read:org', 'gist'],
    });
  });
});
