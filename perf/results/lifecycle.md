<!-- lifecycle-report -->
## Startup and shutdown

One cold start, 5 warm ones, and one shutdown with `20` requests in flight against a `3000`ms upstream. Milliseconds.

| | median | min | max | |
| --- | ---: | ---: | ---: | :-- |
| Cold start → healthy | 1490 | 1490 | 1490 | empty database: every migration runs |
| Warm start → healthy | 975 | 898 | 1154 | what a restart costs once the schema is there |
| Warm start → port accepts | 547 | 492 | 630 | traffic can arrive from here; readiness is the row above |
| Stop (idle) | 349 | 341 | 352 | SIGTERM → process gone |
| Stop (draining) | 6379 | 6379 | 6379 | SIGTERM → process gone, 20 requests mid-flight |

**Migrations cost 515 ms** of the cold start — the difference between the two rows above, which is the only way to get that number without parsing it out of a log. A replica joining a rollout pays it once; a readiness probe has to allow for it.

**The port accepts 428 ms before `/healthz` answers.** Anything routing on the port rather than on the probe sends traffic into that window.

**Draining costs 6030 ms more than an idle stop.** actix stops accepting on the signal and then waits for what is in flight, so this scales with how long the *slowest upstream* keeps a request open, not with how much this process has to tear down. It is the number a `terminationGracePeriodSeconds` has to cover.

<sub>Measured by `perf/scripts/lifecycle.sh`, which starts and stops a real release binary against `perf/config.soak.toml` — 25 registries, so the startup includes constructing every registry client. Raw per-iteration samples are in `lifecycle-samples.csv` beside this report.</sub>
