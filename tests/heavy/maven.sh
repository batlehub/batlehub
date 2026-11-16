#!/usr/bin/env bash
# Heavy Maven integration test — the real `mvn` against a maven registry,
# measuring the four RFC 0018 §4.4 axes of a rejection.
#
# The proxy is configured the way a Maven proxy is deployed: a `<mirrorOf>*`
# in settings.xml, so plugins and dependencies alike come through it. Every
# phase gets its own local repository, seeded from one warmed with the plugins
# (so the phase downloads the artifact under test and nothing else) — except
# the Recover axis, which reuses the repository that saw the refusal on
# purpose: Maven records a failed resolution (`*.lastUpdated`) and does not
# retry it until the update interval elapses, and whether that stops a lifted
# hold from being noticed is the measurement.
#
#   Hide     A blocked version is omitted from `maven-metadata.xml`: a version
#            range `[$PREVIOUS,)` resolves to the previous version; a pom that
#            pins the blocked one reaches the download gate.
#   Refuse   What mvn prints for the native block status and for the other
#            one; whether it retries.
#   Recover  Same local repository: does the next build see the lifted hold,
#            and does `-U` change the answer.
#   Publish  `mvn deploy:deploy-file` to a local registry: the native status,
#            then the same upload with the tap answering `202 Accepted`. And
#            RFC 0040 §6.P case 6: `batlehub-cli publish app-2.0.0.jar --pom pom.xml`
#            PUTs the jar, the pom and their checksums, and `mvn
#            dependency:get` resolves what it sent.
#   CVE      What a Dependency-Track-style consumer reads from Central, through
#            BatleHub: `maven-metadata.xml`'s latest/release agree with
#            Central's, and a registry behind `[security]` refuses
#            log4j-core 2.14.1 to mvn (SCAN_PENDING, then denied/VULNERABILITY)
#            while the verdict API names Log4Shell by its GHSA and CVE ids.
#            OSV is api.osv.dev's recorded answer, replayed by osv_fake.py.
#
# Run via `task test:maven-heavy` or directly. Needs network: repo1.maven.org.
# Environment knobs: DATABASE_URL (required), HEAVY_PORT (8104), HEAVY_TAP_PORT
# (8114), HEAVY_OSV_PORT (8154), COVERAGE, HEAVY_MAVEN_GROUP (org.apache.commons), HEAVY_MAVEN_ARTIFACT
# (commons-lang3), HEAVY_MAVEN_BLOCKED and HEAVY_MAVEN_PREVIOUS (default: the
# newest two versions in Central's maven-metadata.xml, read at start-up — a
# hard-coded pair went red the day commons-lang3 3.21.0 shipped).

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

heavy_init maven 8104 8114
heavy_runner_for mvn maven@3.9.16 java@temurin-21.0.11+10.0.LTS
heavy_need python3 "python3 (the wire tap)"

REG="mvn-$HEAVY_RUN"
LOCAL="mvn-local-$HEAVY_RUN"
GROUP="${HEAVY_MAVEN_GROUP:-org.apache.commons}"
ARTIFACT="${HEAVY_MAVEN_ARTIFACT:-commons-lang3}"
GROUP_PATH="${GROUP//.//}"
if [[ -z "${HEAVY_MAVEN_BLOCKED:-}" || -z "${HEAVY_MAVEN_PREVIOUS:-}" ]]; then
  NEWEST_TWO="$(curl -fsS "https://repo1.maven.org/maven2/$GROUP_PATH/$ARTIFACT/maven-metadata.xml" \
    | sed -n 's:.*<version>\(.*\)</version>.*:\1:p' | tail -2)" \
    || heavy_fail "could not read $GROUP:$ARTIFACT's maven-metadata.xml from Central"
fi
PREVIOUS="${HEAVY_MAVEN_PREVIOUS:-$(sed -n 1p <<<"$NEWEST_TWO")}"
BLOCKED="${HEAVY_MAVEN_BLOCKED:-$(sed -n 2p <<<"$NEWEST_TWO")}"
[[ -n "$PREVIOUS" && -n "$BLOCKED" ]] || heavy_fail "no two versions to block between for $GROUP:$ARTIFACT"

# The CVE axis's OSV: recorded answers only (the flip flag never exists).
OSV_PORT="${HEAVY_OSV_PORT:-8154}"
export HEAVY_OSV_URL="http://127.0.0.1:$OSV_PORT"
python3 tests/heavy/osv_fake.py "$HEAVY_WORK/no-flip" "$OSV_PORT" "pkg:none/none" \
  tests/fixtures/supply-chain-jdd/osv >"$HEAVY_WORK/osv.err" 2>&1 &
OSV_PID=$!
trap 'kill "$OSV_PID" 2>/dev/null || true; heavy_cleanup' EXIT
for _ in $(seq 1 30); do
  curl -s -o /dev/null -X POST "$HEAVY_OSV_URL/v1/query" -d '{}' && break
  sleep 1
done

heavy_start_server tests/heavy/config.maven.toml
heavy_start_tap

MIRROR="$HEAVY_TAP_BASE/proxy/$REG/maven2"
LOCAL_URL="$HEAVY_TAP_BASE/proxy/$LOCAL/maven2"
JAR="/proxy/$REG/maven2/$GROUP_PATH/$ARTIFACT/$BLOCKED/$ARTIFACT-$BLOCKED.jar"
POM="/proxy/$REG/maven2/$GROUP_PATH/$ARTIFACT/$BLOCKED/$ARTIFACT-$BLOCKED.pom"
METADATA="/proxy/$REG/maven2/$GROUP_PATH/$ARTIFACT/maven-metadata.xml"

cat >"$HEAVY_WORK/settings.xml" <<EOF
<settings>
  <mirrors>
    <mirror>
      <id>heavy</id>
      <mirrorOf>*</mirrorOf>
      <url>$MIRROR</url>
    </mirror>
  </mirrors>
  <servers>
    <server>
      <id>heavy-local</id>
      <username>ci-admin</username>
      <password>$ADMIN_TOKEN</password>
    </server>
  </servers>
</settings>
EOF

# consumer <dir> <version-or-range>
consumer() {
  local dir="$1" version="$2"
  mkdir -p "$dir"
  cat >"$dir/pom.xml" <<EOF
<project>
  <modelVersion>4.0.0</modelVersion>
  <groupId>com.heavy</groupId>
  <artifactId>consumer</artifactId>
  <version>0.0.0</version>
  <dependencies>
    <dependency>
      <groupId>$GROUP</groupId>
      <artifactId>$ARTIFACT</artifactId>
      <version>$version</version>
    </dependency>
  </dependencies>
</project>
EOF
  return $?
}

# run_mvn <repo> <dir> <args...>
run_mvn() {
  local repo="$1" dir="$2"
  shift 2
  (cd "$dir" && "${HEAVY_RUNNER[@]}" mvn -B -s "$HEAVY_WORK/settings.xml" \
    -Dmaven.repo.local="$repo" "$@") >"$RUN_OUT" 2>&1
  return $?
}

# fresh_repo <name> — a local repository holding the warmed plugins and none
# of the artifact under test.
fresh_repo() {
  local repo="$HEAVY_WORK/$1"
  cp -r "$HEAVY_WORK/warm" "$repo"
  rm -rf "$repo/$GROUP_PATH/$ARTIFACT"
  echo "$repo"
  return $?
}

# ── Warm the plugins through the proxy ───────────────────────────────────────

heavy_mark warm
RUN_OUT="$HEAVY_WORK/warm.txt"
consumer "$HEAVY_WORK/w" "$PREVIOUS"
run_mvn "$HEAVY_WORK/warm" "$HEAVY_WORK/w" dependency:resolve \
  || { cat "$RUN_OUT" >&2; heavy_fail "mvn dependency:resolve through the proxy failed"; }
heavy_wire_after warm "GET /proxy/$REG/maven2/$GROUP_PATH/$ARTIFACT/$PREVIOUS/$ARTIFACT-$PREVIOUS.jar -> 200"
# The deploy plugin, for the Publish axis, warmed here so it is not what a
# later phase is seen downloading.
run_mvn "$HEAVY_WORK/warm" "$HEAVY_WORK/w" dependency:resolve-plugins -Dplugin=org.apache.maven.plugins:maven-deploy-plugin >/dev/null 2>&1 || true

# ── Hide ─────────────────────────────────────────────────────────────────────

heavy_mark range-before
RUN_OUT="$HEAVY_WORK/range-before.txt"
consumer "$HEAVY_WORK/c0" "[$PREVIOUS,)"
REPO0=$(fresh_repo repo0)
run_mvn "$REPO0" "$HEAVY_WORK/c0" dependency:resolve \
  || { cat "$RUN_OUT" >&2; heavy_fail "range resolution through the proxy failed"; }
grep -q "$ARTIFACT:jar:$BLOCKED" "$RUN_OUT" \
  || { grep "$ARTIFACT" "$RUN_OUT" >&2; heavy_fail "before the block, [$PREVIOUS,) resolves to something other than $BLOCKED — set HEAVY_MAVEN_BLOCKED to the newest"; }
heavy_wire_after range-before "GET $METADATA -> 200"

heavy_log "Blocking $GROUP:$ARTIFACT@$BLOCKED"
heavy_block "$REG" "$GROUP:$ARTIFACT" "$BLOCKED"

heavy_mark range-after
RUN_OUT="$HEAVY_WORK/range-after.txt"
consumer "$HEAVY_WORK/c1" "[$PREVIOUS,)"
REPO1=$(fresh_repo repo1)
run_mvn "$REPO1" "$HEAVY_WORK/c1" dependency:resolve \
  || { cat "$RUN_OUT" >&2; heavy_fail "range resolution with $BLOCKED blocked failed instead of picking the previous version"; }
grep -q "$ARTIFACT:jar:$PREVIOUS" "$RUN_OUT" \
  || { grep "$ARTIFACT" "$RUN_OUT" >&2; heavy_fail "Hide: the range did not resolve to $PREVIOUS"; }
heavy_log "Hide/fresh: [$PREVIOUS,) resolves to $PREVIOUS with $BLOCKED omitted from maven-metadata.xml"

heavy_mark pinned-native
RUN_OUT="$HEAVY_WORK/pinned-native.txt"
consumer "$HEAVY_WORK/c2" "$BLOCKED"
REPO2=$(fresh_repo repo2)
if run_mvn "$REPO2" "$HEAVY_WORK/c2" dependency:resolve; then
  cat "$RUN_OUT" >&2
  heavy_fail "Hide/pinned: resolving the blocked $BLOCKED succeeded"
fi
FIRST=$(awk -v mark="### pinned-native" -v p="GET /proxy/$REG/maven2/$GROUP_PATH/$ARTIFACT/$BLOCKED/" '
  index($0, mark) == 1 { seen = 1; next } seen && index($0, p) == 1 { print; exit }' "$HEAVY_LOG")
[[ -n "$FIRST" ]] || heavy_fail "Hide/pinned: the pinned build never asked for $BLOCKED"
NATIVE=$(echo "$FIRST" | sed 's/.* -> \([0-9]*\).*/\1/')
NATIVE_TRIES=$(awk -v mark="### pinned-native" -v p="GET $POM -> " '
  index($0, mark) == 1 { seen = 1; next } seen && index($0, p) == 1 { c++ } END { print c + 0 }' "$HEAVY_LOG")
heavy_log "Refuse/native: the block answers $NATIVE ($NATIVE_TRIES request(s) for the pom); mvn said:"
heavy_client_said "$RUN_OUT" 'error'

# ── Refuse: the other status ─────────────────────────────────────────────────

case "$NATIVE" in
  403) OTHER=404 ;;
  404) OTHER=403 ;;
  *) heavy_fail "the block answered $NATIVE, neither 403 nor 404" ;;
esac
heavy_tap_rewrite GET "/proxy/$REG/maven2/$GROUP_PATH/$ARTIFACT/$BLOCKED/" "$NATIVE" "$OTHER" "Retry-After: 30"
heavy_mark pinned-other
RUN_OUT="$HEAVY_WORK/pinned-other.txt"
consumer "$HEAVY_WORK/c3" "$BLOCKED"
REPO3=$(fresh_repo repo3)
START=$(date +%s)
if run_mvn "$REPO3" "$HEAVY_WORK/c3" dependency:resolve; then
  heavy_fail "Refuse: mvn succeeded on a $OTHER"
fi
ELAPSED=$(( $(date +%s) - START ))
heavy_wire_after pinned-other "GET $POM -> $NATIVE=>$OTHER"
heavy_log "Refuse/$OTHER: took ${ELAPSED}s with Retry-After: 30; mvn said:"
heavy_client_said "$RUN_OUT" 'error'
[[ "$ELAPSED" -lt 25 ]] || heavy_fail "mvn waited on Retry-After — the CI contract assumes it does not"
heavy_tap_rewrite_clear

# ── Recover ──────────────────────────────────────────────────────────────────

heavy_log "Unblocking $GROUP:$ARTIFACT@$BLOCKED"
heavy_unblock "$REG" "$GROUP:$ARTIFACT" "$BLOCKED"
heavy_mark recover
RUN_OUT="$HEAVY_WORK/recover.txt"
if run_mvn "$REPO2" "$HEAVY_WORK/c2" dependency:resolve; then
  RECOVER="plain"
  heavy_wire_after recover "GET $POM -> 200"
else
  # Maven's own memory of the failure: the refusal is cached in the local
  # repository until the update interval (daily by default) elapses.
  grep -q "resolution will not be reattempted\|lastUpdated\|cached in the local repository" "$RUN_OUT" \
    || { cat "$RUN_OUT" >&2; heavy_fail "Recover: mvn failed after the unblock for a reason other than its cached failure"; }
  RECOVER="needs -U"
  heavy_mark recover-forced
  RUN_OUT="$HEAVY_WORK/recover-forced.txt"
  run_mvn "$REPO2" "$HEAVY_WORK/c2" -U dependency:resolve \
    || { cat "$RUN_OUT" >&2; heavy_fail "Recover: even mvn -U failed after the unblock"; }
  heavy_wire_after recover-forced "GET $POM -> 200"
fi
heavy_log "Recover: $RECOVER"

# ── Publish ──────────────────────────────────────────────────────────────────

echo "heavy" >"$HEAVY_WORK/payload.txt"
(cd "$HEAVY_WORK" && "${HEAVY_RUNNER[@]}" jar cf lib.jar payload.txt 2>/dev/null) \
  || (cd "$HEAVY_WORK" && zip -q lib.jar payload.txt) \
  || heavy_fail "neither jar nor zip could build a fixture jar"
DEPLOY_PREFIX="/proxy/$LOCAL/maven2/com/heavy/lib/"

# deploy <repo> <version>
deploy() {
  local repo="$1" version="$2"
  run_mvn "$repo" "$HEAVY_WORK" deploy:deploy-file -DrepositoryId=heavy-local -Durl="$LOCAL_URL" \
    -Dfile="$HEAVY_WORK/lib.jar" -DgroupId=com.heavy -DartifactId=lib -Dversion="$version" \
    -Dpackaging=jar -DgeneratePom=true
  return $?
}

heavy_mark publish-native
RUN_OUT="$HEAVY_WORK/publish-native.txt"
deploy "$(fresh_repo repop1)" 1.0.0 \
  || { cat "$RUN_OUT" >&2; heavy_fail "Publish/native: mvn deploy:deploy-file failed"; }
heavy_wire_after publish-native "PUT ${DEPLOY_PREFIX}1.0.0/lib-1.0.0.jar -> 201"
heavy_log "Publish/native (201): BUILD SUCCESS"

heavy_tap_rewrite PUT "$DEPLOY_PREFIX" 201 202
heavy_mark publish-202
RUN_OUT="$HEAVY_WORK/publish-202.txt"
if deploy "$(fresh_repo repop2)" 1.1.0; then
  PUBLISH_202="accepted"
else
  PUBLISH_202="rejected"
fi
heavy_wire_after publish-202 "PUT ${DEPLOY_PREFIX}1.1.0/lib-1.1.0.jar -> 201=>202"
heavy_log "Publish/202: mvn $PUBLISH_202 a 202 Accepted"
heavy_client_said "$RUN_OUT" 'error|build'
heavy_tap_rewrite_clear

# ── Publish through the CLI (RFC 0040 §6.P case 6) ───────────────────────────

mkdir -p "$HEAVY_WORK/cli-pub"
cp "$HEAVY_WORK/lib.jar" "$HEAVY_WORK/cli-pub/app-2.0.0.jar"
cat >"$HEAVY_WORK/cli-pub/pom.xml" <<'EOF'
<project>
  <modelVersion>4.0.0</modelVersion>
  <groupId>com.heavy</groupId>
  <artifactId>app</artifactId>
  <version>2.0.0</version>
</project>
EOF
APP_PREFIX="/proxy/$LOCAL/maven2/com/heavy/app/2.0.0/app-2.0.0"
heavy_mark publish-cli
heavy_cli "$ADMIN_TOKEN" publish "$HEAVY_WORK/cli-pub/app-2.0.0.jar" --pom "$HEAVY_WORK/cli-pub/pom.xml" -r "$LOCAL"
[[ "$HEAVY_CLI_CODE" == 0 ]] || { cat "$HEAVY_CLI_OUT" >&2; heavy_fail "Publish/cli: batlehub-cli publish exited $HEAVY_CLI_CODE"; }
for f in jar jar.sha1 jar.md5 pom pom.sha1 pom.md5; do
  heavy_wire_after publish-cli "PUT $APP_PREFIX.$f -> 201" "Publish/cli: the CLI did not PUT app-2.0.0.$f"
done
# The local registry is not behind the `*` mirror, so dependency:get can name it.
sed 's#<mirrorOf>\*</mirrorOf>#<mirrorOf>*,!heavy-local</mirrorOf>#' "$HEAVY_WORK/settings.xml" > "$HEAVY_WORK/settings-local.xml"
heavy_mark publish-cli-get
RUN_OUT="$HEAVY_WORK/publish-cli-get.txt"
(cd "$HEAVY_WORK" && "${HEAVY_RUNNER[@]}" mvn -B -s "$HEAVY_WORK/settings-local.xml" \
  -Dmaven.repo.local="$(fresh_repo repopcli)" dependency:get -Dtransitive=false \
  -DremoteRepositories="heavy-local::default::$LOCAL_URL" -Dartifact=com.heavy:app:2.0.0) >"$RUN_OUT" 2>&1 \
  || { cat "$RUN_OUT" >&2; heavy_fail "Publish/cli: mvn dependency:get did not resolve what the CLI published"; }
heavy_wire_after publish-cli-get "GET $APP_PREFIX.jar -> 200" "Publish/cli: mvn did not fetch the published jar"
heavy_log "Publish/cli: com.heavy:app:2.0.0 published by the CLI and resolved by mvn"

# ── CVE: Central's advisories, read through BatleHub ─────────────────────────

CVE_REG="mvn-cve-$HEAVY_RUN"
CVE_URL="$HEAVY_TAP_BASE/proxy/$CVE_REG/maven2"
LOG4J_PATH="org/apache/logging/log4j/log4j-core"
LOG4J_NAME="org.apache.logging.log4j:log4j-core"
LOG4J_VULN="2.14.1"
LOG4SHELL_GHSA="GHSA-jfh8-c2jp-5v3q"
LOG4SHELL_CVE="CVE-2021-44228"

# Dependency-Track's repository meta analyzer reads the newest version from
# maven-metadata.xml; through the proxy it must be Central's.
curl -fsS "https://repo1.maven.org/maven2/$LOG4J_PATH/maven-metadata.xml" -o "$HEAVY_WORK/log4j-central.xml" \
  || heavy_fail "CVE: could not read log4j-core's maven-metadata.xml from Central"
heavy_mark cve-metadata
curl -fsS "$MIRROR/$LOG4J_PATH/maven-metadata.xml" -o "$HEAVY_WORK/log4j-proxy.xml" \
  || heavy_fail "CVE: log4j-core's maven-metadata.xml through the proxy failed"
heavy_wire_after cve-metadata "GET /proxy/$REG/maven2/$LOG4J_PATH/maven-metadata.xml -> 200"
for tag in latest release; do
  want=$(sed -n "s:.*<$tag>\(.*\)</$tag>.*:\1:p" "$HEAVY_WORK/log4j-central.xml")
  got=$(sed -n "s:.*<$tag>\(.*\)</$tag>.*:\1:p" "$HEAVY_WORK/log4j-proxy.xml")
  [[ -n "$want" && "$want" == "$got" ]] || heavy_fail "CVE: <$tag> through the proxy is '$got', Central says '$want'"
done

# The `*` mirror is the quarantined registry. A `*,!id` exclusion with
# -DremoteRepositories still sent everything to the first mirror, so the repo
# is seeded from repopcli, which already holds dependency:get's own plugin
# deps: log4j is the only thing this mvn asks the quarantine for.
# mvn reads as the admin (quarantine:read): an anonymous caller gets a bare
# 403 on a hold, by design (RFC 0018 §7), and the reason headers are the
# point here. A `<server>` with the mirror's id, sent preemptively.
sed -e "s#<url>$MIRROR</url>#<url>$CVE_URL</url>#" \
  -e "s#<servers>#<servers><server><id>heavy</id><username>ci-admin</username><password>$ADMIN_TOKEN</password></server>#" \
  "$HEAVY_WORK/settings.xml" > "$HEAVY_WORK/settings-cve.xml"
grep -q "$CVE_URL" "$HEAVY_WORK/settings-cve.xml" || heavy_fail "CVE: settings-cve.xml does not point at $CVE_URL"
# cve_repo <name> — a local repository with the plugins and no log4j-core.
cve_repo() {
  cp -r "$HEAVY_WORK/repopcli" "$HEAVY_WORK/$1"
  echo "$HEAVY_WORK/$1"
  return $?
}
# get_log4j <repo> — mvn dependency:get of the vulnerable version through the
# quarantined registry; its exit status.
get_log4j() {
  (cd "$HEAVY_WORK" && "${HEAVY_RUNNER[@]}" mvn -B -s "$HEAVY_WORK/settings-cve.xml" \
    -Daether.connector.http.preemptiveAuth=true -Daether.transport.http.preemptiveAuth=true \
    -Dmaven.repo.local="$1" dependency:get -Dtransitive=false \
    -Dartifact="$LOG4J_NAME:$LOG4J_VULN") >"$RUN_OUT" 2>&1
  return $?
}
LOG4J_FILE="GET /proxy/$CVE_REG/maven2/$LOG4J_PATH/$LOG4J_VULN/log4j-core-$LOG4J_VULN\.(pom|jar) -> 403 "

heavy_mark cve-first-contact
RUN_OUT="$HEAVY_WORK/cve-first-contact.txt"
if get_log4j "$(cve_repo repocve1)"; then
  cat "$RUN_OUT" >&2
  heavy_fail "CVE: mvn resolved log4j-core $LOG4J_VULN on first contact, before any scan"
fi
heavy_wire_re_after cve-first-contact "$LOG4J_FILE.*X-BatleHub-Reason: SCAN_PENDING" \
  "CVE: first contact with log4j-core $LOG4J_VULN was not held on SCAN_PENDING"

VERDICT_URL="$HEAVY_BASE/api/v1/verdicts/$CVE_REG/$LOG4J_NAME/$LOG4J_VULN"
STATE=""
for _ in $(seq 1 100); do
  curl -fsS -H "Authorization: Bearer $ADMIN_TOKEN" "$VERDICT_URL" -o "$HEAVY_WORK/log4j-verdict.json" 2>/dev/null \
    && STATE=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("state",""))' "$HEAVY_WORK/log4j-verdict.json")
  [[ "$STATE" == "denied" ]] && break
  sleep 3
done
[[ "$STATE" == "denied" ]] || { cat "$HEAVY_WORK/log4j-verdict.json" >&2; heavy_fail "CVE: log4j-core $LOG4J_VULN's verdict is '$STATE' after 5 minutes, expected denied"; }
python3 - "$HEAVY_WORK/log4j-verdict.json" "$LOG4SHELL_GHSA" "$LOG4SHELL_CVE" <<'PY' \
  || heavy_fail "CVE: the verdict API does not carry Log4Shell with its CVE id"
import json, sys
verdict, ghsa, cve = json.load(open(sys.argv[1])), sys.argv[2], sys.argv[3]
hits = [f for f in verdict.get("findings", []) if f.get("reference") == ghsa]
if not hits:
    sys.exit(f"no finding references {ghsa}: {[f.get('reference') for f in verdict.get('findings', [])]}")
if cve not in (hits[0].get("raw") or {}).get("aliases", []):
    sys.exit(f"{ghsa}'s finding has no {cve} alias: {hits[0].get('raw')}")
if hits[0].get("severity") != "critical":
    sys.exit(f"{ghsa} is rated {hits[0].get('severity')}, OSV says critical")
cves = sorted({a for f in verdict["findings"] for a in (f.get("raw") or {}).get("aliases", []) if a.startswith("CVE-")})
print(f"{len(verdict['findings'])} findings, CVEs: {' '.join(cves)}")
PY

heavy_mark cve-refused
RUN_OUT="$HEAVY_WORK/cve-refused.txt"
if get_log4j "$(cve_repo repocve2)"; then
  cat "$RUN_OUT" >&2
  heavy_fail "CVE: mvn resolved log4j-core $LOG4J_VULN over a denied verdict"
fi
heavy_wire_re_after cve-refused "$LOG4J_FILE.*X-BatleHub-Verdict: denied.*X-BatleHub-Reason: [A-Z_,]*VULNERABILITY" \
  "CVE: log4j-core $LOG4J_VULN was not refused as denied/VULNERABILITY on the wire"
heavy_log "CVE: $LOG4SHELL_GHSA ($LOG4SHELL_CVE) refused to mvn and named by the verdict API; mvn said:"
heavy_client_said "$RUN_OUT" 'error'

heavy_done "maven heavy test passed: hide=omitted/$PREVIOUS refuse=$NATIVE(x$NATIVE_TRIES)/$OTHER recover=$RECOVER publish-202=$PUBLISH_202 cve=$LOG4SHELL_CVE"
