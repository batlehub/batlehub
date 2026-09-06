/*---------------------------------------------------------------------------------------------
 *  Batlehub credential resolution for the extension gallery (RFC 0011 §4.2).
 *
 *  VS Code sends no Authorization header to its gallery and product.json has
 *  nowhere to put a token, so a gallery that requires one answers every query
 *  with an empty list and the editor reports that no extensions were found.
 *  This module is the smallest thing that fixes that in a build you control:
 *  a credential in, a header out.
 *
 *  What it deliberately cannot do, and why the file is this short:
 *
 *   - It never reads the contract file's `refresh` block. Redeeming a refresh
 *     token means an IDP client id, a token endpoint and a POST from the
 *     editor's own process — Batlehub-specific coupling in a patch that wants
 *     to be upstreamable. Refresh belongs to brokers: the CLI, the proxy.
 *   - It understands a literal `token` string and nothing else. A source
 *     object (`{"from": "file", …}`) reads as no credential and falls through
 *     to the environment variable, because the editor should not open a
 *     second file or read another process's environment on a gallery request.
 *   - It never throws. Every failure is "no credential", which is what an
 *     anonymous gallery already looks like. An editor that failed to start
 *     because a JSON file had a stray comma would be a worse bug than the one
 *     this fixes.
 *--------------------------------------------------------------------------------------------*/

import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';

/** The contract's own version. A document from the future is left alone. */
const CONTRACT_VERSION = 1;

/** A credential is a few kilobytes. A larger file is a mistake. */
const MAX_CONTRACT_BYTES = 1024 * 1024;

let warned = false;

/**
 * Say it once. A gallery query runs on every editor start and on every
 * keystroke in the search box; a warning per request is a log nobody reads.
 */
function warnOnce(message: string): void {
	if (!warned) {
		warned = true;
		console.warn(`[batlehub] ${message}`);
	}
}

/** `$BATLEHUB_HOME/state/vsx-token.json`, with `$HOME/.batlehub` as the default. */
export function defaultContractPath(env: NodeJS.ProcessEnv = process.env): string {
	const home = env['BATLEHUB_HOME'] || path.join(os.homedir(), '.batlehub');
	return path.join(home, 'state', 'vsx-token.json');
}

/**
 * The origin an entry is filed under: scheme, host and port, no trailing
 * slash. The gallery URL carries a path (`/proxy/<registry>/vscode/gallery`)
 * and the entry does not, so both sides reduce to this before comparing.
 */
export function originOf(url: string): string | undefined {
	try {
		return new URL(url).origin;
	} catch {
		return undefined;
	}
}

/**
 * Resolve the credential for `galleryUrl`, in the order §4.2 fixes:
 *
 *   1. `VSX_REGISTRY_AUTH_TOKEN_FILE`, if set
 *   2. the default contract path
 *   3. `VSX_REGISTRY_AUTH_TOKEN`
 *
 * First source yielding a credential wins. The environment variable is last
 * because it is global to the process and the file is per-registry: a
 * developer with two Batlehubs open would otherwise send one's token to the
 * other.
 */
export function resolveGalleryToken(
	galleryUrl: string,
	env: NodeJS.ProcessEnv = process.env,
): string | undefined {
	const origin = originOf(galleryUrl);
	if (origin) {
		const explicit = env['VSX_REGISTRY_AUTH_TOKEN_FILE'];
		const fromFile =
			readContract(explicit || defaultContractPath(env), origin);
		if (fromFile) {
			return fromFile;
		}
	}
	const inline = env['VSX_REGISTRY_AUTH_TOKEN'];
	return inline?.trim() || undefined;
}

/**
 * One entry out of the contract file, or nothing.
 *
 * Every failure here — absent, unreadable, not JSON, a version this build
 * does not know, an entry whose token is a source object — is the same
 * answer: no credential. The editor then behaves exactly as it does against
 * an anonymous gallery, which is a state users already understand.
 */
function readContract(file: string, origin: string): string | undefined {
	let raw: string;
	try {
		const stat = fs.statSync(file);
		if (!stat.isFile()) {
			return undefined;
		}
		if (stat.size > MAX_CONTRACT_BYTES) {
			warnOnce(`${file} is ${stat.size} bytes; ignoring it`);
			return undefined;
		}
		raw = fs.readFileSync(file, 'utf8');
	} catch {
		// Absent is the common case — no login yet — and is not worth a line.
		return undefined;
	}

	let doc: any;
	try {
		doc = JSON.parse(raw);
	} catch (e) {
		warnOnce(`${file} is not valid JSON, so no gallery credential was used: ${e}`);
		return undefined;
	}
	if (typeof doc?.version === 'number' && doc.version > CONTRACT_VERSION) {
		warnOnce(`${file} is version ${doc.version} and this build understands ${CONTRACT_VERSION}`);
		return undefined;
	}

	const entry = doc?.registries?.[origin] ?? doc?.registries?.[origin + '/'];
	if (!entry) {
		return undefined;
	}
	if (typeof entry.token !== 'string') {
		// A source object: for a broker to resolve, not the editor. Falling
		// through to the environment variable is the documented behaviour,
		// not a failure.
		warnOnce(
			`${file} describes ${origin}'s credential as a source this build does not resolve; ` +
			`falling back to VSX_REGISTRY_AUTH_TOKEN`,
		);
		return undefined;
	}
	const token = entry.token.trim();
	if (!token) {
		return undefined;
	}
	// An expired entry is still returned. The server is the judge of that,
	// and the 401 retry below is what turns a rotation into one extra
	// request rather than a failed search.
	return token;
}

/**
 * Whether `requestUrl` may carry the gallery credential.
 *
 * Origin equality, not prefix matching: a redirect to a CDN, an asset host,
 * or anything else off the gallery's origin drops the header. A bearer token
 * that follows a redirect is a bearer token sent to whoever controls the
 * redirect.
 */
export function mayAttachCredential(requestUrl: string, galleryUrl: string): boolean {
	const a = originOf(requestUrl);
	const b = originOf(galleryUrl);
	return !!a && !!b && a === b;
}

/**
 * Add the header when the request is going to the gallery's own origin.
 * Returns the headers unchanged otherwise, so a call site can apply this
 * without asking where the request is going.
 */
export function withGalleryCredential(
	headers: Record<string, string>,
	requestUrl: string,
	galleryUrl: string,
	env: NodeJS.ProcessEnv = process.env,
): Record<string, string> {
	if (!mayAttachCredential(requestUrl, galleryUrl)) {
		return headers;
	}
	const token = resolveGalleryToken(galleryUrl, env);
	if (!token) {
		return headers;
	}
	return { ...headers, Authorization: `Bearer ${token}` };
}

/**
 * Run `send`, and on a `401` re-resolve the credential once and run it again.
 *
 * This is what makes short-lived tokens work with no file watcher and no IPC
 * between the editor and whatever refreshes them: the broker rewrites the
 * file, the retry picks it up. Once, not in a loop — a gallery that answers
 * 401 to a fresh credential is misconfigured, and retrying would turn that
 * into a hang.
 */
export async function sendWithCredential(
	requestUrl: string,
	galleryUrl: string,
	headers: Record<string, string>,
	send: (headers: Record<string, string>) => Promise<{ statusCode?: number }>,
	env: NodeJS.ProcessEnv = process.env,
): Promise<{ statusCode?: number }> {
	const firstHeaders = withGalleryCredential(headers, requestUrl, galleryUrl, env);
	const first = await send(firstHeaders);
	if (first.statusCode !== 401 || !mayAttachCredential(requestUrl, galleryUrl)) {
		return first;
	}
	warned = false; // a second reason to complain is worth hearing
	const retryHeaders = withGalleryCredential(headers, requestUrl, galleryUrl, env);
	// Against what was *sent*, not against the base headers — those never
	// carried an Authorization, so comparing with them makes every 401 look
	// like a changed credential and turns "retry once" into "always retry".
	if (retryHeaders['Authorization'] === firstHeaders['Authorization']) {
		// Nothing changed underneath us; a second identical request would
		// only be a second 401.
		return first;
	}
	return send(retryHeaders);
}
