# Local changes to httpress 0.6.0

Source: https://crates.io/crates/httpress/0.6.0
Repository: https://github.com/TecuceanuGabriel/httpress
Original MIT license is retained in LICENSE. Source and upstream tests are included;
images and examples are omitted. This is a local fork, not an unmodified release.

The application uses the existing `Benchmark::builder().request_fn(...)` API,
concurrent workers, connection pool, and metrics.

Local changes:

- `client.rs`: follow up to 50 redirects. POST becomes GET on 301/302/303;
  307/308 preserve the method/body. Drop credentials across origins.
- `client.rs`: apply timeout to the complete redirect chain and response stream.
  Drain chunks without collecting the whole body; propagate truncated-body errors.
- `executor.rs`: stagger fixed-rate workers, skip missed timer ticks, remove the
  0.1 RPS per-worker floor, and stop waiting/sending at the duration deadline.
- `executor.rs`: send Ctrl+C diagnostics to stderr, preserving JSON stdout.
- `config.rs` / `executor.rs`: add `HookAction::Stop` for graceful completion of
  event-limited runs. A before-request stop produces no fake failure/latency sample;
  an after-request stop preserves the actual result. Wake workers waiting on rate
  timers during explicit shutdown, while draining in-flight requests.
- `metrics.rs`: use nearest-rank percentiles (round the rank up) so small and
  odd-sized samples do not understate median/tail latency.
- `Cargo.toml`: add `url` for resolving relative redirect locations; build profiles
  are controlled by the workspace root.

`cargo test --workspace` runs both application and upstream tests.
