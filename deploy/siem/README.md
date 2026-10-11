# BatleHub audit stream — SIEM rules

Sigma rules for the audit stream BatleHub writes under `[logging] format = "json"`
(RFC 0036 §6.2). The operator page — the field reference, what each rule is for,
how to add one — is [SIEM integration](https://batleforc.git.batleforc.fr/batlehub/operations/siem).

```
sigma/       one rule per file; a burst rule carries its base rule and the correlation
fixtures/    a recorded stream, and whether each rule fires on it
replay.py    runs the fixture through the rules, and checks every field a rule
             reads is one the server emits
```

`task siem:check` runs `sigma check` and the replay; CI runs the same in the
`siem` job of `.github/workflows/test.yaml`.

## Every line, and how to select it

Each audit row is one JSON object on stdout, ECS field names at the top level:

```json
{"timestamp":"2026-10-05T09:01:30.000000Z","level":"INFO","target":"batlehub::audit",
 "event.dataset":"batlehub.audit","event.kind":"event","event.category":"authentication",
 "event.action":"credential_rejected","event.outcome":"denied",
 "event.reason":"no authentication provider accepted the credential",
 "source.ip":"203.0.113.9","user_agent.original":"npm/10.9.0",
 "batlehub.audit.persisted":true,"batlehub.audit.throttled_count":20,
 "span":{"request_id":"7f0c…","name":"HTTP request"}, "…": "…"}
```

Select on `event.dataset = "batlehub.audit"` (or `target = "batlehub::audit"`);
every other line on stdout is an ordinary log line.

## Converting the rules

```bash
uvx --from sigma-cli sigma plugin install splunk
uvx --from sigma-cli sigma convert -t splunk --without-pipeline deploy/siem/sigma/
```

Swap `splunk` for your backend (`elasticsearch`, `loki`, `opensearch`, …).
`--without-pipeline` because the field names are already ECS and need no mapping.

## Shipping the stream

**Vector** — read the container's stdout, keep the audit lines, parse them:

```toml
[sources.batlehub]
type = "kubernetes_logs"
extra_label_selector = "app.kubernetes.io/name=batlehub"

[transforms.batlehub_audit]
type = "remap"
inputs = ["batlehub"]
source = '''
. = parse_json!(.message)
'''

[transforms.batlehub_audit_only]
type = "filter"
inputs = ["batlehub_audit"]
condition = '."event.dataset" == "batlehub.audit"'
```

**Fluent Bit** — the same, with the JSON parser and a grep filter:

```ini
[INPUT]
    Name    tail
    Path    /var/log/containers/batlehub-*.log
    Parser  cri
    Tag     batlehub

[FILTER]
    Name         parser
    Match        batlehub
    Key_Name     message
    Parser       json

[FILTER]
    Name    grep
    Match   batlehub
    Regex   event.dataset ^batlehub\.audit$
```

## What is not here yet

`audit_chain_gap.yml` — two consecutive `audit_seal` lines that do not chain —
lands with the seals of RFC 0036 phase 5; until then the stream carries no
`audit_seal` line for it to read.
