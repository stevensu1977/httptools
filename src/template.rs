use anyhow::{Result, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FieldTemplates {
    pub event: String,
    pub message_id: String,
    pub anonymous_id: String,
}

impl Default for FieldTemplates {
    fn default() -> Self {
        Self {
            event: "curl_ok".into(),
            message_id: "m-{{run_id}}-{{seq}}".into(),
            anonymous_id: "a-{{seq}}".into(),
        }
    }
}

enum Part {
    Literal(String),
    Seq,
    RequestSeq,
    BatchIndex,
    WorkerId,
    Uuid,
    RunId,
    TimestampMs,
}

struct Template(Vec<Part>);

impl Template {
    fn compile(mut input: &str) -> Result<Self> {
        let mut parts = Vec::new();
        while let Some(start) = input.find("{{") {
            let prefix = &input[..start];
            if prefix.contains("}}") {
                bail!("unmatched '}}}}' in template");
            }
            parts.push(Part::Literal(prefix.into()));
            input = &input[start + 2..];
            let Some(end) = input.find("}}") else {
                bail!("unclosed template variable");
            };
            parts.push(match input[..end].trim() {
                "seq" => Part::Seq,
                "request_seq" => Part::RequestSeq,
                "batch_index" => Part::BatchIndex,
                "worker_id" => Part::WorkerId,
                "uuid" => Part::Uuid,
                "run_id" => Part::RunId,
                "timestamp_ms" => Part::TimestampMs,
                unknown => bail!("unknown template variable: {unknown}"),
            });
            input = &input[end + 2..];
        }
        if input.contains("}}") {
            bail!("unmatched '}}}}' in template");
        }
        parts.push(Part::Literal(input.into()));
        Ok(Self(parts))
    }

    fn uses_uuid(&self) -> bool {
        self.0.iter().any(|part| matches!(part, Part::Uuid))
    }

    fn render(&self, ctx: &Context<'_>) -> String {
        use std::fmt::Write;
        let mut out = String::with_capacity(64);
        for part in &self.0 {
            match part {
                Part::Literal(s) => out.push_str(s),
                Part::Seq => write!(out, "{}", ctx.seq).unwrap(),
                Part::RequestSeq => write!(out, "{}", ctx.request_seq).unwrap(),
                Part::BatchIndex => write!(out, "{}", ctx.batch_index).unwrap(),
                Part::WorkerId => write!(out, "{}", ctx.worker_id).unwrap(),
                Part::Uuid => out.push_str(ctx.uuid),
                Part::RunId => out.push_str(ctx.run_id),
                Part::TimestampMs => write!(out, "{}", ctx.timestamp_ms).unwrap(),
            }
        }
        out
    }
}

struct Context<'a> {
    seq: u64,
    request_seq: u64,
    batch_index: usize,
    worker_id: usize,
    uuid: &'a str,
    run_id: &'a str,
    timestamp_ms: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Payload {
    batch: Vec<Event>,
    sent_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Event {
    #[serde(rename = "type")]
    kind: &'static str,
    event: String,
    message_id: String,
    anonymous_id: String,
}

pub struct PayloadFactory {
    event: Template,
    message_id: Template,
    anonymous_id: Template,
    run_id: String,
    uses_uuid: bool,
    next_event: AtomicU64,
    next_request: AtomicU64,
}

impl PayloadFactory {
    pub fn new(fields: FieldTemplates) -> Result<Self> {
        let event = Template::compile(&fields.event)?;
        let message_id = Template::compile(&fields.message_id)?;
        let anonymous_id = Template::compile(&fields.anonymous_id)?;
        let uses_uuid = event.uses_uuid() || message_id.uses_uuid() || anonymous_id.uses_uuid();
        Ok(Self {
            event,
            message_id,
            anonymous_id,
            run_id: Uuid::new_v4().to_string(),
            uses_uuid,
            next_event: AtomicU64::new(1),
            next_request: AtomicU64::new(1),
        })
    }

    pub fn render(&self, worker_id: usize, batch_size: usize, now: DateTime<Utc>) -> Payload {
        let request_seq = self.next_request.fetch_add(1, Ordering::Relaxed);
        let first_event = self
            .next_event
            .fetch_add(batch_size as u64, Ordering::Relaxed);
        let batch = (0..batch_size)
            .map(|i| {
                let uuid = if self.uses_uuid {
                    Uuid::new_v4().to_string()
                } else {
                    String::new()
                };
                let ctx = Context {
                    seq: first_event + i as u64,
                    request_seq,
                    batch_index: i + 1,
                    worker_id,
                    uuid: &uuid,
                    run_id: &self.run_id,
                    timestamp_ms: now.timestamp_millis(),
                };
                Event {
                    kind: "track",
                    event: self.event.render(&ctx),
                    message_id: self.message_id.render(&ctx),
                    anonymous_id: self.anonymous_id.render(&ctx),
                }
            })
            .collect();
        Payload {
            batch,
            sent_at: now.to_rfc3339_opts(SecondsFormat::Millis, true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn escapes_json_and_keeps_uuid_consistent_per_event() {
        let factory = PayloadFactory::new(FieldTemplates {
            event: "点击 \"购买\"\n{{seq}}".into(),
            message_id: "{{uuid}}".into(),
            anonymous_id: "{{uuid}}".into(),
        })
        .unwrap();
        let now = DateTime::parse_from_rfc3339("2026-10-15T08:00:00.123Z")
            .unwrap()
            .with_timezone(&Utc);
        let json = serde_json::to_vec(&factory.render(3, 2, now)).unwrap();
        let value: Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(value["sentAt"], "2026-10-15T08:00:00.123Z");
        assert_eq!(value["batch"][0]["event"], "点击 \"购买\"\n1");
        assert_eq!(value["batch"][1]["event"], "点击 \"购买\"\n2");
        assert_eq!(value["batch"][0]["type"], "track");
        assert_eq!(
            value["batch"][0]["messageId"],
            value["batch"][0]["anonymousId"]
        );
        assert_ne!(
            value["batch"][0]["messageId"],
            value["batch"][1]["messageId"]
        );
    }

    #[test]
    fn rejects_invalid_templates_before_sending() {
        for input in ["{{typo}}", "{{seq", "seq}}", "}}{{seq}}"] {
            assert!(Template::compile(input).is_err(), "{input}");
        }
    }

    #[test]
    fn run_id_and_sequence_prevent_duplicates_across_workers_and_runs() {
        let first = PayloadFactory::new(FieldTemplates::default()).unwrap();
        let second = PayloadFactory::new(FieldTemplates::default()).unwrap();
        let payloads = [
            first.render(0, 1, Utc::now()),
            first.render(1, 2, Utc::now()),
            second.render(0, 1, Utc::now()),
        ];
        let ids: std::collections::HashSet<_> =
            payloads.iter().map(|p| &p.batch[0].message_id).collect();
        assert_eq!(ids.len(), 3);
    }
}
