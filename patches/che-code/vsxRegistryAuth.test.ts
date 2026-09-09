/*---------------------------------------------------------------------------------------------
 *  RFC 0011 §10, the patch's half: resolution order, origin scoping including
 *  the redirect drop, the single 401 retry, an unparseable file treated as no
 *  credential, and a token *source* object falling through to the environment
 *  variable.
 *
 *  Run with `node --test patches/che-code/` — Node strips the types itself, so
 *  the module that goes into an editor build is tested here with no toolchain
 *  of its own. The point is that this file travels with the module: whoever
 *  rebases it onto a new upstream can tell in one command whether it still
 *  does what the contract says.
 *--------------------------------------------------------------------------------------------*/

import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

import {
	defaultContractPath,
	mayAttachCredential,
	originOf,
	resolveGalleryToken,
	sendWithCredential,
	withGalleryCredential,
} from './vsxRegistryAuth.ts';

const GALLERY = 'https://hub.example.dev/proxy/vsx/vscode/gallery';
const ORIGIN = 'https://hub.example.dev';

/** A throwaway `$BATLEHUB_HOME` holding `contents` as the contract file. */
function homeWith(contents: string | undefined): { home: string; env: NodeJS.ProcessEnv } {
	const home = fs.mkdtempSync(path.join(os.tmpdir(), 'vsxauth-'));
	if (contents !== undefined) {
		const file = path.join(home, 'state', 'vsx-token.json');
		fs.mkdirSync(path.dirname(file), { recursive: true });
		fs.writeFileSync(file, contents);
	}
	// A bare env: the real one may have either variable set, and a test that
	// passed because the developer had logged in would be worthless.
	return { home, env: { BATLEHUB_HOME: home } };
}

function contract(entry: unknown, origin = ORIGIN): string {
	return JSON.stringify({ version: 1, registries: { [origin]: entry } });
}

// ── where it looks ───────────────────────────────────────────────────────────

test('the default path is $BATLEHUB_HOME/state/vsx-token.json', () => {
	assert.equal(
		defaultContractPath({ BATLEHUB_HOME: '/somewhere' }),
		path.join('/somewhere', 'state', 'vsx-token.json'),
	);
	// And with no BATLEHUB_HOME it is under the user's home, not the cwd.
	assert.ok(defaultContractPath({}).startsWith(os.homedir()));
});

test('resolution order: the file wins over the environment variable', () => {
	const { env } = homeWith(contract({ token: 'from-the-file', kind: 'oidc' }));
	env['VSX_REGISTRY_AUTH_TOKEN'] = 'from-the-env';
	assert.equal(resolveGalleryToken(GALLERY, env), 'from-the-file');
});

test('the environment variable is the fallback, not the default', () => {
	const { env } = homeWith(undefined);
	env['VSX_REGISTRY_AUTH_TOKEN'] = 'from-the-env';
	assert.equal(resolveGalleryToken(GALLERY, env), 'from-the-env');

	// It is last because it is global to the process while the file is
	// per-registry: a developer with two BatleHubs open would otherwise send
	// one's token to the other.
	const two = homeWith(contract({ token: 'for-this-one', kind: 'oidc' }));
	two.env['VSX_REGISTRY_AUTH_TOKEN'] = 'for-anything';
	assert.equal(
		resolveGalleryToken('https://hub.other.dev/vscode/gallery', two.env),
		'for-anything',
		'the other origin has no entry, so the global variable answers',
	);
	assert.equal(resolveGalleryToken(GALLERY, two.env), 'for-this-one');
});

test('VSX_REGISTRY_AUTH_TOKEN_FILE overrides the default path', () => {
	const { home, env } = homeWith(contract({ token: 'default-path', kind: 'oidc' }));
	const elsewhere = path.join(home, 'other.json');
	fs.writeFileSync(elsewhere, contract({ token: 'explicit-path', kind: 'oidc' }));
	env['VSX_REGISTRY_AUTH_TOKEN_FILE'] = elsewhere;
	assert.equal(resolveGalleryToken(GALLERY, env), 'explicit-path');
});

test('an entry filed with a trailing slash is still found', () => {
	const { env } = homeWith(contract({ token: 'slashed', kind: 'oidc' }, ORIGIN + '/'));
	assert.equal(resolveGalleryToken(GALLERY, env), 'slashed');
});

// ── what it refuses to read ──────────────────────────────────────────────────

test('an unparseable file is no credential, never a throw', () => {
	const { env } = homeWith('{ not json at all');
	env['VSX_REGISTRY_AUTH_TOKEN'] = 'fallback';
	// It must never break extension installs for an anonymous gallery, which
	// is why every failure here is the same answer rather than an exception.
	assert.equal(resolveGalleryToken(GALLERY, env), 'fallback');
});

test('a token source object yields no credential and falls through', () => {
	const { env } = homeWith(
		contract({ token: { from: 'file', path: '/var/run/secrets/token' }, kind: 'kubernetes' }),
	);
	env['VSX_REGISTRY_AUTH_TOKEN'] = 'fallback';
	// Sources are for brokers. The editor should not open a second file on a
	// gallery request, and the documented behaviour is this fall-through
	// rather than a failure.
	assert.equal(resolveGalleryToken(GALLERY, env), 'fallback');
});

test('a document from the future is left alone', () => {
	const { env } = homeWith(
		JSON.stringify({ version: 99, registries: { [ORIGIN]: { token: 'x', kind: 'oidc' } } }),
	);
	assert.equal(resolveGalleryToken(GALLERY, env), undefined);
});

test('an absent file, an absent entry and an empty token are all no credential', () => {
	assert.equal(resolveGalleryToken(GALLERY, homeWith(undefined).env), undefined);
	assert.equal(
		resolveGalleryToken(GALLERY, homeWith(contract({ token: 'x', kind: 'oidc' }, 'https://elsewhere')).env),
		undefined,
	);
	assert.equal(
		resolveGalleryToken(GALLERY, homeWith(contract({ token: '   ', kind: 'oidc' })).env),
		undefined,
	);
});

test('an expired entry is still sent; the server is the judge of that', () => {
	const { env } = homeWith(
		contract({ token: 'stale', kind: 'oidc', expires_at: '2020-01-01T00:00:00Z' }),
	);
	assert.equal(resolveGalleryToken(GALLERY, env), 'stale');
});

// ── where it will send it ────────────────────────────────────────────────────

test('origin scoping: the header goes to the gallery origin and nowhere else', () => {
	assert.equal(originOf(GALLERY), ORIGIN);
	assert.ok(mayAttachCredential(ORIGIN + '/proxy/vsx/asset/x', GALLERY));
	// A redirect to a CDN, an asset host, or anything else: a bearer token
	// that follows a redirect is a bearer token sent to whoever controls it.
	assert.ok(!mayAttachCredential('https://cdn.example.net/file.vsix', GALLERY));
	// Scheme and port are part of an origin, so neither downgrades silently.
	assert.ok(!mayAttachCredential('http://hub.example.dev/x', GALLERY));
	assert.ok(!mayAttachCredential('https://hub.example.dev:8443/x', GALLERY));
	assert.ok(!mayAttachCredential('not a url', GALLERY));
});

test('withGalleryCredential adds the header only where it may', () => {
	const { env } = homeWith(contract({ token: 'abc', kind: 'oidc' }));
	const base = { Accept: 'application/json' };

	const onGallery = withGalleryCredential(base, ORIGIN + '/proxy/vsx/x', GALLERY, env);
	assert.equal(onGallery['Authorization'], 'Bearer abc');
	assert.equal(onGallery['Accept'], 'application/json', 'it adds, it does not replace');

	const offGallery = withGalleryCredential(base, 'https://cdn.example.net/f.vsix', GALLERY, env);
	assert.deepEqual(offGallery, base);

	// No credential is not an empty bearer.
	const none = withGalleryCredential(base, ORIGIN + '/x', GALLERY, homeWith(undefined).env);
	assert.equal(none['Authorization'], undefined);
});

// ── the retry ────────────────────────────────────────────────────────────────

test('a 401 re-reads the credential once and retries once', async () => {
	const { home, env } = homeWith(contract({ token: 'stale', kind: 'oidc' }));
	const file = path.join(home, 'state', 'vsx-token.json');

	const sent: (string | undefined)[] = [];
	const res = await sendWithCredential(
		ORIGIN + '/proxy/vsx/x',
		GALLERY,
		{},
		async headers => {
			sent.push(headers['Authorization']);
			// Whatever refreshes the file has rewritten it by the time the
			// first request comes back. This is the whole mechanism: no file
			// watcher, no IPC between the editor and the broker.
			fs.writeFileSync(file, contract({ token: 'fresh', kind: 'oidc' }));
			return { statusCode: sent.length === 1 ? 401 : 200 };
		},
		env,
	);

	assert.equal(res.statusCode, 200);
	assert.deepEqual(sent, ['Bearer stale', 'Bearer fresh']);
});

test('a 401 with an unchanged credential is not retried', async () => {
	const { env } = homeWith(contract({ token: 'same', kind: 'oidc' }));
	let calls = 0;
	const res = await sendWithCredential(
		ORIGIN + '/x',
		GALLERY,
		{},
		async () => {
			calls++;
			return { statusCode: 401 };
		},
		env,
	);
	// Nothing changed underneath us, so a second identical request would only
	// be a second 401 — and a gallery that answers 401 to a fresh credential
	// is misconfigured, not slow.
	assert.equal(calls, 1);
	assert.equal(res.statusCode, 401);
});

test('a 401 from a foreign origin is not retried either', async () => {
	const { env } = homeWith(contract({ token: 'abc', kind: 'oidc' }));
	let calls = 0;
	await sendWithCredential(
		'https://cdn.example.net/f.vsix',
		GALLERY,
		{},
		async headers => {
			calls++;
			assert.equal(headers['Authorization'], undefined, 'never off-origin');
			return { statusCode: 401 };
		},
		env,
	);
	assert.equal(calls, 1);
});

test('a non-401 answer is returned as it came', async () => {
	const { env } = homeWith(contract({ token: 'abc', kind: 'oidc' }));
	let calls = 0;
	const res = await sendWithCredential(ORIGIN + '/x', GALLERY, {}, async () => {
		calls++;
		return { statusCode: 403 };
	}, env);
	assert.equal(calls, 1);
	assert.equal(res.statusCode, 403);
});
