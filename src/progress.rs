use std::io::{self, IsTerminal};

use indicatif::{ProgressBar, ProgressStyle};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventSnapshot {
    pub attempted: u64,
    pub successful: u64,
    pub failed: u64,
}

impl EventSnapshot {
    pub fn completed(self) -> u64 {
        self.successful + self.failed
    }

    pub fn in_flight(self) -> u64 {
        self.attempted.saturating_sub(self.completed())
    }
}

enum Output {
    Hidden,
    Terminal(ProgressBar),
    Lines,
}

pub struct EventProgress {
    output: Output,
    target: Option<u64>,
    previous: Option<EventSnapshot>,
}

impl EventProgress {
    pub fn new(target: Option<u64>, force: bool, disable: bool) -> Self {
        let terminal = io::stderr().is_terminal();
        let output = if disable || (!terminal && !force) {
            Output::Hidden
        } else if terminal {
            Output::Terminal(
                ProgressBar::new_spinner()
                    .with_style(ProgressStyle::with_template("{msg}").unwrap()),
            )
        } else {
            Output::Lines
        };
        Self {
            output,
            target,
            previous: None,
        }
    }

    pub fn enabled(&self) -> bool {
        !matches!(self.output, Output::Hidden)
    }

    pub fn update(&mut self, snapshot: EventSnapshot) {
        if !self.enabled() || self.previous == Some(snapshot) {
            return;
        }
        self.previous = Some(snapshot);
        let line = format_progress(snapshot, self.target, "running");
        match &self.output {
            Output::Terminal(bar) => bar.set_message(line.replace(" | ", "\n")),
            Output::Lines => eprintln!("{line}"),
            Output::Hidden => {}
        }
    }

    pub fn finish(&self, snapshot: EventSnapshot, errored: bool) {
        let status = if errored {
            "error"
        } else if self
            .target
            .is_some_and(|target| snapshot.completed() < target)
        {
            "stopped"
        } else {
            "finished"
        };
        let line = format_progress(snapshot, self.target, status);
        match &self.output {
            Output::Terminal(bar) => bar.finish_with_message(line.replace(" | ", "\n")),
            Output::Lines => eprintln!("{line}"),
            Output::Hidden => {}
        }
    }
}

fn format_progress(snapshot: EventSnapshot, target: Option<u64>, status: &str) -> String {
    let completed = snapshot.completed();
    let totals = match target {
        Some(target) => {
            let percent = completed as f64 / target as f64 * 100.0;
            // Keep <100% until all responses finish, even when rounding to one decimal.
            let percent = if completed < target {
                percent.min(99.9)
            } else {
                percent
            };
            format!(
                "completed={completed}/{target} ({percent:.1}%) remaining={}",
                target.saturating_sub(completed)
            )
        }
        None => format!("completed={completed} attempted={}", snapshot.attempted),
    };
    let unsent = target.map_or(String::new(), |target| {
        format!(" unsent={}", target.saturating_sub(snapshot.attempted))
    });
    format!(
        "Events [{status}] {totals} | in_flight={}{unsent} successful={} failed={}",
        snapshot.in_flight(),
        snapshot.successful,
        snapshot.failed,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatched_events_are_not_completed_while_responses_are_pending() {
        let snapshot = EventSnapshot {
            attempted: 100,
            successful: 60,
            failed: 10,
        };
        let line = format_progress(snapshot, Some(100), "running");
        assert!(line.contains("completed=70/100 (70.0%) remaining=30"));
        assert!(line.contains("in_flight=30 unsent=0 successful=60 failed=10"));
    }

    #[test]
    fn percentage_does_not_round_to_complete_before_the_last_response() {
        let snapshot = EventSnapshot {
            attempted: 100_000,
            successful: 99_999,
            failed: 0,
        };
        let line = format_progress(snapshot, Some(100_000), "running");
        assert!(line.contains("(99.9%) remaining=1"));
    }

    #[test]
    fn unknown_total_does_not_invent_a_remaining_count() {
        let snapshot = EventSnapshot {
            attempted: 100,
            successful: 70,
            failed: 5,
        };
        let line = format_progress(snapshot, None, "running");
        assert!(line.contains("completed=75 attempted=100"));
        assert!(line.contains("in_flight=25"));
        assert!(!line.contains("remaining="));
        assert!(!line.contains('%'));
    }
}
