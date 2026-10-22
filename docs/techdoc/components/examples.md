---
title: examples
description: Test-only crate holding structure, smoke, local-registry and real-proxy integration tests.
---
<!-- techdoc:begin components/examples#notice -->
> Sections between `techdoc` markers are regenerated; write your own notes outside them.
<!-- techdoc:end -->

<!-- techdoc:begin components/examples#role -->
## Role

`crates/examples` (crate `batlehub-examples`) has an empty library (`src/lib.rs` is a placeholder); its content is the tests in `crates/examples/tests/`: `structure.rs`, `smoke.rs`, `local_registry.rs`, `real_proxy.rs`. `task test:real-proxy` drives the real-proxy suite against real upstreams. Client-facing sample projects live separately in the top-level `examples/` directory.
<!-- techdoc:end -->
