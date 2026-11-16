#!/usr/bin/env bash
# Heavy Dependency-Track test — a real Dependency-Track whose only egress is
# this instance, finding Log4Shell in a Java SBOM.
#
# Dependency-Track 4.14 (the single-jar line, embedded H2) is started with its
# HTTP proxy set to a closed port and `no_proxy` limited to 127.0.0.1: a request
# to anywhere but this host fails. Every feed it needs for a Java project is
# then pointed at a registry here, as docs/use/vulnerability-proxy.md
# § Maven — Dependency-Track describes:
#
#   osv-*    generic → osv-vulnerabilities.storage.googleapis.com (Maven/all.zip)
#   nvd-*    generic → nvd.nist.gov/feeds (the 2021 and `modified` feeds only)
#   epss-*   generic → epss.empiricalsecurity.com
#   maven-*  maven   → repo1.maven.org, read with a token over HTTP Basic
#
# What it proves, in order:
#
#   1. The egress is closed. Before configuration, Dependency-Track's own
#      listing of OSV ecosystems — a GET of the default googleapis host — is
#      empty.
#   2. The same listing, pointed at BatleHub, names `Maven`.
#   3. After a restart (Dependency-Track schedules its mirrors at boot, and a
#      changed property does not re-run them), the OSV, NVD and EPSS mirrors
#      complete, each feed read through the tap with a 200.
#   4. A CycloneDX SBOM holding log4j-core 2.14.1 (purl and CPE) is analysed:
#      the findings carry GHSA-jfh8-c2jp-5v3q (from OSV, by purl) and
#      CVE-2021-44228 (from NVD, by CPE) with an EPSS score.
#   5. The repository analyzer reads log4j-core's maven-metadata.xml through
#      the `maven` registry with Basic credentials, and the latest version
#      Dependency-Track reports is Central's.
#
# Run via `task test:dependency-track-heavy` or directly. Needs network (the four
# upstreams above, ~25 MB) and Java 21 (on PATH, or through mise). Environment
# knobs: DATABASE_URL (required), HEAVY_PORT (8184), HEAVY_TAP_PORT (8185),
# HEAVY_DT_PORT (8187), COVERAGE.

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

heavy_init dependency_track 8184 8185
# Dependency-Track 4.14 needs Java 21; a GitHub runner's default `java` is 17
# and ships 21 beside it.
[[ -n "${JAVA_HOME_21_X64:-}" ]] && export PATH="$JAVA_HOME_21_X64/bin:$PATH"
heavy_runner_for java java@temurin-21.0.11+10.0.LTS
"${HEAVY_RUNNER[@]}" java -version 2>&1 | grep -q 'version "2[1-9]' \
  || heavy_fail "Dependency-Track 4.14 needs Java 21: $("${HEAVY_RUNNER[@]}" java -version 2>&1 | head -1)"
heavy_need python3 "python3 (the wire tap, JSON reads)"

DT_VERSION="4.14.6"
DT_SHA256="3a6866f8919864dca70c50f438874d0c0396c27077d3ef8e4d358d940459eeb5"
DT_PORT="${HEAVY_DT_PORT:-8187}"
DT="http://127.0.0.1:$DT_PORT"
DT_PASS="heavy-dt-pass"
DT_TOKEN="heavy-dt-token"
OSV_REG="osv-$HEAVY_RUN"
NVD_REG="nvd-$HEAVY_RUN"
EPSS_REG="epss-$HEAVY_RUN"
MVN_REG="maven-$HEAVY_RUN"
LOG4J_PATH="org/apache/logging/log4j/log4j-core"
LOG4SHELL_GHSA="GHSA-jfh8-c2jp-5v3q"
LOG4SHELL_CVE="CVE-2021-44228"

# ── The jar, pinned by its release checksum ──────────────────────────────────

DT_JAR="$HEAVY_CACHE/dependency-track-$DT_VERSION/dependency-track-apiserver.jar"
if ! echo "$DT_SHA256  $DT_JAR" | sha256sum -c --quiet 2>/dev/null; then
  heavy_log "Downloading Dependency-Track $DT_VERSION"
  mkdir -p "$(dirname "$DT_JAR")"
  # `-L` follows GitHub's redirect to its asset host; `--proto-redir` keeps it on https.
  curl -fsSL --proto '=https' --proto-redir '=https' -o "$DT_JAR.part" \
    "https://github.com/DependencyTrack/dependency-track/releases/download/$DT_VERSION/dependency-track-apiserver.jar" \
    || heavy_fail "could not download Dependency-Track $DT_VERSION"
  echo "$DT_SHA256  $DT_JAR.part" | sha256sum -c --quiet \
    || { rm -f "$DT_JAR.part"; heavy_fail "Dependency-Track $DT_VERSION does not match its release checksum"; }
  mv "$DT_JAR.part" "$DT_JAR"
fi

heavy_start_server tests/heavy/config.dependency-track.toml
heavy_start_tap

# ── Dependency-Track: start, stop, call ──────────────────────────────────────

DT_DATA="$HEAVY_WORK/dt-data"
mkdir -p "$DT_DATA"
DT_PID=""

# start_dt <n> — boot n, logging to dt-<n>.log; waits for the API.
start_dt() {
  local boot="$1"
  DT_LOG="$HEAVY_WORK/dt-$boot.log"
  ALPINE_DATA_DIRECTORY="$DT_DATA" \
    ALPINE_DATABASE_URL="jdbc:h2:$DT_DATA/db;DB_CLOSE_ON_EXIT=FALSE" \
    ALPINE_HTTP_PROXY_ADDRESS=127.0.0.1 ALPINE_HTTP_PROXY_PORT=9 \
    ALPINE_NO_PROXY=127.0.0.1,localhost \
    setsid "${HEAVY_RUNNER[@]}" java -Xmx3g -jar "$DT_JAR" -host 127.0.0.1 -port "$DT_PORT" \
    >"$DT_LOG" 2>&1 </dev/null &
  DT_PID=$!
  HEAVY_EXTRA_PIDS+=("$DT_PID")
  for _ in $(seq 1 90); do
    curl -sf -o /dev/null "$DT/api/version" && { heavy_log "Dependency-Track $DT_VERSION up (boot $boot)"; return 0; }
    kill -0 "$DT_PID" 2>/dev/null || { tail -50 "$DT_LOG" >&2; heavy_fail "Dependency-Track exited during boot $boot"; }
    sleep 2
  done
  tail -50 "$DT_LOG" >&2
  heavy_fail "Dependency-Track did not answer within 3 minutes (boot $boot)"
}

# stop_dt — SIGTERM to the process group (mise → java), wait for the port.
stop_dt() {
  kill -- "-$DT_PID" 2>/dev/null || kill "$DT_PID" 2>/dev/null || true
  for _ in $(seq 1 60); do
    curl -s -o /dev/null "$DT/api/version" || return 0
    sleep 1
  done
  heavy_fail "Dependency-Track still answering 60 s after SIGTERM"
}

# dt <method> <path> [curl args…] — the API as the admin; body on stdout.
dt() {
  local method="$1" path="$2"
  shift 2
  curl -fsS -X "$method" -H "Authorization: Bearer $DT_JWT" "$DT$path" "$@"
}

# wait_log <pattern> <what> <seconds>
wait_log() {
  local pattern="$1" what="$2" seconds="$3" i
  for ((i = 0; i < seconds; i += 5)); do
    grep -q "$pattern" "$DT_LOG" && return 0
    sleep 5
  done
  grep -E "WARN|ERROR" "$DT_LOG" | tail -30 >&2
  heavy_fail "Dependency-Track: $what not logged within $seconds s"
}

# ── 1. Boot 1: the egress is closed ──────────────────────────────────────────

heavy_mark dt-boot1
start_dt 1
curl -fsS -X POST "$DT/api/v1/user/forceChangePassword" -d username=admin -d password=admin \
  -d newPassword="$DT_PASS" -d confirmPassword="$DT_PASS" -o /dev/null \
  || heavy_fail "could not set the admin password"
DT_JWT=$(curl -fsS -X POST "$DT/api/v1/user/login" -d username=admin -d password="$DT_PASS") \
  || heavy_fail "could not log in to Dependency-Track"

ECOS=$(dt GET /api/v1/integration/osv/ecosystem)
[[ "$ECOS" == "[]" ]] \
  || heavy_fail "1: Dependency-Track listed OSV ecosystems from googleapis directly ($ECOS): its egress is not closed"
heavy_log "1: with the default OSV host, the ecosystem list is empty — direct egress is closed"

# ── 2. Point every source at BatleHub ────────────────────────────────────────

BH="$HEAVY_TAP_BASE/proxy"
dt POST /api/v1/configProperty/aggregate -H "Content-Type: application/json" -o /dev/null -d "[
  {\"groupName\":\"vuln-source\",\"propertyName\":\"google.osv.base.url\",\"propertyValue\":\"$BH/$OSV_REG/generic/\"},
  {\"groupName\":\"vuln-source\",\"propertyName\":\"google.osv.enabled\",\"propertyValue\":\"Maven\"},
  {\"groupName\":\"vuln-source\",\"propertyName\":\"nvd.feeds.url\",\"propertyValue\":\"$BH/$NVD_REG/generic\"},
  {\"groupName\":\"vuln-source\",\"propertyName\":\"epss.feeds.url\",\"propertyValue\":\"$BH/$EPSS_REG/generic\"},
  {\"groupName\":\"scanner\",\"propertyName\":\"ossindex.enabled\",\"propertyValue\":\"false\"},
  {\"groupName\":\"telemetry\",\"propertyName\":\"submission.enabled\",\"propertyValue\":\"false\"}
]" || heavy_fail "2: the configuration properties were refused"

# The five default Maven repositories off, BatleHub's on, with credentials.
dt GET /api/v1/repository/MAVEN >"$HEAVY_WORK/repos.json"
python3 - "$HEAVY_WORK/repos.json" "$HEAVY_WORK/repos" <<'PY'
import json, sys, os
os.makedirs(sys.argv[2], exist_ok=True)
for r in json.load(open(sys.argv[1])):
    r["enabled"] = False
    json.dump(r, open(os.path.join(sys.argv[2], r["identifier"] + ".json"), "w"))
PY
for f in "$HEAVY_WORK"/repos/*.json; do
  dt POST /api/v1/repository -H "Content-Type: application/json" -d @"$f" -o /dev/null \
    || heavy_fail "2: could not disable the default repository $(basename "$f" .json)"
done
dt PUT /api/v1/repository -H "Content-Type: application/json" -o /dev/null -d "{
  \"type\":\"MAVEN\",\"identifier\":\"batlehub\",\"url\":\"$BH/$MVN_REG/maven2/\",
  \"resolutionOrder\":1,\"enabled\":true,\"internal\":false,
  \"authenticationRequired\":true,\"username\":\"dependency-track\",\"password\":\"$DT_TOKEN\"
}" || heavy_fail "2: could not add BatleHub as a Maven repository"

heavy_mark dt-ecosystems
ECOS=$(dt GET /api/v1/integration/osv/ecosystem)
grep -q '"Maven"' <<<"$ECOS" \
  || heavy_fail "2: through BatleHub, the OSV ecosystem list does not name Maven: ${ECOS:0:200}"
heavy_wire_after dt-ecosystems "GET /proxy/$OSV_REG/generic/ecosystems.txt -> 200"
heavy_log "2: the ecosystem list, read through $OSV_REG, names Maven"
stop_dt

# ── 3. Boot 2: the mirrors, through BatleHub ─────────────────────────────────

# Dependency-Track retries a 403 from the NVD six times, backing off to 32 s
# (NistMirrorTask's RETRY), so the 46 feeds the registry refuses would cost
# hours. The tap turns those refusals into the 404 it gives up on at once;
# the transcript shows them as `403=>404`, and the two feeds served are 200s.
heavy_tap_rewrite GET "/proxy/$NVD_REG/generic/json/cve/2.0/" 403 404

heavy_mark dt-boot2
start_dt 2
wait_log "Google OSV mirroring complete" "the OSV mirror" 600
wait_log "NIST mirroring complete" "the NVD mirror" 900
wait_log "EPSS mirroring complete" "the EPSS mirror" 600
heavy_wire_after dt-boot2 "GET /proxy/$OSV_REG/generic/Maven/all.zip -> 200"
heavy_wire_after dt-boot2 "GET /proxy/$NVD_REG/generic/json/cve/2.0/nvdcve-2.0-2021.json.gz -> 200"
heavy_wire_after dt-boot2 "GET /proxy/$NVD_REG/generic/json/cve/2.0/nvdcve-2.0-modified.json.gz -> 200"
heavy_wire_after dt-boot2 "GET /proxy/$EPSS_REG/generic/epss_scores-current.csv.gz -> 200"
heavy_log "3: the OSV, NVD and EPSS mirrors completed, each feed through BatleHub"

# ── 4. A Java SBOM, analysed ─────────────────────────────────────────────────

cat >"$HEAVY_WORK/bom.json" <<'EOF'
{
  "bomFormat": "CycloneDX",
  "specVersion": "1.5",
  "version": 1,
  "components": [
    {
      "type": "library",
      "group": "org.apache.logging.log4j",
      "name": "log4j-core",
      "version": "2.14.1",
      "purl": "pkg:maven/org.apache.logging.log4j/log4j-core@2.14.1",
      "cpe": "cpe:2.3:a:apache:log4j:2.14.1:*:*:*:*:*:*:*"
    }
  ]
}
EOF
heavy_mark dt-bom
TOKEN=$(dt PUT /api/v1/bom -H "Content-Type: application/json" -d "{
  \"projectName\":\"heavy-java\",\"projectVersion\":\"1.0.0\",\"autoCreate\":true,
  \"bom\":\"$(base64 -w0 "$HEAVY_WORK/bom.json")\"
}" | python3 -c 'import json,sys;print(json.load(sys.stdin)["token"])') \
  || heavy_fail "4: the SBOM upload was refused"
for _ in $(seq 1 60); do
  dt GET "/api/v1/event/token/$TOKEN" | grep -q '"processing":false' && break
  sleep 2
done
PROJECT=$(dt GET "/api/v1/project/lookup?name=heavy-java&version=1.0.0" \
  | python3 -c 'import json,sys;print(json.load(sys.stdin)["uuid"])') \
  || heavy_fail "4: the project the SBOM created is not found"

# The analysis the upload dispatched may still be running: poll the findings.
for _ in $(seq 1 60); do
  dt GET "/api/v1/finding/project/$PROJECT" >"$HEAVY_WORK/findings.json" || true
  grep -q "$LOG4SHELL_CVE" "$HEAVY_WORK/findings.json" && grep -q "$LOG4SHELL_GHSA" "$HEAVY_WORK/findings.json" \
    && grep -q '"epssScore"' "$HEAVY_WORK/findings.json" && break
  sleep 5
done
python3 - "$HEAVY_WORK/findings.json" "$LOG4SHELL_GHSA" "$LOG4SHELL_CVE" <<'PY' \
  || heavy_fail "4: Dependency-Track's findings for log4j-core 2.14.1 miss Log4Shell"
import json, sys
findings, ghsa, cve = json.load(open(sys.argv[1])), sys.argv[2], sys.argv[3]
by_id = {f["vulnerability"]["vulnId"]: f["vulnerability"] for f in findings}
if ghsa not in by_id:
    sys.exit(f"no {ghsa} (OSV, by purl) among {sorted(by_id)}")
if cve not in by_id:
    sys.exit(f"no {cve} (NVD, by CPE) among {sorted(by_id)}")
if by_id[cve].get("source") != "NVD":
    sys.exit(f"{cve} came from {by_id[cve].get('source')}, not NVD")
if not by_id[cve].get("epssScore"):
    sys.exit(f"{cve} has no EPSS score: {by_id[cve]}")
print(f"{len(findings)} findings; {ghsa} from {by_id[ghsa].get('source')}, "
      f"{cve} from NVD with EPSS {by_id[cve]['epssScore']}")
PY
heavy_log "4: Log4Shell found by Dependency-Track from feeds it read only through BatleHub"

# ── 5. The repository analyzer ───────────────────────────────────────────────

LATEST=$(curl -fsS "https://repo1.maven.org/maven2/$LOG4J_PATH/maven-metadata.xml" \
  | sed -n 's:.*<latest>\(.*\)</latest>.*:\1:p') || heavy_fail "5: could not read Central's maven-metadata.xml"
GOT=""
for _ in $(seq 1 60); do
  GOT=$(dt GET "/api/v1/repository/latest?purl=pkg:maven/org.apache.logging.log4j/log4j-core" 2>/dev/null \
    | python3 -c 'import json,sys;print(json.load(sys.stdin).get("latestVersion",""))' 2>/dev/null) || true
  [[ -n "$GOT" ]] && break
  sleep 2
done
[[ "$GOT" == "$LATEST" ]] || heavy_fail "5: Dependency-Track reports latest '$GOT' for log4j-core, Central says '$LATEST'"
heavy_wire_re_after dt-bom "GET /proxy/$MVN_REG/maven2/$LOG4J_PATH/maven-metadata.xml -> 200 .*Authorization: Basic" \
  "5: the repository analyzer did not read maven-metadata.xml through $MVN_REG with Basic credentials"
heavy_log "5: the repository analyzer reads $LATEST through $MVN_REG"

stop_dt
heavy_done "dependency-track heavy test passed: OSV+NVD+EPSS+Maven through BatleHub, $LOG4SHELL_GHSA and $LOG4SHELL_CVE found, latest=$LATEST"
