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
#            then the same upload with the tap answering `202 Accepted`.
#
# Run via `task test:maven-heavy` or directly. Needs network: repo1.maven.org.
# Environment knobs: DATABASE_URL (required), HEAVY_PORT (8104), HEAVY_TAP_PORT
# (8114), COVERAGE, HEAVY_MAVEN_GROUP (org.apache.commons), HEAVY_MAVEN_ARTIFACT
# (commons-lang3), HEAVY_MAVEN_BLOCKED (3.20.0, the newest), HEAVY_MAVEN_PREVIOUS (3.19.0).

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

heavy_init maven 8104 8114
heavy_runner_for mvn maven@3.9.16 java@temurin-21.0.11+10.0.LTS
heavy_need python3 "python3 (the wire tap)"

REG="mvn-$HEAVY_RUN"
LOCAL="mvn-local-$HEAVY_RUN"
GROUP="${HEAVY_MAVEN_GROUP:-org.apache.commons}"
ARTIFACT="${HEAVY_MAVEN_ARTIFACT:-commons-lang3}"
BLOCKED="${HEAVY_MAVEN_BLOCKED:-3.20.0}"
PREVIOUS="${HEAVY_MAVEN_PREVIOUS:-3.19.0}"
GROUP_PATH="${GROUP//.//}"

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

heavy_done "maven heavy test passed: hide=omitted/$PREVIOUS refuse=$NATIVE(x$NATIVE_TRIES)/$OTHER recover=$RECOVER publish-202=$PUBLISH_202"
