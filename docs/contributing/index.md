# Contributing

For someone changing the code.

BatleHub is a Rust workspace with a Vue console and this documentation site in
the same repository. The [project repository](https://git.batleforc.fr/batleforc/batlehub)
is the canonical one; the GitHub copy is a mirror.

- **[Working on BatleHub](/contributing/contributing)** — layout of the
  workspace, how to build and run it, and the conventions a change is expected
  to follow.
- **[Testing](/contributing/testing)** — the test suites, what each one covers,
  and which ones need Postgres or MinIO running.
- **[Adding a registry](/contributing/adding-a-registry)** — the nine places a
  new registry type has to be wired in, in order.
- **[Adding a vulnerability scanner](/contributing/adding-a-vulnerability-scanner)**
  — the same, for a CVE source.
- **[Translating the docs](/contributing/translating)** — where a French page
  lives, and what to do when you edit an English page that has one.

If you want to know *why* something works the way it does rather than how to
change it, the [design history](/rfc/) is the place that argues it out.
