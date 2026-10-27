mod batch_size;
mod event_budget;
mod progress;
mod report_file;
mod template;

use std::{
    collections::{BTreeMap, HashMap},
    fs,
    num::{NonZeroU64, NonZeroUsize},
    path::PathBuf,
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use batch_size::BatchSize;
use bytes::Bytes;
use chrono::Utc;
use clap::Parser;
use event_budget::EventBudget;
use httpress::{Benchmark, BenchmarkResults, HookAction, HttpMethod, RequestConfig};
use hyper::header::HeaderValue;
use progress::{EventProgress, EventSnapshot};
use report_file::ReportFile;
use serde_json::json;
use template::{FieldTemplates, PayloadFactory};
use url::Url;

#[derive(Parser)]
#[command(version, about = "Templated batch load tests powered by httpress")]
struct Args {
    /// Request endpoint; command-line flags override ENDPOINT from the environment or .env
    #[arg(
        long = "endpoint",
        visible_alias = "url",
        env = "ENDPOINT",
        hide_env_values = true,
        default_value = "https://zt0b6mmsrc.execute-api.us-east-2.amazonaws.com/test/v1/batch"
    )]
    url: String,

    /// HTTP method; uses METHOD from the environment or .env when no flag is given
    #[arg(
        long,
        env = "METHOD",
        hide_env_values = true,
        value_enum,
        ignore_case = true,
        default_value = "POST"
    )]
    method: HttpMethod,

    /// Environment variable containing the Base64 Basic credential (optional "Basic " prefix)
    #[arg(long, default_value = "ACCESS_TOKEN")]
    auth_env: String,

    /// JSON file containing event, messageId and anonymousId templates
    #[arg(long)]
    template: Option<PathBuf>,

    #[arg(long)]
    event: Option<String>,
    #[arg(long)]
    message_id: Option<String>,
    #[arg(long)]
    anonymous_id: Option<String>,

    /// Events per HTTP request: fixed N or random MIN-MAX; --rate counts HTTP requests
    #[arg(long, default_value = "1-10")]
    batch_size: BatchSize,

    /// Maximum concurrent in-flight HTTP requests
    #[arg(short, long, default_value = "1")]
    concurrency: NonZeroUsize,

    /// Total HTTP requests (default: 1); mutually exclusive with --duration and --events
    #[arg(short = 'n', long, conflicts_with = "duration")]
    requests: Option<NonZeroUsize>,

    /// Total event records to attempt; the final batch is trimmed to avoid oversending
    #[arg(long, conflicts_with_all = ["requests", "duration"])]
    events: Option<NonZeroU64>,

    /// Sending duration, e.g. 60s or 5m; in-flight requests are drained afterward
    #[arg(short, long, value_parser = positive_duration)]
    duration: Option<Duration>,

    /// Target HTTP requests/second across all workers; omitted means unlimited
    #[arg(short, long)]
    rate: Option<NonZeroU64>,

    /// Timeout for the complete request, redirects and response body included
    #[arg(long, default_value = "10s", value_parser = positive_duration)]
    timeout: Duration,

    /// Print one rendered JSON payload without sending or requiring credentials
    #[arg(long)]
    dry_run: bool,

    /// Print a machine-readable JSON report
    #[arg(long)]
    json: bool,

    /// Save a .json or .csv report to a new file; repeat to save both formats
    #[arg(long, conflicts_with = "dry_run")]
    output: Vec<PathBuf>,

    /// Show live event progress on stderr even when redirected (automatic in a terminal)
    #[arg(long, conflicts_with = "no_progress")]
    progress: bool,

    /// Disable live progress
    #[arg(long)]
    no_progress: bool,
}

fn positive_duration(value: &str) -> Result<Duration, String> {
    let duration = humantime::parse_duration(value).map_err(|e| e.to_string())?;
    if duration.is_zero() || duration > Duration::from_secs(7 * 24 * 3600) {
        return Err("duration must be greater than zero and at most 7 days".into());
    }
    Ok(duration)
}

fn authorization(name: &str) -> Result<String> {
    let value = std::env::var(name).with_context(|| {
        format!("set environment variable {name} to your Base64 Basic credential")
    })?;
    let value = value.trim();
    let credential = value.strip_prefix("Basic ").unwrap_or(value);
    ensure!(
        !credential.is_empty() && STANDARD.decode(credential).is_ok(),
        "{name} must contain a Base64 Basic credential, optionally prefixed with 'Basic '"
    );
    let header = format!("Basic {credential}");
    HeaderValue::from_str(&header).context("invalid Authorization header")?;
    Ok(header)
}

#[derive(Default)]
struct EventCounts {
    attempted: AtomicU64,
    successful: AtomicU64,
    failed: AtomicU64,
}

impl EventCounts {
    fn snapshot(&self) -> EventSnapshot {
        let successful = self.successful.load(Ordering::Relaxed);
        let failed = self.failed.load(Ordering::Relaxed);
        // Read attempts last; clamp for independently sampled atomic counters.
        let attempted = self
            .attempted
            .load(Ordering::Relaxed)
            .max(successful + failed);
        EventSnapshot {
            attempted,
            successful,
            failed,
        }
    }
}

fn report(results: &BenchmarkResults, args: &Args, events: &EventCounts) -> serde_json::Value {
    let seconds = results.duration.as_secs_f64();
    let successes = results.successful_requests as f64;
    let event_snapshot = events.snapshot();
    let attempted_events = event_snapshot.attempted;
    let successful_events = event_snapshot.successful;
    let statuses: BTreeMap<_, _> = results.status_codes.iter().collect();
    let responses: usize = results.status_codes.values().sum();
    let non_200_status_codes: BTreeMap<_, _> = results
        .status_codes
        .iter()
        .filter_map(|(&status, &count)| (status != 200).then_some((status, count)))
        .collect();
    let non_200_responses: usize = non_200_status_codes.values().sum();
    json!({
        "generated_at": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "concurrency": args.concurrency.get(),
        "http_requests": results.total_requests,
        "http_responses": responses,
        "successful_http_requests": results.successful_requests,
        "failed_http_requests": results.failed_requests,
        "transport_errors": results.total_requests - responses,
        "elapsed_seconds": seconds,
        "target_http_rps": args.rate.map(NonZeroU64::get),
        "actual_http_rps": results.throughput,
        "successful_http_rps": successes / seconds,
        "batch_size": args.batch_size.to_string(),
        "target_events": args.events.map(NonZeroU64::get),
        "event_limit_reached": args.events.map(|target| attempted_events == target.get()),
        "completed_events": event_snapshot.completed(),
        "failed_events": event_snapshot.failed,
        "in_flight_events": event_snapshot.in_flight(),
        "remaining_events": args.events.map(|target| target.get().saturating_sub(event_snapshot.completed())),
        "events_in_attempted_http_requests": attempted_events,
        "events_in_successful_http_responses": successful_events,
        "events_per_second_in_successful_http_responses": successful_events as f64 / seconds,
        "success_percent": if results.total_requests == 0 { 0.0 } else { successes * 100.0 / results.total_requests as f64 },
        "status_codes": statuses,
        "non_200_responses": non_200_responses,
        "non_200_percent": if responses == 0 { 0.0 } else { non_200_responses as f64 * 100.0 / responses as f64 },
        "non_200_status_codes": non_200_status_codes,
        "response_bytes": results.total_bytes,
        "latency_sample_count": results.total_requests,
        "latency_scope": "all_completed_requests",
        "latency_ms": {
            "min": results.latency_min.as_secs_f64() * 1000.0,
            "mean": results.latency_mean.as_secs_f64() * 1000.0,
            "p50": results.latency_p50.as_secs_f64() * 1000.0,
            "p90": results.latency_p90.as_secs_f64() * 1000.0,
            "p95": results.latency_p95.as_secs_f64() * 1000.0,
            "p99": results.latency_p99.as_secs_f64() * 1000.0,
            "max": results.latency_max.as_secs_f64() * 1000.0
        }
    })
}

async fn run(args: Args) -> Result<ExitCode> {
    let url = Url::parse(&args.url).context("invalid URL")?;
    ensure!(
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
        "URL must use http:// or https://"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none() && url.fragment().is_none(),
        "put credentials in the environment variable; URL must not contain userinfo or a fragment"
    );
    if let Some(rate) = args.rate {
        ensure!(rate.get() <= 1_000_000, "--rate must be at most 1000000");
    }
    ensure!(
        args.concurrency.get() <= 100_000,
        "--concurrency must be at most 100000"
    );
    if let Some(requests) = args.requests {
        ensure!(
            (requests.get() as u64)
                .checked_mul(args.batch_size.max() as u64)
                .is_some(),
            "total event count is too large"
        );
    }

    let mut fields = match &args.template {
        Some(path) => serde_json::from_slice::<FieldTemplates>(
            &fs::read(path).with_context(|| format!("reading {}", path.display()))?,
        )
        .context("template JSON must contain event, messageId and anonymousId strings")?,
        None => FieldTemplates::default(),
    };
    if let Some(event) = &args.event {
        fields.event.clone_from(event);
    }
    if let Some(message_id) = &args.message_id {
        fields.message_id.clone_from(message_id);
    }
    if let Some(anonymous_id) = &args.anonymous_id {
        fields.anonymous_id.clone_from(anonymous_id);
    }
    let factory = PayloadFactory::new(fields)?;
    if args.dry_run {
        let size = args.batch_size.choose();
        let size = args
            .events
            .map_or(size, |total| (size as u64).min(total.get()) as usize);
        println!(
            "{}",
            serde_json::to_string_pretty(&factory.render(0, size, Utc::now()))?
        );
        return Ok(ExitCode::SUCCESS);
    }

    let headers = HashMap::from([
        ("Content-Type".into(), "application/json".into()),
        ("Authorization".into(), authorization(&args.auth_env)?),
    ]);
    let batch_size = args.batch_size;
    let events = Arc::new(EventCounts::default());
    let sent_events = Arc::clone(&events);
    let finished_events = Arc::clone(&events);
    // Each worker has at most one in-flight request, so its batch size needs no lock.
    let worker_batches: Arc<Vec<AtomicUsize>> = Arc::new(
        (0..args.concurrency.get())
            .map(|_| AtomicUsize::new(0))
            .collect(),
    );
    let completed_batches = Arc::clone(&worker_batches);
    let reserved_batches = Arc::clone(&worker_batches);
    let event_budget = args
        .events
        .map(|total| Arc::new(EventBudget::new(total.get())));
    let completed_budget = event_budget.clone();
    let limited = event_budget.is_some();
    let target_url = url.to_string();
    let method = args.method;
    let mut benchmark = Benchmark::builder()
        .concurrency(args.concurrency.get())
        .timeout(args.timeout)
        .max_retries(0)
        .request_fn(move |ctx| {
            let size = if limited {
                // The before-request hook reserved these records after rate limiting.
                worker_batches[ctx.worker_id].load(Ordering::Relaxed)
            } else {
                batch_size.choose()
            };
            worker_batches[ctx.worker_id].store(size, Ordering::Relaxed);
            sent_events
                .attempted
                .fetch_add(size as u64, Ordering::Relaxed);
            // Struct serialization handles JSON escaping; this payload has no fallible map keys.
            let body = serde_json::to_vec(&factory.render(ctx.worker_id, size, Utc::now()))
                .expect("serializing a batch of strings is infallible");
            RequestConfig {
                url: target_url.clone(),
                method,
                headers: headers.clone(),
                body: Some(Bytes::from(body)),
            }
        })
        .after_request(move |ctx| {
            let count = completed_batches[ctx.worker_id].load(Ordering::Relaxed);
            if ctx
                .status
                .is_some_and(|status| (200..300).contains(&status))
            {
                finished_events
                    .successful
                    .fetch_add(count as u64, Ordering::Relaxed);
            } else {
                finished_events
                    .failed
                    .fetch_add(count as u64, Ordering::Relaxed);
            }
            if completed_budget
                .as_ref()
                .is_some_and(|budget| budget.is_exhausted())
            {
                HookAction::Stop
            } else {
                HookAction::Continue
            }
        });
    if let Some(budget) = event_budget {
        benchmark = benchmark.before_request(move |ctx| match budget.claim(batch_size.choose()) {
            Some(size) => {
                reserved_batches[ctx.worker_id].store(size, Ordering::Relaxed);
                HookAction::Continue
            }
            None => HookAction::Stop,
        });
    }
    benchmark = match args.duration {
        Some(duration) => benchmark.duration(duration),
        None if args.events.is_some() => benchmark,
        None => benchmark.requests(args.requests.map_or(1, NonZeroUsize::get)),
    };
    if let Some(rate) = args.rate {
        benchmark = benchmark.rate(rate.get());
    }
    let benchmark = benchmark.build()?;
    let mut report_files = args
        .output
        .iter()
        .map(|path| ReportFile::create(path))
        .collect::<Result<Vec<_>>>()?;
    let mut progress = EventProgress::new(
        args.events.map(NonZeroU64::get),
        args.progress,
        args.no_progress,
    );
    progress.update(events.snapshot());
    let period = Duration::from_millis(250);
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let run = benchmark.run();
    tokio::pin!(run);
    let result = loop {
        tokio::select! {
            biased;
            result = &mut run => break result,
            _ = tick.tick(), if progress.enabled() => progress.update(events.snapshot()),
        }
    };
    progress.finish(events.snapshot(), result.is_err());
    let results = result?;
    let summary = report(&results, &args, &events);
    for file in &mut report_files {
        file.save(&summary)?;
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        results.print();
        println!(
            "\nSuccessful HTTP requests/s: {:.2}\nEvents/s in successful HTTP responses: {:.2} (batch size {})",
            summary["successful_http_rps"].as_f64().unwrap_or_default(),
            summary["events_per_second_in_successful_http_responses"]
                .as_f64()
                .unwrap_or_default(),
            args.batch_size
        );
        println!("Transport errors: {}", summary["transport_errors"]);
        println!(
            "Non-200 responses: {} / {} ({:.2}% of HTTP responses)",
            summary["non_200_responses"],
            summary["http_responses"],
            summary["non_200_percent"].as_f64().unwrap_or_default()
        );
        println!("Non-200 status codes: {}", summary["non_200_status_codes"]);
        println!(
            "Event records: {} attempted, {} completed, {} successful, {} failed",
            summary["events_in_attempted_http_requests"],
            summary["completed_events"],
            summary["events_in_successful_http_responses"],
            summary["failed_events"]
        );
        if let Some(target) = args.events {
            println!(
                "Event target: {target}, reached: {}",
                summary["event_limit_reached"]
            );
        }
    }
    if results.total_requests == 0 {
        bail!("no requests completed");
    }
    let incomplete = args
        .events
        .is_some_and(|target| events.attempted.load(Ordering::Relaxed) != target.get());
    Ok(if results.failed_requests > 0 || incomplete {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn main() -> ExitCode {
    // Load before creating runtime threads; exported variables take precedence.
    if let Err(error) = dotenvy::dotenv()
        && !error.not_found()
    {
        // dotenv parser errors may contain the offending line, including a credential.
        eprintln!("Error: could not load .env; check its syntax and permissions");
        return ExitCode::from(2);
    }
    let args = Args::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("creating Tokio runtime");
    match runtime.block_on(run(args)) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("Error: {error:#}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpress::metrics::{Metrics, RequestResult};

    fn report_for_statuses(statuses: &[Option<u16>]) -> serde_json::Value {
        let mut metrics = Metrics::new();
        for &status in statuses {
            metrics.record(RequestResult {
                latency: Duration::from_millis(1),
                status,
                bytes: 0,
            });
        }
        let results = metrics.into_results(Duration::from_secs(1));
        let args = Args::try_parse_from([
            "batchbench",
            "--endpoint",
            "http://127.0.0.1:1/batch",
            "--method",
            "POST",
        ])
        .unwrap();
        report(&results, &args, &EventCounts::default())
    }

    #[test]
    fn non_200_counts_include_other_2xx_and_exclude_transport_errors() {
        let report = report_for_statuses(&[
            Some(200),
            Some(200),
            Some(201),
            Some(204),
            Some(304),
            Some(429),
            Some(429),
            Some(500),
            None,
        ]);
        assert_eq!(report["http_requests"], 9);
        assert_eq!(report["http_responses"], 8);
        assert_eq!(report["non_200_responses"], 6);
        assert_eq!(report["non_200_percent"], 75.0);
        assert_eq!(
            report["non_200_status_codes"],
            json!({
                "201": 1, "204": 1, "304": 1, "429": 2, "500": 1,
            })
        );
        assert_eq!(report["status_codes"]["200"], 2);
        assert_eq!(report["transport_errors"], 1);
        assert_eq!(report["successful_http_requests"], 4);
        assert_eq!(report["failed_http_requests"], 5);
    }

    #[test]
    fn all_200_and_no_response_runs_have_zero_non_200_counts() {
        for statuses in [&[Some(200), Some(200)][..], &[None, None], &[]] {
            let report = report_for_statuses(statuses);
            assert_eq!(report["non_200_responses"], 0);
            assert_eq!(report["non_200_percent"], 0.0);
            assert_eq!(report["non_200_status_codes"], json!({}));
            assert_eq!(
                report["transport_errors"].as_u64().unwrap(),
                statuses.iter().filter(|status| status.is_none()).count() as u64
            );
        }
    }

    #[test]
    fn non_200_details_are_preserved_in_json_and_csv_exports() {
        let report = report_for_statuses(&[Some(200), Some(201), Some(429), Some(500), None]);
        let directory =
            std::env::temp_dir().join(format!("batchbench-status-{}", uuid::Uuid::new_v4()));
        let json_path = directory.join("report.json");
        let csv_path = directory.join("report.csv");
        for path in [&json_path, &csv_path] {
            ReportFile::create(path).unwrap().save(&report).unwrap();
        }
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(&json_path).unwrap()).unwrap();
        assert_eq!(saved, report);
        let mut reader = csv::Reader::from_path(csv_path).unwrap();
        let rows: Vec<HashMap<String, String>> =
            reader.deserialize().collect::<Result<_, _>>().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["http_responses"], "4");
        assert_eq!(rows[0]["non_200_responses"], "3");
        assert_eq!(rows[0]["non_200_percent"].parse::<f64>().unwrap(), 75.0);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&rows[0]["non_200_status_codes"]).unwrap(),
            json!({"201": 1, "429": 1, "500": 1})
        );
        drop(reader);
        fs::remove_dir_all(directory).unwrap();
    }
}
