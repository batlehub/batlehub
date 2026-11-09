#!/usr/bin/env python3
"""How a quarantined registry would judge a set of package versions (RFC 0041 §17).

    probe_osv_severity.py cases [--record]      # the JDD in cases.json
    probe_osv_severity.py purls <file>          # one purl per line (e.g. a lockfile's)

For each purl it reads OSV's `/v1/query` answer — the recorded one in
osv/<slug>.json, or the live one with --record (which rewrites the recording)
— and computes the verdict under the default profile (max_severity = high,
mode = block) three ways:

  today   severity_of as shipped: database_specific.severity, else a bare
          numeric score, else `unknown`, which never reaches the threshold
  draft   RFC 0041 as first written: MAL- is malware, CVSS vectors are
          scored, an unrated record is judged `high`
  rfc     RFC 0041 decision 21: within a group of twins (records sharing an
          id or alias), a curated database_specific.severity wins over a
          computed vector; an unrated record takes its group's rating; an
          informational record (RUSTSEC unsound/unmaintained/notice) is low;
          a withdrawn one is ignored; what is still unrated is judged high

`cvss` (PyPI) scores the vectors, pinned exactly (RFC 0041 §8); run through uv:
    uv run --with cvss==3.6 python3 -I probe_osv_severity.py cases
"""

import json
import re
import sys
import urllib.request
from datetime import date
from pathlib import Path

HERE = Path(__file__).resolve().parent
RANK = {"unknown": 0, "low": 1, "medium": 2, "high": 3, "critical": 4}
HIGH = RANK["high"]


def parse(s):
    s = (s or "").strip().lower()
    return {"moderate": "medium", "important": "high"}.get(s, s) if s else None


def bucket(score):
    # Severity::from_cvss_score's buckets: 0.0 ("None") is low, a non-score unknown.
    if not 0.0 <= score <= 10.0:
        return "unknown"
    if score >= 9.0:
        return "critical"
    if score >= 7.0:
        return "high"
    if score >= 4.0:
        return "medium"
    return "low"


def vector_score(vector):
    from cvss import CVSS2, CVSS3, CVSS4  # noqa: PLC0415 — optional dependency

    try:
        if vector.startswith("CVSS:4"):
            return CVSS4(vector).base_score
        if vector.startswith("CVSS:3"):
            return CVSS3(vector).base_score
        return CVSS2(vector).base_score
    except Exception:  # noqa: BLE001 — an unparsable vector is "unknown", not a crash
        return None


def today(v):
    s = parse((v.get("database_specific") or {}).get("severity"))
    if s in RANK:
        return s
    for e in v.get("severity") or []:
        try:
            return bucket(float(e.get("score", "")))
        except ValueError:
            continue
    return "unknown"


def malicious(v):
    return any(i.startswith("MAL-") for i in [v.get("id", "")] + (v.get("aliases") or []))


def rated(v):
    s = today(v)
    if s != "unknown":
        return s
    scores = [vector_score(e["score"]) for e in v.get("severity") or [] if "score" in e]
    scores = [x for x in scores if x is not None]
    return bucket(max(scores)) if scores else "unknown"


def curated(v):
    s = parse((v.get("database_specific") or {}).get("severity"))
    return s if s in RANK else None


def informational(v, purl):
    # Decision 21: RUSTSEC only, and only on the affected[] entry of the queried package.
    if not v.get("id", "").startswith("RUSTSEC-"):
        return False
    name = purl.split("/")[-1].rsplit("@", 1)[0]
    return any(
        (a.get("package") or {}).get("name") == name and (a.get("database_specific") or {}).get("informational")
        for a in v.get("affected") or []
    )


def judge(vulns, purl=""):
    # `today` is what ships: withdrawn records are not dropped there.
    t = max((RANK[today(v)] for v in vulns), default=0)
    out = {"today": "denied" if t >= HIGH else "allowed"}
    if any(malicious(v) for v in vulns):  # decided first; nothing below lowers it
        out["draft"] = out["rfc"] = "denied (KNOWN_MALICIOUS)"
        return out
    vulns = [v for v in vulns if not v.get("withdrawn")]
    r = {v["id"]: rated(v) for v in vulns}
    out["draft"] = "denied" if any(RANK[s] >= HIGH or s == "unknown" for s in r.values()) else "allowed"
    names = {v["id"]: {v["id"], *(v.get("aliases") or [])} for v in vulns}
    worst = 0
    for v in vulns:
        if informational(v, purl):
            s = "low"
        elif curated(v):  # its own database_specific.severity first
            s = curated(v)
        else:
            # direct twins only: records sharing an id or alias with this one
            twins = [o for o in vulns if names[o["id"]] & names[v["id"]]]
            cur = [curated(o) for o in twins if curated(o)]
            scored = [r[o["id"]] for o in twins if r[o["id"]] != "unknown"]
            s = max(cur, key=RANK.get) if cur else max(scored, key=RANK.get) if scored else "high"
        worst = max(worst, RANK[s])
    out["rfc"] = "denied" if worst >= HIGH else "allowed"
    return out


def slug(purl):
    return re.sub(r"[^A-Za-z0-9._-]+", "_", purl.removeprefix("pkg:"))


def query(purl, record):
    path = HERE / "osv" / f"{slug(purl)}.json"
    if not record and path.exists():
        return json.loads(path.read_text())["answer"].get("vulns", [])
    req = urllib.request.Request(
        "https://api.osv.dev/v1/query",
        data=json.dumps({"package": {"purl": purl}}).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=30) as r:
        answer = json.load(r)
    if record:
        path.write_text(json.dumps({"purl": purl, "recorded": str(date.today()), "answer": answer}, indent=1) + "\n")
    return answer.get("vulns", [])


def main(argv):
    record = "--record" in argv
    args = [a for a in argv if a != "--record"]
    if args[:1] == ["cases"]:
        cases = json.loads((HERE / "cases.json").read_text())["cases"]
    elif args[:1] == ["purls"] and len(args) == 2:
        cases = [{"purl": p.strip(), "expect": "", "why": ""} for p in Path(args[1]).read_text().splitlines() if p.strip()]
    else:
        print(__doc__, file=sys.stderr)
        return 2
    flips = {"draft": 0, "rfc": 0}
    wrong = 0
    for c in cases:
        v = query(c["purl"], record and args[0] == "cases")
        j = judge(v, c["purl"])
        for k in flips:
            flips[k] += j["today"] != j[k].split()[0]
        ok = not c["expect"] or c["expect"] == "check" or j["rfc"].startswith(c["expect"])
        wrong += not ok
        if v or c["expect"]:
            ids = ",".join(sorted(x["id"] for x in v)[:6]) + ("…" if len(v) > 6 else "")
            print(f"{'  ' if ok else '✗ '}{c['purl']:45} today={j['today']:8} draft={j['draft']:26} rfc={j['rfc']:26} expect={c['expect'] or '-':8} [{len(v)}: {ids}]")
    print(f"\n{len(cases)} purls · flips vs today: draft {flips['draft']}, rfc {flips['rfc']} · cases off expectation: {wrong}")
    return 1 if wrong else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
