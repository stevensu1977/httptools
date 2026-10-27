mod common;

use common::TestServer;
use httpress::metrics::{Metrics, RequestResult};
use httpress::{Benchmark, HookAction};
use std::time::Duration;

#[test]
fn odd_sample_median_and_small_sample_tails_use_nearest_rank() {
    let mut metrics = Metrics::new();
    for milliseconds in [30, 10, 20] {
        metrics.record(RequestResult {
            latency: Duration::from_millis(milliseconds),
            status: Some(200),
            bytes: 0,
        });
    }
    let results = metrics.into_results(Duration::from_secs(1));
    assert_eq!(results.latency_p50, Duration::from_millis(20));
    assert_eq!(results.latency_p90, Duration::from_millis(30));
    assert_eq!(results.latency_p95, Duration::from_millis(30));
    assert_eq!(results.latency_p99, Duration::from_millis(30));
}

#[test]
fn slow_failure_is_included_in_tail_latency_statistics() {
    let mut metrics = Metrics::new();
    for _ in 0..4 {
        metrics.record(RequestResult {
            latency: Duration::from_millis(10),
            status: Some(200),
            bytes: 0,
        });
    }
    metrics.record(RequestResult {
        latency: Duration::from_millis(1000),
        status: None,
        bytes: 0,
    });
    let results = metrics.into_results(Duration::from_secs(2));
    assert_eq!(results.successful_requests, 4);
    assert_eq!(results.failed_requests, 1);
    assert_eq!(results.latency_p50, Duration::from_millis(10));
    assert_eq!(results.latency_p90, Duration::from_millis(1000));
    assert_eq!(results.latency_p95, Duration::from_millis(1000));
    assert_eq!(results.latency_p99, Duration::from_millis(1000));
}

#[tokio::test]
async fn test_bytes_nonzero_for_successful_requests() {
    let server = TestServer::start().await;

    let results = Benchmark::builder()
        .url(&format!("{}/ok", server.base_url))
        .requests(100)
        .concurrency(4)
        .build()
        .unwrap()
        .run()
        .await
        .unwrap();

    assert_eq!(results.successful_requests, 100);
    // "/ok" returns "OK" = 2 bytes
    assert!(
        results.total_bytes >= 100 * 2,
        "expected at least {} bytes, got {}",
        100 * 2,
        results.total_bytes
    );
}

#[tokio::test]
async fn test_bytes_zero_for_failed_requests() {
    // Use a port that is not listening
    let results = Benchmark::builder()
        .url("http://127.0.0.1:1")
        .requests(5)
        .concurrency(1)
        .build()
        .unwrap()
        .run()
        .await
        .unwrap();

    assert_eq!(results.successful_requests, 0);
    assert_eq!(results.total_bytes, 0);
}

#[tokio::test]
async fn test_bytes_scale_with_request_count() {
    let server = TestServer::start().await;

    let results = Benchmark::builder()
        .url(&format!("{}/body/1000", server.base_url))
        .requests(10)
        .concurrency(1)
        .build()
        .unwrap()
        .run()
        .await
        .unwrap();

    assert_eq!(results.successful_requests, 10);
    assert_eq!(
        results.total_bytes,
        10 * 1000,
        "expected exactly {} bytes, got {}",
        10 * 1000,
        results.total_bytes
    );
}

#[tokio::test]
async fn test_bytes_zero_for_aborted_hooks() {
    let server = TestServer::start().await;

    let results = Benchmark::builder()
        .url(&format!("{}/ok", server.base_url))
        .requests(10)
        .concurrency(2)
        .before_request(|_ctx| HookAction::Abort)
        .build()
        .unwrap()
        .run()
        .await
        .unwrap();

    assert_eq!(results.successful_requests, 0);
    assert_eq!(results.total_bytes, 0);
}
