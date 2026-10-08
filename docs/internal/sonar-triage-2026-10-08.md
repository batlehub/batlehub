# SonarCloud triage — 2026-10-08

PR #195 (`feat/devfile`), analysed at `7b23491c`. The gate was red on
**security rating on new code, C against A**, from one vulnerability.
Companion to [`sonar-triage-2026-09-22.md`](./sonar-triage-2026-09-22.md).

## `pythonsecurity:S8707` — `deploy/siem/replay.py:182` — won't fix

"Path traversal via faulty LLM-supplied CLI arguments in `replay.load_stream()`".

`replay.py` is a developer and CI check (`task siem:check`, the `SIEM rules`
job), and it does not run on a server. `--stream` is the path of an audit log the
operator chose to replay. The script reads that file and writes nothing. Any
path it can open, the person running it could already `cat`. If the path were
restricted to `deploy/siem/`, replaying a stream captured from a real
deployment would stop working, and that is the reason the flag exists.

Resolved as **won't fix** in the SonarCloud UI.
