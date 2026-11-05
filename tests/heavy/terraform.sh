#!/usr/bin/env bash
# Heavy Terraform integration test — `terraform init`, against the real client.
#
# The registry protocol in proxy mode had never once worked, and RFC 0009 §12.11
# and §12.12 are three separate reasons why — each one a `200` at the right URL:
#
#   1. **Discovery was unreachable where it exists.** `/.well-known/terraform.json`
#      only answers on a host bound to one registry, and the host-routing
#      middleware rewrites the path to `/proxy/{registry}/.well-known/…` before
#      routing. Nothing was registered there, so the npm/cargo catch-all replied
#      *"registry 'tf' is not an npm or cargo registry"*.
#   2. **The download document was the versions listing.** The handler asked for
#      `DocumentKind::Versions` and patched URLs into whatever came back, so a
#      request for one platform of one version got the list of every version:
#      *"registry response to request for linux_amd64 archive has incorrect
#      target _"* — an empty os and arch joined by an underscore.
#   3. **The archive was that download document.** `fetch_artifact` followed the
#      `download_url` field for `shasums` and `shasums.sig` but not for the
#      archive, so it streamed 8 KB of JSON labelled `application/zip` as the
#      provider binary. Terraform caught it on the checksum — which means the
#      proxied shasums and their signature had already verified against
#      HashiCorp's key.
#
# None of the three is a wrong path, and no status code, schema check or route
# assertion catches any of them. `terraform init` does, in one command.
#
# TLS is not decoration here: Terraform rejects a plain-`http:` registry with no
# opt-out (§12.3), so the tap terminates TLS with a certificate this run
# generates and Terraform is told to trust it through `SSL_CERT_FILE`.
#
# Run via `task test:terraform-heavy` or directly. Needs network: the registry
# is a proxy of registry.terraform.io, and the point is that every byte of a
# real provider install passes through BatleHub.
#
# Environment knobs: DATABASE_URL (required), HEAVY_PORT (8087),
# HEAVY_TAP_PORT (8443 — the TLS port Terraform is pointed at), COVERAGE,
# TERRAFORM_VERSION (1.16.2) for the mise fallback, TF_PROVIDER
# (hashicorp/null), TF_PROVIDER_VERSION (3.2.2).

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

# 8443 rather than the usual 809x: this port is in a provider source address
# that Terraform parses, and a privileged-looking one is what a reader expects
# to see there.
heavy_init terraform 8087 8443
heavy_need python3 "python3"

TERRAFORM_VERSION="${TERRAFORM_VERSION:-1.16.2}"
heavy_runner_for terraform "terraform@$TERRAFORM_VERSION"
TF=("${HEAVY_RUNNER[@]}" terraform)

REGISTRY="tf-$HEAVY_RUN"
TF_PROVIDER="${TF_PROVIDER:-hashicorp/null}"
TF_PROVIDER_VERSION="${TF_PROVIDER_VERSION:-3.2.2}"
PROVIDER_NS="${TF_PROVIDER%%/*}"
PROVIDER_TYPE="${TF_PROVIDER##*/}"

# The hostname Terraform will use in the source address. It must resolve, be in
# the certificate, and be bound to this registry in the config — `localhost` is
# the only name that is all three without touching /etc/hosts.
export HEAVY_TAP_HOST=localhost
TF_HOST="localhost:$HEAVY_TAP_PORT"

heavy_start_server tests/heavy/config.terraform.toml
CERT="$(heavy_self_signed "$HEAVY_TAP_HOST" tf.localhost)"
heavy_start_tap "$CERT" "$HEAVY_WORK/tls-key.pem"

# Terraform is a Go program: it reads the system roots, and SSL_CERT_FILE
# replaces them. Trusting one file for one run beats installing a CA.
export SSL_CERT_FILE="$CERT"

heavy_log "terraform $("${TF[@]}" version | head -1)"

# ── 1. A configuration whose provider is addressed at this instance ──────────

PROJECT="$HEAVY_WORK/project"
mkdir -p "$PROJECT"
cat > "$PROJECT/main.tf" <<EOF
terraform {
  required_providers {
    probe = {
      source  = "$TF_HOST/$PROVIDER_NS/$PROVIDER_TYPE"
      version = "$TF_PROVIDER_VERSION"
    }
  }
}
EOF

export TF_PLUGIN_CACHE_DIR="$HEAVY_WORK/plugin-cache"
export TF_CLI_CONFIG_FILE="$HEAVY_WORK/terraformrc"
mkdir -p "$TF_PLUGIN_CACHE_DIR"
cat > "$TF_CLI_CONFIG_FILE" <<EOF
plugin_cache_dir = "$TF_PLUGIN_CACHE_DIR"
disable_checkpoint = true
EOF
export TF_IN_AUTOMATION=1 CHECKPOINT_DISABLE=1

# ── 2. terraform init ────────────────────────────────────────────────────────

heavy_mark "init"
heavy_log "terraform init (provider $TF_PROVIDER $TF_PROVIDER_VERSION through $TF_HOST)"
set +e
(cd "$PROJECT" && "${TF[@]}" init -no-color) >"$HEAVY_WORK/init.log" 2>&1
INIT_RC=$?
set -e
cat "$HEAVY_WORK/init.log"

if [[ $INIT_RC -ne 0 ]]; then
  # Name the three known shapes, because the message Terraform prints is the
  # only place they are distinguishable and the next person to see one should
  # not have to re-derive which is which.
  if grep -q "not an npm or cargo registry" "$HEAVY_WORK/init.log"; then
    heavy_fail "discovery was answered by the npm/cargo catch-all — the host-routed .well-known path is unregistered (RFC 0009 §12.11)"
  fi
  if grep -q "incorrect target" "$HEAVY_WORK/init.log"; then
    heavy_fail "the download endpoint answered with the versions listing, so os/arch came back empty (RFC 0009 §12.12)"
  fi
  if grep -q "incorrect checksum" "$HEAVY_WORK/init.log"; then
    heavy_fail "the archive was not the archive — most likely the download document served as the provider zip (RFC 0009 §12.12)"
  fi
  heavy_fail "terraform init failed (rc=$INIT_RC)"
fi

grep -q "Terraform has been successfully initialized" "$HEAVY_WORK/init.log" \
  || heavy_fail "terraform init reported no success line"
grep -qi "signed by" "$HEAVY_WORK/init.log" \
  || heavy_fail "Terraform did not report a signature verification — the proxied shasums/signature are what make that possible"

# ── 3. The whole sequence, through the proxy ─────────────────────────────────
#
# Six requests, and the last three are the ones that matter: an install that
# reached the internet for its checksums or its bytes would show up as their
# absence here, not as a failure.

# The paths here carry no `/proxy/{registry}` prefix: this registry is reached
# on its own host, and the rewrite to the canonical subpath happens *inside* the
# server. What the tap sees is what Terraform sent — which is the whole reason
# §12.11 existed, since the rewritten path was the one with no route.
BASE="/v1/providers/$PROVIDER_NS/$PROVIDER_TYPE"
heavy_wire_after "init" "GET /.well-known/terraform.json -> 200" \
  "discovery did not answer on the host it is the only place to answer on"
heavy_wire_after "init" "$BASE/versions -> 200" "the versions listing was not read"
heavy_wire_after "init" "$BASE/$TF_PROVIDER_VERSION/download/linux/amd64 -> 200" \
  "the download document was not read"
heavy_wire_after "init" "$BASE/$TF_PROVIDER_VERSION/shasums -> 200" \
  "the checksum manifest was fetched somewhere else — an air-gapped install would stop here (RFC 0009 §12.8)"
heavy_wire_after "init" "$BASE/$TF_PROVIDER_VERSION/shasums.sig -> 200" \
  "the signature was fetched somewhere else"
heavy_wire_after "init" "$BASE/$TF_PROVIDER_VERSION/artifact/linux/amd64 -> 200" \
  "the provider archive did not come through the proxy"

# It is a real provider binary, not a document wearing `application/zip`: the
# lock file records the checksum Terraform computed and accepted.
grep -q "$TF_HOST/$PROVIDER_NS/$PROVIDER_TYPE" "$PROJECT/.terraform.lock.hcl" \
  || { cat "$PROJECT/.terraform.lock.hcl" >&2; heavy_fail "the lock file does not name this registry as the provider's source"; }
# `-L`: with a plugin cache dir the per-project directory is a *symlink* into
# the cache, and find does not descend into one by default.
BINARY="$(find -L "$PROJECT/.terraform" "$TF_PLUGIN_CACHE_DIR" \
  -type f -name "terraform-provider-$PROVIDER_TYPE*" 2>/dev/null | head -1)"
[[ -n "$BINARY" ]] || heavy_fail "no provider binary was installed"
# It is an executable, not a document wearing `application/zip`. The bug in
# §12.12 unpacked to 8 KB of JSON; a real provider is megabytes of ELF, and the
# two are only distinguishable by looking.
head -c 4 "$BINARY" | grep -q "ELF" \
  || heavy_fail "the installed provider is not an ELF binary: $(head -c 120 "$BINARY")"

heavy_log "TERRAFORM-INIT-OK (discovery, versions, download document, shasums, signature, archive — all through BatleHub)"

# ── 4. A module and a provider published by batlehub-cli (RFC 0040 §6.P case 7)
#
# The CLI reaches the instance at 127.0.0.1, not `localhost`: a host bound to a
# registry rewrites *every* path to that registry's, `/api/v1/registries`
# included. Terraform keeps using `localhost`, the host its addresses name —
# `tf.localhost` for the module, since a module host must contain a dot.

export HEAVY_CLI_SERVER="https://127.0.0.1:$HEAVY_TAP_PORT"

MOD_SRC="$HEAVY_WORK/module-src"
mkdir -p "$MOD_SRC"
printf 'output "greeting" {\n  value = "from a published module"\n}\n' > "$MOD_SRC/main.tf"
tar -czf "$HEAVY_WORK/module.tar.gz" -C "$MOD_SRC" main.tf
heavy_mark "module-publish"
heavy_cli "$ADMIN_TOKEN" publish "$HEAVY_WORK/module.tar.gz" -r "$REGISTRY" --name heavy/greet/aws --version 0.1.0
[[ "$HEAVY_CLI_CODE" == 0 ]] || { cat "$HEAVY_CLI_OUT" >&2; heavy_fail "batlehub-cli could not publish the module (exit $HEAVY_CLI_CODE)"; }
heavy_wire_re_after "module-publish" "POST /proxy/$REGISTRY/v1/modules/heavy/greet/aws/0[.]1[.]0 -> 20[01]" \
  "the CLI did not publish through the module upload route"

MOD_PROJECT="$HEAVY_WORK/module-project"
mkdir -p "$MOD_PROJECT"
cat > "$MOD_PROJECT/main.tf" <<EOF
module "greet" {
  source  = "tf.localhost:$HEAVY_TAP_PORT/heavy/greet/aws"
  version = "0.1.0"
}
EOF
heavy_mark "module-init"
(cd "$MOD_PROJECT" && "${TF[@]}" init -no-color) >"$HEAVY_WORK/module-init.log" 2>&1 \
  || { cat "$HEAVY_WORK/module-init.log" >&2; heavy_fail "terraform init could not install the module the CLI published"; }
heavy_wire_after "module-init" "/v1/modules/heavy/greet/aws/0.1.0/download" \
  "terraform did not ask this instance for the module's download location"
[[ -f "$MOD_PROJECT/.terraform/modules/greet/main.tf" ]] \
  || heavy_fail "the published module's main.tf is not where terraform installs modules"

PROV_SRC="$HEAVY_WORK/provider-src"
mkdir -p "$PROV_SRC/bin"
printf '#!/bin/sh\necho heavy\n' > "$PROV_SRC/bin/terraform-provider-probe_v0.1.0"
chmod +x "$PROV_SRC/bin/terraform-provider-probe_v0.1.0"
mkdir -p "$HEAVY_WORK/provider"
(cd "$PROV_SRC/bin" && python3 -c "
import zipfile,sys
z=zipfile.ZipFile(sys.argv[1],'w')
i=zipfile.ZipInfo('terraform-provider-probe_v0.1.0'); i.external_attr=0o755<<16
z.writestr(i, open('terraform-provider-probe_v0.1.0','rb').read())" \
  "$HEAVY_WORK/provider/terraform-provider-probe_0.1.0_linux_amd64.zip")
# The release as goreleaser lays it out: the archives, their `SHA256SUMS` and
# a detached signature over it, by a key this run makes and registers for the
# `heavy` namespace (issue #281). A short GNUPGHOME: gpg-agent's socket path
# has to fit in 108 bytes, which the work directory may not.
heavy_need gpg "gpg"
GNUPGHOME="$(mktemp -d)"
export GNUPGHOME
gpg --batch --passphrase '' --quick-gen-key "heavy <heavy@localhost>" rsa3072 sign never 2>/dev/null
TF_KEY_ID="$(gpg --with-colons --list-keys heavy@localhost | awk -F: '$1 == "pub" { print $5; exit }')"
gpg --armor --export heavy@localhost >"$HEAVY_WORK/provider-key.asc"
(cd "$HEAVY_WORK/provider" \
  && sha256sum terraform-provider-probe_0.1.0_*.zip >terraform-provider-probe_0.1.0_SHA256SUMS \
  && gpg --batch --detach-sign terraform-provider-probe_0.1.0_SHA256SUMS)
heavy_cli "$ADMIN_TOKEN" namespace signing-key set heavy -r "$REGISTRY" \
  --key-id "$TF_KEY_ID" --armor "$HEAVY_WORK/provider-key.asc"
[[ "$HEAVY_CLI_CODE" == 0 ]] || { cat "$HEAVY_CLI_OUT" >&2; heavy_fail "batlehub-cli could not register the namespace's signing key (exit $HEAVY_CLI_CODE)"; }

heavy_mark "provider-publish"
heavy_cli "$ADMIN_TOKEN" publish "$HEAVY_WORK/provider" -r "$REGISTRY" --name heavy/probe
[[ "$HEAVY_CLI_CODE" == 0 ]] || { cat "$HEAVY_CLI_OUT" >&2; heavy_fail "batlehub-cli could not publish the provider (exit $HEAVY_CLI_CODE)"; }
heavy_wire_after "provider-publish" "POST /proxy/$REGISTRY/v1/providers/heavy/probe/versions" \
  "the CLI did not send the provider's version manifest"
heavy_wire_after "provider-publish" "PUT /proxy/$REGISTRY/v1/providers/heavy/probe/0.1.0/artifact/linux/amd64" \
  "the CLI did not upload the provider's platform archive"

heavy_wire_after "provider-publish" "PUT /proxy/$REGISTRY/v1/providers/heavy/probe/0.1.0/shasums -> 200" \
  "the CLI did not upload the release's SHA256SUMS"
heavy_wire_after "provider-publish" "PUT /proxy/$REGISTRY/v1/providers/heavy/probe/0.1.0/shasums.sig -> 200" \
  "the CLI did not upload the signature over SHA256SUMS"

# The instance checked that signature against the namespace's key before
# storing it: gpg's own RSA output, through the server's OpenPGP verifier.
SIG_ANSWER="$(curl -sk -X PUT -H "Authorization: Bearer $ADMIN_TOKEN" \
  --data-binary "@$HEAVY_WORK/provider/terraform-provider-probe_0.1.0_SHA256SUMS.sig" \
  "$HEAVY_CLI_SERVER/proxy/$REGISTRY/v1/providers/heavy/probe/0.1.0/shasums.sig")"
[[ "$SIG_ANSWER" == *"verified with key $TF_KEY_ID"* ]] \
  || heavy_fail "the instance did not verify the gpg signature with the namespace's key: $SIG_ANSWER"

# Installed by `terraform init`, which verifies the archive against the list
# and the list against the namespace's key. Before issue #281 nothing uploaded
# either, and Terraform refused the archive ("checksum list has no SHA-256
# hash for …", RFC 0040 §17.1).
PROV_PROJECT="$HEAVY_WORK/provider-project"
mkdir -p "$PROV_PROJECT"
cat > "$PROV_PROJECT/main.tf" <<EOF
terraform {
  required_providers {
    probe = {
      source  = "$TF_HOST/heavy/probe"
      version = "0.1.0"
    }
  }
}
EOF
heavy_mark "provider-init"
(cd "$PROV_PROJECT" && "${TF[@]}" init -no-color) >"$HEAVY_WORK/provider-init.log" 2>&1 \
  || { cat "$HEAVY_WORK/provider-init.log" >&2; heavy_fail "terraform init could not install the provider the CLI published"; }
grep -q "signed by" "$HEAVY_WORK/provider-init.log" \
  || { cat "$HEAVY_WORK/provider-init.log" >&2; heavy_fail "terraform did not verify the published provider's signature"; }
PROV_BASE="/v1/providers/heavy/probe/0.1.0"
heavy_wire_after "provider-init" "$PROV_BASE/shasums -> 200" \
  "terraform did not read the published SHA256SUMS from this instance"
heavy_wire_after "provider-init" "$PROV_BASE/shasums.sig -> 200" \
  "terraform did not read the published signature from this instance"
heavy_wire_after "provider-init" "$PROV_BASE/artifact/linux/amd64 -> 200" \
  "terraform did not fetch the published archive from this instance"
heavy_log "TERRAFORM-PUBLISH-OK (a module and a provider published by batlehub-cli, both installed by terraform init)"
heavy_done TERRAFORM-HEAVY-OK
