//! Progress reporting types and indicatif rendering.

use std::sync::Arc;
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

use crate::config::StopCondition;

/// Type alias for progress callback functions.
pub type ProgressFn = Arc<dyn Fn(ProgressSnapshot) + Send + Sync>;

/// Snapshot of benchmark state passed to progress callbacks.
#[derive(Debug, Clone)]
pub struct ProgressSnapshot {
    /// Total requests completed so far (success + failure).
    pub total_requests: usize,
    /// Successful requests so far (HTTP 2xx).
    pub successful_requests: usize,
    /// Failed requests so far (non-2xx or connection errors).
    pub failed_requests: usize,
    /// Time elapsed since benchmark start.
    pub elapsed: Duration,
    /// Current rolling requests-per-second (sampled over the last tick interval).
    pub current_rps: f64,
    /// Target request count, if using `StopCondition::Requests`.
    pub target_requests: Option<usize>,
    /// Target duration, if using `StopCondition::Duration`.
    pub target_duration: Option<Duration>,
}

/// Build an indicatif `ProgressBar` appropriate for the given stop condition.
pub fn create_progress_bar(stop_condition: &StopCondition) -> ProgressBar {
    match stop_condition {
        StopCondition::Duration(d) => {
            let pb = ProgressBar::new(d.as_millis() as u64);
            pb.set_style(
                ProgressStyle::with_template("[{bar:40.green/white}] {msg}")
                    .unwrap()
                    .progress_chars("█░"),
            );
            pb
        }
        StopCondition::Requests(n) => {
            let pb = ProgressBar::new(*n as u64);
            pb.set_style(
                ProgressStyle::with_template("[{bar:40.green/white}] {msg}")
                    .unwrap()
                    .progress_chars("█░"),
            );
            pb
        }
        StopCondition::Infinite => {
            let pb = ProgressBar::new_spinner();
            pb.set_style(ProgressStyle::with_template("{spinner}  {msg}").unwrap());
            pb.enable_steady_tick(Duration::from_millis(100));
            pb
        }
    }
}

/// Update an indicatif `ProgressBar` from a `ProgressSnapshot`.
pub fn update_progress_bar(pb: &ProgressBar, snap: &ProgressSnapshot) {
    let success_pct = if snap.total_requests > 0 {
        snap.successful_requests as f64 / snap.total_requests as f64 * 100.0
    } else {
        100.0
    };

    match (snap.target_requests, snap.target_duration) {
        (Some(target), _) => {
            pb.set_position(snap.total_requests.min(target) as u64);
            pb.set_message(format!(
                "{:>8} / {:<8}  {:>8.0} req/s  {:.1}% ok",
                snap.total_requests, target, snap.current_rps, success_pct,
            ));
        }
        (_, Some(target)) => {
            pb.set_position(snap.elapsed.as_millis().min(target.as_millis()) as u64);
            pb.set_message(format!(
                "{} / {}  {:>8.0} req/s  {:.1}% ok",
                format_duration(snap.elapsed),
                format_duration(target),
                snap.current_rps,
                success_pct,
            ));
        }
        _ => {
            pb.set_message(format!(
                "{}  {:>8.0} req/s  {:.1}% ok",
                format_duration(snap.elapsed),
                snap.current_rps,
                success_pct,
            ));
        }
    }
}

fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    let mins = secs / 60;
    let secs = secs % 60;
    if mins > 0 {
        format!("{:02}m{:02}s", mins, secs)
    } else {
        format!("{:02}s", secs)
    }
}
