use std::{
    collections::HashSet,
    process::{Command, Output},
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::Request,
    http::{HeaderMap, Method, Response, StatusCode},
    routing::any,
};
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::{net::TcpListener, task::JoinHandle};

struct Seen {
    method: Method,
    uri: String,
    headers: HeaderMap,
    body: Bytes,
}

struct Server {
    url: String,
    requests: Arc<Mutex<Vec<Seen>>>,
    task: JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn server(status: u16, location: Option<String>, slow_body: bool) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let app = Router::new().fallback(any(move |req: Request| {
        let captured = Arc::clone(&captured);
        let location = location.clone();
        async move {
            let final_path = req.uri().path() == "/final";
            let truncated = req.uri().path() == "/truncated";
            let (parts, body) = req.into_parts();
            let body = to_bytes(body, 1024 * 1024).await.unwrap();
            captured.lock().unwrap().push(Seen {
                method: parts.method,
                uri: parts.uri.to_string(),
                headers: parts.headers,
                body,
            });
            let mut response = Response::builder().status(if final_path { 200 } else { status });
            if !final_path && let Some(location) = location {
                response = response.header("Location", location);
            }
            if truncated {
                let stream = futures_util::stream::once(async {
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    Err::<Bytes, _>(std::io::Error::other("simulated body failure"))
                });
                response
                    .header("Content-Length", "10")
                    .body(Body::from_stream(stream))
                    .unwrap()
            } else if slow_body {
                let stream = futures_util::stream::once(async {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    Ok::<_, std::io::Error>(Bytes::from_static(b"{}"))
                });
                response.body(Body::from_stream(stream)).unwrap()
            } else {
                response.body(Body::from("{}")).unwrap()
            }
        }
    }));
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Server {
        url,
        requests,
        task,
    }
}

async fn cli(url: &str, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_batchbench"));
    command
        .args(["--url", url, "--json"])
        .args(args)
        .env("ACCESS_TOKEN", "dXNlcjpwYXNz")
        .env("METHOD", "POST");
    run_cli(command).await
}

async fn run_cli(mut command: Command) -> Output {
    tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || command.output().unwrap()),
    )
    .await
    .expect("CLI should terminate within 10 seconds")
    .unwrap()
}

#[tokio::test]
async fn reads_endpoint_and_method_from_dotenv_and_honors_overrides() {
    let server = server(200, None, false).await;
    let directory =
        std::env::temp_dir().join(format!("batchbench-target-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(
        directory.join(".env"),
        format!(
            "ACCESS_TOKEN=dXNlcjpwYXNz\nENDPOINT={}/dotenv?source=file\nMETHOD=pAtCh\n",
            server.url
        ),
    )
    .unwrap();
    for mode in ["dotenv", "environment", "flags"] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_batchbench"));
        command
            .current_dir(&directory)
            .env_remove("ACCESS_TOKEN")
            .env_remove("ENDPOINT")
            .env_remove("METHOD")
            .args(["--json", "--batch-size", "2"]);
        if mode == "environment" {
            command
                .env(
                    "ENDPOINT",
                    format!("{}/environment?source=shell", server.url),
                )
                .env("METHOD", "PUT");
        } else if mode == "flags" {
            // Invalid environment values must not defeat explicit command-line flags.
            command
                .env("ENDPOINT", "not-a-url")
                .env("METHOD", "not-a-method")
                .args([
                    "--endpoint",
                    &format!("{}/flags?source=cli", server.url),
                    "--method",
                    "delete",
                ]);
        }
        let output = run_cli(command).await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    std::fs::remove_dir_all(directory).unwrap();
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    for (request, (method, uri)) in requests.iter().zip([
        (Method::PATCH, "/dotenv?source=file"),
        (Method::PUT, "/environment?source=shell"),
        (Method::DELETE, "/flags?source=cli"),
    ]) {
        assert_eq!(request.method, method);
        assert_eq!(request.uri, uri);
        assert_eq!(request.headers["authorization"], "Basic dXNlcjpwYXNz");
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["batch"].as_array().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn method_flag_changes_the_wire_method_and_preserves_the_batch_body() {
    let server = server(200, None, false).await;
    let methods = ["GET", "post", "PUT", "pAtCh", "DELETE", "HEAD", "OPTIONS"];
    for method in methods {
        let output = cli(&server.url, &["--method", method, "--batch-size", "1"]).await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let requests = server.requests.lock().unwrap();
    for (request, method) in requests.iter().zip(methods) {
        assert_eq!(request.method.as_str(), method.to_ascii_uppercase());
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["batch"].as_array().unwrap().len(), 1);
    }
    assert_eq!(requests.len(), methods.len());
}

fn summary(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout must be JSON: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

#[tokio::test]
async fn live_progress_counts_responses_not_just_dispatched_events() {
    let server = server(200, None, true).await;
    let output = cli(
        &server.url,
        &[
            "--events",
            "23",
            "--batch-size",
            "10",
            "-c",
            "1",
            "--progress",
        ],
    )
    .await;
    assert!(output.status.success());
    let report = summary(&output); // stdout remains a standalone JSON document.
    assert_eq!(report["completed_events"], 23);
    assert_eq!(report["remaining_events"], 0);
    assert_eq!(report["in_flight_events"], 0);
    let stderr = String::from_utf8(output.stderr).unwrap();
    let lines: Vec<_> = stderr
        .lines()
        .filter(|line| line.starts_with("Events "))
        .collect();
    assert!(lines.len() >= 3, "{stderr}");
    assert!(
        lines.iter().any(|line| {
            line.contains("[running]")
                && line.contains("completed=0/23")
                && line.contains("remaining=23")
                && line.contains("in_flight=10")
        }),
        "{stderr}"
    );
    assert!(lines.last().unwrap().contains(
        "[finished] completed=23/23 (100.0%) remaining=0 | in_flight=0 unsent=0 successful=23 failed=0"
    ));
    assert!(!stderr.contains("dXNlcjpwYXNz"));
}

#[tokio::test]
async fn progress_includes_failures_and_can_be_disabled() {
    let server = server(429, None, false).await;
    let output = cli(
        &server.url,
        &["--events", "23", "--batch-size", "10", "--progress"],
    )
    .await;
    assert_eq!(output.status.code(), Some(1));
    let report = summary(&output);
    assert_eq!(report["completed_events"], 23);
    assert_eq!(report["failed_events"], 23);
    assert_eq!(report["remaining_events"], 0);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(
        "[finished] completed=23/23 (100.0%) remaining=0 | in_flight=0 unsent=0 successful=0 failed=23"
    ), "{stderr}");
    let silent = cli(&server.url, &["--events", "3", "--no-progress"]).await;
    assert!(!String::from_utf8_lossy(&silent.stderr).contains("Events ["));
    let auto = cli(&server.url, &["--events", "3"]).await;
    assert!(!String::from_utf8_lossy(&auto.stderr).contains("Events ["));
}

#[tokio::test]
async fn event_total_is_exact_with_concurrent_fixed_and_random_batches() {
    for (total, size) in [
        ("1", "10"),
        ("7", "10"),
        ("103", "10"),
        ("1003", "1-10"),
        ("103", "5-10"),
    ] {
        let server = server(200, None, false).await;
        let output = cli(
            &server.url,
            &[
                "--events",
                total,
                "--batch-size",
                size,
                "-c",
                "128",
                "--event",
                "{{seq}}",
            ],
        )
        .await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let total = total.parse::<usize>().unwrap();
        let report = summary(&output);
        assert_eq!(report["target_events"], total);
        assert_eq!(report["event_limit_reached"], true);
        assert_eq!(report["events_in_attempted_http_requests"], total);
        assert_eq!(report["events_in_successful_http_responses"], total);
        assert_eq!(report["failed_http_requests"], 0);
        let requests = server.requests.lock().unwrap();
        assert_eq!(report["http_requests"], requests.len());
        assert_eq!(report["latency_sample_count"], requests.len());
        let mut seqs = HashSet::new();
        let mut lengths = Vec::new();
        for req in requests.iter() {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            let batch = body["batch"].as_array().unwrap();
            assert!((1..=10).contains(&batch.len()));
            lengths.push(batch.len());
            for event in batch {
                assert!(seqs.insert(event["event"].as_str().unwrap().parse::<usize>().unwrap()));
            }
        }
        assert_eq!(lengths.iter().sum::<usize>(), total);
        assert_eq!(seqs, (1..=total).collect());
        if size == "10" {
            assert_eq!(requests.len(), total.div_ceil(10));
            assert_eq!(
                lengths.iter().filter(|&&size| size < 10).count(),
                usize::from(!total.is_multiple_of(10))
            );
        } else if size == "5-10" {
            assert!(lengths.iter().filter(|&&size| size < 5).count() <= 1);
        }
    }
}

#[tokio::test]
async fn reaching_event_limit_wakes_workers_waiting_for_rate_slots() {
    let server = server(200, None, false).await;
    let start = std::time::Instant::now();
    let output = cli(
        &server.url,
        &[
            "--events",
            "3",
            "--batch-size",
            "10",
            "-c",
            "128",
            "--rate",
            "1",
        ],
    )
    .await;
    assert!(output.status.success());
    assert!(start.elapsed() < Duration::from_secs(2));
    let report = summary(&output);
    assert_eq!(report["http_requests"], 1);
    assert_eq!(report["failed_http_requests"], 0);
    assert_eq!(report["events_in_attempted_http_requests"], 3);
    assert_eq!(report["event_limit_reached"], true);
}

#[tokio::test]
async fn failed_requests_consume_event_budget_without_replacement() {
    let server = server(429, None, false).await;
    for url in [server.url.as_str(), "http://127.0.0.1:1"] {
        let output = cli(url, &["--events", "23", "--batch-size", "10", "-c", "32"]).await;
        assert_eq!(output.status.code(), Some(1));
        let report = summary(&output);
        assert_eq!(report["event_limit_reached"], true);
        assert_eq!(report["events_in_attempted_http_requests"], 23);
        assert_eq!(report["events_in_successful_http_responses"], 0);
        assert_eq!(report["failed_http_requests"], 3);
        assert_eq!(report["http_requests"], 3);
    }
    assert_eq!(server.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn event_limit_validates_conflicts_and_trims_preview() {
    let server = server(200, None, false).await;
    for args in [
        vec!["--events", "0"],
        vec!["--events", "10", "--requests", "3"],
        vec!["--events", "10", "--duration", "1s"],
    ] {
        assert_eq!(cli(&server.url, &args).await.status.code(), Some(2));
    }
    let output = cli(
        &server.url,
        &["--dry-run", "--events", "3", "--batch-size", "10"],
    )
    .await;
    assert!(output.status.success());
    assert_eq!(summary(&output)["batch"].as_array().unwrap().len(), 3);
    assert!(server.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn saves_consistent_json_and_csv_reports_even_when_requests_fail() {
    let directory =
        std::env::temp_dir().join(format!("batchbench-reports-{}", uuid::Uuid::new_v4()));
    for status in [200, 201, 429] {
        let server = server(status, None, false).await;
        let json_path = directory.join(format!("{status}.json"));
        let csv_path = directory.join(format!("{status}.csv"));
        let output = cli(
            &server.url,
            &[
                "-n",
                "3",
                "--output",
                json_path.to_str().unwrap(),
                "--output",
                csv_path.to_str().unwrap(),
            ],
        )
        .await;
        assert_eq!(
            output.status.code(),
            Some(if (200..300).contains(&status) { 0 } else { 1 })
        );
        let report = summary(&output);
        let original = std::fs::read(&json_path).unwrap();
        let saved: Value = serde_json::from_slice(&original).unwrap();
        assert_eq!(saved, report);
        assert_eq!(report["latency_sample_count"], 3);
        assert_eq!(report["latency_scope"], "all_completed_requests");
        assert_eq!(report["http_responses"], 3);
        assert_eq!(
            report["non_200_responses"],
            if status == 200 { 0 } else { 3 }
        );
        assert_eq!(
            report["non_200_percent"],
            if status == 200 { 0.0 } else { 100.0 }
        );
        if status == 200 {
            assert_eq!(report["non_200_status_codes"], serde_json::json!({}));
        } else {
            assert_eq!(report["non_200_status_codes"][status.to_string()], 3);
        }
        let mut reader = csv::Reader::from_path(&csv_path).unwrap();
        let rows: Vec<std::collections::HashMap<String, String>> =
            reader.deserialize().collect::<Result<_, _>>().unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        for metric in ["min", "mean", "p50", "p90", "p95", "p99", "max"] {
            let csv_latency = row[&format!("latency_{metric}_ms")].parse::<f64>().unwrap();
            let json_latency = report["latency_ms"][metric].as_f64().unwrap();
            // JSON and CSV parsers can differ by one floating-point rounding step.
            assert!(
                (csv_latency - json_latency).abs() <= 1e-12 * json_latency.abs().max(1.0),
                "{metric}: CSV {csv_latency}, JSON {json_latency}"
            );
        }
        assert_eq!(
            serde_json::from_str::<Value>(&row["status_codes"]).unwrap(),
            report["status_codes"]
        );
        assert_eq!(
            serde_json::from_str::<Value>(&row["non_200_status_codes"]).unwrap(),
            report["non_200_status_codes"]
        );
        assert_eq!(row["http_requests"], "3");
        // Reusing a destination fails before issuing additional HTTP requests.
        let retry = cli(&server.url, &["--output", json_path.to_str().unwrap()]).await;
        assert_eq!(retry.status.code(), Some(2));
        assert_eq!(server.requests.lock().unwrap().len(), 3);
        assert_eq!(std::fs::read(&json_path).unwrap(), original);
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn concurrent_posts_have_unique_ids_current_time_and_exact_event_counts() {
    let server = server(200, None, false).await;
    let start = Utc::now();
    let output = cli(
        &server.url,
        &["-n", "50", "-c", "8", "--event", "点击 \"购买\"\n{{seq}}"],
    )
    .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let end = Utc::now();
    let report = summary(&output);
    assert_eq!(report["http_requests"], 50);
    assert_eq!(report["successful_http_requests"], 50);
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 50);
    let mut ids = HashSet::new();
    let mut seqs = HashSet::new();
    let mut count = 0;
    for req in requests.iter() {
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.headers["authorization"], "Basic dXNlcjpwYXNz");
        assert_eq!(req.headers["content-type"], "application/json");
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let sent_at = DateTime::parse_from_rfc3339(body["sentAt"].as_str().unwrap()).unwrap();
        assert!(sent_at.timestamp_millis() >= start.timestamp_millis());
        assert!(sent_at <= end);
        let batch = body["batch"].as_array().unwrap();
        assert!((1..=10).contains(&batch.len()));
        count += batch.len();
        for event in batch {
            assert_eq!(event["type"], "track");
            assert!(ids.insert(event["messageId"].as_str().unwrap().to_string()));
            let seq = event["event"]
                .as_str()
                .unwrap()
                .strip_prefix("点击 \"购买\"\n")
                .unwrap()
                .parse::<usize>()
                .unwrap();
            assert!(seqs.insert(seq));
        }
    }
    assert_eq!(seqs, (1..=count).collect());
    assert_eq!(report["events_in_successful_http_responses"], count);
    assert_eq!(report["events_in_attempted_http_requests"], count);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("dXNlcjpwYXNz"));
}

#[tokio::test]
async fn redirects_preserve_or_change_post_like_curl() {
    for status in [301, 302, 303, 307, 308] {
        let server = server(status, Some("/final".into()), false).await;
        let output = cli(&server.url, &["--batch-size", "1"]).await;
        assert!(output.status.success());
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].headers["authorization"], "Basic dXNlcjpwYXNz");
        if status == 307 || status == 308 {
            assert_eq!(requests[1].method, Method::POST);
            assert_eq!(requests[1].body, requests[0].body);
        } else {
            assert_eq!(requests[1].method, Method::GET);
            assert!(requests[1].body.is_empty());
        }
    }
}

#[tokio::test]
async fn redirects_do_not_forward_credentials_to_other_origins() {
    let target = server(200, None, false).await;
    let source = server(307, Some(target.url.clone()), false).await;
    assert!(cli(&source.url, &[]).await.status.success());
    let received = target.requests.lock().unwrap();
    assert_eq!(received[0].method, Method::POST);
    assert!(!received[0].headers.contains_key("authorization"));
}

#[tokio::test]
async fn http_failures_and_body_timeouts_do_not_count_as_success() {
    for status in [401, 429, 500] {
        let server = server(status, None, false).await;
        let output = cli(&server.url, &["--batch-size", "10"]).await;
        assert_eq!(output.status.code(), Some(1));
        let report = summary(&output);
        assert_eq!(report["status_codes"][status.to_string()], 1);
        assert_eq!(report["events_in_successful_http_responses"], 0);
        assert_eq!(report["events_in_attempted_http_requests"], 10);
    }
    let slow = server(200, None, true).await;
    let output = cli(&slow.url, &["--timeout", "100ms"]).await;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(summary(&output)["transport_errors"], 1);
    let truncated = server(200, None, false).await;
    let output = cli(&format!("{}/truncated", truncated.url), &[]).await;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(summary(&output)["transport_errors"], 1);
}

#[tokio::test]
async fn reads_access_token_from_dotenv_without_an_exported_variable() {
    let server = server(200, None, false).await;
    let directory = std::env::temp_dir().join(format!("batchbench-env-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(
        directory.join(".env"),
        "ACCESS_TOKEN='Basic dXNlcjpwYXNz'\n",
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_batchbench"));
    command
        .current_dir(&directory)
        .env_remove("ACCESS_TOKEN")
        .env_remove("ENDPOINT")
        .env_remove("METHOD")
        .args(["--url", &server.url, "--json"]);
    let output = tokio::task::spawn_blocking(move || command.output().unwrap())
        .await
        .unwrap();
    std::fs::remove_dir_all(&directory).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(server.requests.lock().unwrap()[0].method, Method::POST);
    assert_eq!(
        server.requests.lock().unwrap()[0].headers["authorization"],
        "Basic dXNlcjpwYXNz"
    );
}

#[tokio::test]
async fn low_rate_does_not_burst_or_wait_past_sending_deadline() {
    let server = server(200, None, false).await;
    let start = std::time::Instant::now();
    let output = cli(
        &server.url,
        &["--duration", "250ms", "--rate", "2", "-c", "20"],
    )
    .await;
    assert!(output.status.success());
    assert_eq!(summary(&output)["http_requests"], 1);
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn machine_time_is_refreshed_for_each_request() {
    let server = server(200, None, false).await;
    assert!(
        cli(&server.url, &["-n", "3", "--rate", "5"])
            .await
            .status
            .success()
    );
    let requests = server.requests.lock().unwrap();
    let first: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let last: Value = serde_json::from_slice(&requests[2].body).unwrap();
    let parse = |v: &Value| DateTime::parse_from_rfc3339(v["sentAt"].as_str().unwrap()).unwrap();
    assert!((parse(&last) - parse(&first)).num_milliseconds() >= 300);
}

#[tokio::test]
async fn preview_needs_no_token_and_invalid_configuration_sends_nothing() {
    let server = server(200, None, false).await;
    let output = cli(
        &server.url,
        &[
            "--dry-run",
            "--auth-env",
            "BATCHBENCH_MISSING_TEST_TOKEN",
            "--batch-size",
            "1",
        ],
    )
    .await;
    assert!(output.status.success());
    assert_eq!(summary(&output)["batch"].as_array().unwrap().len(), 1);
    for args in [
        vec!["--event", "{{typo}}"],
        vec!["--batch-size", "10-1"],
        vec!["--concurrency", "0"],
        vec!["--rate", "0"],
        vec!["--auth-env", "BATCHBENCH_MISSING_TEST_TOKEN"],
    ] {
        assert_eq!(cli(&server.url, &args).await.status.code(), Some(2));
    }
    assert!(server.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn redirect_loop_is_a_failure() {
    let server = server(
        StatusCode::TEMPORARY_REDIRECT.as_u16(),
        Some("/".into()),
        false,
    )
    .await;
    let output = cli(&server.url, &[]).await;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(summary(&output)["transport_errors"], 1);
}
