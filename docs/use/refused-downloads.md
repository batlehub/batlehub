---
reference: true
---

# Why a download was refused

When BatleHub refuses a download, it tells your package manager why, but most
package managers print only part of that answer. This page explains what each
refusal means, how to get the full reason with `batlehub-cli why`, and what you, or
the administrator of the instance, can do about each one. How these checks are
configured is on the operator's page, [Security gates](/guide/security-gates).

---

## What you see {#what-you-see}

A refusal comes back in one of three shapes.

**A `403` with a reason in it.** The registry runs its checks as the request
happens. The message names the check:

```text
npm error 403 Forbidden - GET https://hub.example.com/proxy/npm/left-pad/-/left-pad-1.3.1.tgz
npm error 403 release is too recent (age: 3600s, minimum: 259200s, 255600 seconds remaining in quarantine)
```

The messages are listed under [Refusals with a plain message](#plain-messages).

**A `403` naming a verdict.** The registry has a supply-chain profile: every
version is scanned, and the scan's result, its **verdict**, decides:

```text
npm-public:left-pad@1.3.1 is quarantined (MIN_AGE_NOT_MET, available 2026-09-05T14:12:00Z). Run `batlehub-cli why npm-public:left-pad@1.3.1` for details.
```

The uppercase words are **reason codes**, each explained under
[Reason codes](#codes). The same information is in the response headers
`X-BatleHub-Verdict`, `X-BatleHub-Reason` and `X-BatleHub-Available-At`, which
`curl -I` shows even when your package manager does not.

**A `404`, for a version you know exists.** The version is held or refused by
a verdict, and your token is not allowed to see verdicts: that needs the
`quarantine:read` permission, which signed-in users have by default and
anonymous callers do not. Sign in, or use a token, and retry, and the `404`
becomes the `403` above. `goproxy` and `maven` registries answer `403` instead
of `404` in this case, because their clients treat a `404` as "try the next
source".

---

## Ask for the full reason: `batlehub-cli why` {#batlehub-why}

```console
$ batlehub-cli why npm-public:left-pad@1.3.1
npm-public:left-pad@1.3.1
  state        quarantined
  reasons      MIN_AGE_NOT_MET
  docs         https://batlehub.dev/use/refused-downloads#min-age-not-met
  available    2026-09-05T14:12:00Z
  policy       npm-public/default
  evaluated    2026-09-02T14:12:03Z
  scanned      2026-09-02T14:12:01Z
  scanners     osv
  findings     none

Held until 2026-09-05T14:12:00Z. `batlehub-cli wait` waits for it.
```

- **`docs`:** one link per reason code, to its section of this page.
- **`findings`:** the scanners' own detail, shown when your token holds
  `findings:read` (administrators, by default). Otherwise it reads `withheld`.
- **On the console:** the package page shows the same verdict for the version
  you select, and each reason code links here.
- **`--rescan`:** also queues a new scan. It needs `gates:exempt`, which
  nobody holds by default, so it is usually your administrator who runs it.

The command is described in full in [the CLI](/use/cli#commands-security).

**Wait for a hold to lift** in a script or a pipeline:

```console
$ batlehub-cli wait npm-public:left-pad@1.3.1 --timeout 2h
```

It exits `0` once the version can be served, `1` when waiting cannot help (the
version is refused, or held with no end date, as `TIMESTAMP_MISSING` is), and
`2` when the timeout passes first.

---

## The four states {#states}

| State | Served? | What happens next |
| --- | --- | --- |
| `allowed` | yes | Nothing. Every required scanner answered and found nothing serious enough |
| `warned` | yes | Nothing. The download carries the reasons in its headers, and your package manager installs it |
| `quarantined` | no | **It lifts on its own** — at the time in `available`, or when a scanner answers |
| `denied` | no | **It stays refused** until a new scan clears it or an administrator acts |

---

## Reason codes {#codes}

### Held for now {#held}

These hold a version for a while. Waiting is usually the answer.

#### `MIN_AGE_NOT_MET` {#min-age-not-met}

The version was published too recently. The registry waits a set time after
every release (three days is common) so that a compromised release can be
noticed and pulled before anyone installs it. The verdict carries the time it
becomes available: wait for it with `batlehub-cli wait`, or pin the previous
version until then.

#### `SCAN_PENDING` {#scan-pending}

A scanner the registry requires has not answered yet. This is normal on the
first request for a version: that request is what queued the scan. Retry in a
minute or two. If it lasts, the scan worker may be down: tell your
administrator.

#### `SCANNER_ERROR` {#scanner-error}

A required scanner failed on this version, after its retries. The hold lifts
when a scan succeeds, and your administrator can queue one with
`batlehub-cli why --rescan`. A version older than
the registry's maturity age is served `warned` despite this code, so a broken
scanner does not block everything forever.

#### `SCANNER_UNSUPPORTED` {#scanner-unsupported}

A required scanner cannot read this kind of artifact, for instance an archive
scanner on a package made of several files. Whether that holds the version is
the registry's choice. If it does, your administrator has to change the
registry's required scanners: waiting will not help.

#### `TIMESTAMP_MISSING` {#timestamp-missing}

The upstream did not say when the version was published, so the registry
cannot tell whether it is old enough. The hold has no end date: it lifts if the
upstream starts dating the version. Otherwise this needs your administrator,
who can either exempt the version or stop holding undated versions on this
registry.

### Refused because of what a scan found {#found}

A scanner found something at or above the registry's severity threshold. On a
registry in `warn` mode these produce `warned` instead, and the version is
served.

#### `VULNERABILITY` {#vulnerability}

A known vulnerability affects this version. `batlehub-cli why` names the advisory
when you can see findings. Upgrade to a fixed version if there is one. If the
vulnerability cannot affect how you use the package, your administrator can
record that judgement as an exemption, with a reason and an end date.

The advisory may be **unrated**: some databases, the Go vulnerability database
first among them, publish advisories with no severity. The registry then
judges it at its `unknown_severity` level, `high` unless your administrator
changed it, so an unrated advisory is refused like a high one.
`batlehub-cli why` shows it as severity `unknown`, judged as `high` by
`unknown_severity`. See [Unrated findings](/guide/security-gates#unknown-severity).

#### `MALWARE_SIGNAL` {#malware-signal}

A scanner found code that behaves like malware: obfuscated execution, a network
call to a known-bad address, credentials read and sent somewhere, or a match
on one of your organisation's own rules. **Do not work around this one.** Tell
your administrator or security team, and use a version that does not carry it.

#### `KNOWN_MALICIOUS` {#known-malicious}

An advisory database lists this version as malicious — an OSV `MAL-` record:
a package published to steal credentials, mine, or open a back door. This is
always refused, whatever the registry's mode or severity threshold. **Do not
work around this one.** Remove the package from your project, and tell your
security team if it was ever installed: what it ran at install time already
happened. If the listing is wrong, only an administrator can serve this exact
version, with an exemption on the gate `security_verdict`
(`batlehub-cli exemption add … --gate security_verdict`). It is then served as
`warned`, with `ADMIN_OVERRIDE` in its reasons.

#### `INSTALL_HOOK` {#install-hook}

The package runs a script when it is installed (an npm `preinstall`, a Python
`setup.py`). Many legitimate packages do, and the registry decides whether
that refuses, warns or is ignored. If you need this package, ask your
administrator to review it.

#### `TYPOSQUAT_SUSPECT` {#typosquat-suspect}

The name is suspiciously close to a popular package's. Check the spelling of
what you asked for: this is the classic way malware gets installed by mistake.

#### `INSTALL_HOOK_ADDED` {#install-hook-added}

This version adds an install script that the previous version did not have.
That is a common sign of a hijacked package. Prefer the previous version until
someone has looked at the new script.

#### `PUBLISHER_CHANGED` {#publisher-changed}

This version was published by a different account than the ones before it.
That can be an ordinary change of maintainer, or a takeover. Prefer the
previous version until the change is explained.

#### `REPOSITORY_MOVED` {#repository-moved}

The package's declared source repository changed in this version. Like a new
publisher, it is ordinary as often as it is not. It is worth a look before you
trust the version.

#### `DORMANT_RELEASE` {#dormant-release}

A new version appeared after a long silence. Abandoned packages that suddenly
release are a known takeover pattern. Check the release before you adopt it.

#### `UNPUBLISHED_UPSTREAM` {#unpublished-upstream}

The upstream registry no longer lists this version: it was unpublished or
removed, often because it was bad. Move to a version the upstream still
publishes.

### Signatures and provenance {#provenance}

#### `PROVENANCE_MISSING` {#provenance-missing}

The version has no provenance attestation, the signed record of which build
produced it. It refuses only on registries that require provenance, and warns
otherwise.

#### `PROVENANCE_INVALID` {#provenance-invalid}

The version claims a provenance attestation that the transparency log does not
hold. That is a stronger signal than a missing one. Tell your administrator.

#### `PROVENANCE_UNVERIFIABLE` {#provenance-unverifiable}

The attestation could not be checked, for instance because the forge did not
answer. This code only ever warns.

#### `PROVENANCE_REMOVED` {#provenance-removed}

Earlier versions had a provenance attestation and this one does not. A build
pipeline that stopped signing is worth asking about before you upgrade.

#### `SIGNATURE_MISSING` {#signature-missing}

The registry requires signed releases and this one is not signed. Use a signed
version, or ask the publisher to sign their releases.

### The registry's own policy {#policy}

#### `LICENSE_DENIED` {#license-denied}

The version's declared licence is not one your organisation allows on this
registry. This is a policy decision, not a defect: ask whoever owns the
licence policy, and do not copy the package in by another route.

#### `UNTRUSTED_PUBLISHER` {#untrusted-publisher}

This registry serves only packages from an approved list of publishers, and
this one is not on it. Ask your administrator to add the publisher if you need
it.

#### `BLOCK_LIST` {#block-list}

An administrator blocked this version, and `batlehub-cli why` shows their reason
when you can see findings. This is always refused, whatever the registry's
mode. Ask the administrator, or use another version.

#### `SOC_VERDICT` {#soc-verdict}

Your security team, or a feed it trusts, flagged this version for blocking.
This is always refused. Use another version, and ask the security team if you
think the flag is wrong.

#### `ADMIN_OVERRIDE` {#admin-override}

Not a refusal. An administrator exempted this version from a hold or a
refusal, so it is served as `warned` with this code beside the original ones.
The exemption has an end date.

### Git forges {#forge}

These come from registries that proxy GitHub, GitLab or Forgejo.

#### `MUTABLE_REF` {#mutable-ref}

You asked for a branch, and a branch moves: the same request can return
different code tomorrow. Ask for a tag or a commit instead. This code warns by
default.

#### `TAG_MOVED` {#tag-moved}

The tag you asked for now points to a different commit than when this instance
first saw it. Tags are supposed to be permanent, so this is refused by
default. Find out why the tag moved before you trust it.

#### `ASSET_REPLACED` {#asset-replaced}

A release file was replaced under the same name: its contents changed since
this instance cached it. It is treated like a moved tag.

#### `PINNED_REF_REQUIRED` {#pinned-ref-required}

This registry serves raw files only at a tag or a commit, never at a branch.
Use a pinned ref in the URL.

#### `RAW_SCRIPT` {#raw-script}

You fetched a script as a raw file from a forge. Running a script straight from
a forge is how many supply-chain attacks start, so the registry warns or
refuses. Check the script, and pin it to a commit.

---

### A code this server does not know {#unknown-code}

#### `UNRECOGNISED` {#unrecognised}

The version's verdict was written by a newer release of the server than the
one that answered you, and carries a reason this release cannot read. It is
refused rather than guessed at. This happens only while an upgrade is rolling
out, or after a rollback: retry in a few minutes, and if it persists, tell your
administrator — the replica that answered is older than the database it reads.

## Refusals with a plain message {#plain-messages}

These come from registries without a supply-chain profile. The message is the
whole answer, and `batlehub-cli why` has nothing to add.

| The message starts with… | Meaning | What to do |
| --- | --- | --- |
| `no grant for 'releases:read'` | Your token may not read this registry, or this package | Ask for access, or check you are using the right token |
| `release is too recent` | A cooling-off period after every release | Wait the remaining seconds, or pin the previous version |
| `release timestamp is missing` | The upstream did not date it, and this registry requires a date | Ask your administrator |
| `requests for the 'latest' version tag are not allowed` | This registry requires an explicit version | Pin a version in your manifest or lockfile |
| `version '…' is blocked by policy` | This exact version is known to be bad | Use another version |
| `version '…' is not in the approved allowlist` | Only approved versions are served here | Use one in the approved range, or ask for it to be approved |
| `publisher '…' is not in the trusted_publisher allowlist` | Only approved publishers are served | Ask for the publisher to be added |
| `cannot verify the publisher` | The publisher check is on, and this registry type cannot report publishers | Your administrator's configuration needs a fix |
| `this release is not signed` | Only signed releases are served | Use a signed version |
| `blocked: known … vulnerability` | A recorded vulnerability at or above the threshold | Upgrade, or ask for an exemption |
| `blocked: unrated vulnerability` | An advisory with no severity, judged at the registry's `unknown_severity` | Upgrade, or ask for an exemption |
| `blocked: known malicious package` | An OSV `MAL-` record lists this version as malware | Remove it from your project; see [`KNOWN_MALICIOUS`](#known-malicious) |
| `blocked: licence '…'` / `blocked: no declared licence` | The licence policy | Ask whoever owns the licence policy |
| `blocked: … flagged by …` | Your security team flagged it | Use another version |
| any other text | Most likely an administrator's block, in their own words | Ask the administrator |

Every refusal is recorded in the audit log with your identity, so you do not
need to reproduce it. Send the time and the package to your administrator, and
they can find it.
