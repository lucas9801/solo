//! SOLO: GitHub Copilot CLI's model requests, from the OpenTelemetry file it
//! writes when `COPILOT_OTEL_FILE_EXPORTER_PATH` names one
//! (<https://docs.github.com/en/copilot/reference/enterprise-administrators/opentelemetry-reference>):
//! every signal, one JSON object per line.
//!
//! Each model request is a `chat` span (named `chat <model>`) with
//! `gen_ai.response.time_to_first_chunk` (seconds, streamed requests only)
//! and `gen_ai.usage.output_tokens`. The documentation doesn't give the
//! lines' shape, so spans are looked for in both OTLP's JSON
//! (`resourceSpans`, attributes as `{key, value}` lists, times in
//! nanoseconds) and OpenTelemetry JS's own (attributes as an object, times
//! as `[seconds, nanoseconds]`). Without a sample from Copilot itself this
//! is unverified; whatever doesn't fit is skipped.

use std::time::Duration;

use serde_json::Value;

use crate::ResponseTiming;

const TIME_TO_FIRST_CHUNK: &str = "gen_ai.response.time_to_first_chunk";
const OUTPUT_TOKENS: &str = "gen_ai.usage.output_tokens";

/// The model requests in one line of the file: each one's span id (when it
/// has one) and timing.
pub fn parse_line(line: &str) -> Vec<(Option<String>, ResponseTiming)> {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let mut spans = Vec::new();
    find_chat_spans(&value, 0, &mut spans);
    spans.into_iter().filter_map(chat_timing).collect()
}

fn find_chat_spans<'a>(value: &'a Value, depth: usize, spans: &mut Vec<&'a Value>) {
    if depth > 8 {
        return;
    }
    match value {
        Value::Object(map) => {
            let is_chat = map
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| name == "chat" || name.starts_with("chat "));
            if is_chat && map.contains_key("attributes") {
                spans.push(value);
                return;
            }
            for child in map.values() {
                find_chat_spans(child, depth + 1, spans);
            }
        }
        Value::Array(values) => {
            for child in values {
                find_chat_spans(child, depth + 1, spans);
            }
        }
        _ => {}
    }
}

fn chat_timing(span: &Value) -> Option<(Option<String>, ResponseTiming)> {
    let first_chunk = attribute(span, TIME_TO_FIRST_CHUNK)
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.)
        .map(Duration::from_secs_f64);
    let output_tokens = attribute(span, OUTPUT_TOKENS)
        .filter(|tokens| *tokens >= 0. && tokens.fract() == 0.)
        .map(|tokens| tokens as u64);
    let duration = span_duration(span);
    let generation = first_chunk
        .zip(duration)
        .and_then(|(first_chunk, duration)| duration.checked_sub(first_chunk));
    if first_chunk.is_none() && generation.is_none() {
        return None;
    }
    let id = ["spanId", "span_id", "id"]
        .iter()
        .find_map(|name| span.get(*name).and_then(Value::as_str))
        .or_else(|| span.pointer("/spanContext/spanId").and_then(Value::as_str))
        .map(str::to_string);
    Some((
        id,
        ResponseTiming {
            first_token: first_chunk,
            output_tokens,
            generation,
        },
    ))
}

/// A numeric attribute, from an object of attributes or OTLP's list.
fn attribute(span: &Value, key: &str) -> Option<f64> {
    match span.get("attributes")? {
        Value::Object(map) => number(map.get(key)?),
        Value::Array(list) => list
            .iter()
            .find(|attribute| attribute.get("key").and_then(Value::as_str) == Some(key))
            .and_then(|attribute| {
                let value = attribute.get("value")?;
                ["doubleValue", "intValue", "stringValue"]
                    .iter()
                    .find_map(|kind| value.get(*kind))
                    .and_then(number)
                    .or_else(|| number(value))
            }),
        _ => None,
    }
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

/// How long the span took: from OTLP's start and end in nanoseconds, or
/// OpenTelemetry JS's `[seconds, nanoseconds]` times or duration.
fn span_duration(span: &Value) -> Option<Duration> {
    let nanos = |name: &str| -> Option<u128> {
        match span.get(name)? {
            Value::String(text) => text.parse().ok(),
            Value::Number(number) => number.as_u64().map(u128::from),
            _ => None,
        }
    };
    if let (Some(start), Some(end)) = (nanos("startTimeUnixNano"), nanos("endTimeUnixNano")) {
        let nanos = u64::try_from(end.checked_sub(start)?).ok()?;
        return Some(Duration::from_nanos(nanos));
    }
    let hrtime = |name: &str| -> Option<Duration> {
        let parts = span.get(name)?.as_array()?;
        let [seconds, nanos] = parts.as_slice() else {
            return None;
        };
        Some(Duration::new(
            seconds.as_u64()?,
            u32::try_from(nanos.as_u64()?).ok()?,
        ))
    };
    if let Some(duration) = hrtime("duration") {
        return Some(duration);
    }
    hrtime("endTime")?.checked_sub(hrtime("startTime")?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_otlp_json_spans() {
        // Made up, in OTLP's JSON encoding.
        let line = r#"{"resourceSpans":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"github-copilot"}}]},"scopeSpans":[{"spans":[
            {"traceId":"t1","spanId":"s1","name":"chat gpt-5","startTimeUnixNano":"1793540676000000000","endTimeUnixNano":"1793540686500000000","attributes":[
                {"key":"gen_ai.response.time_to_first_chunk","value":{"doubleValue":2.5}},
                {"key":"gen_ai.usage.output_tokens","value":{"intValue":"1200"}},
                {"key":"gen_ai.conversation.id","value":{"stringValue":"c1"}}]},
            {"traceId":"t1","spanId":"s2","name":"execute_tool bash","startTimeUnixNano":"1","endTimeUnixNano":"2","attributes":[]}
        ]}]}]}"#;
        let line = line.replace('\n', "");
        let spans = parse_line(&line);
        assert_eq!(spans.len(), 1);
        let (id, timing) = &spans[0];
        assert_eq!(id.as_deref(), Some("s1"));
        assert_eq!(timing.first_token(), Some(Duration::from_millis(2500)));
        assert_eq!(timing.generation, Some(Duration::from_secs(8)));
        assert_eq!(timing.speed(), Some(150.));
    }

    #[test]
    fn test_js_spans() {
        let line = r#"{"name":"chat claude-sonnet-5","spanContext":{"traceId":"t","spanId":"s9"},"startTime":[1793540676,0],"endTime":[1793540680,500000000],"attributes":{"gen_ai.response.time_to_first_chunk":0.5,"gen_ai.usage.output_tokens":800}}"#;
        let spans = parse_line(line);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].0.as_deref(), Some("s9"));
        assert_eq!(spans[0].1.speed(), Some(200.));

        let duration_only = r#"{"name":"chat","attributes":{"gen_ai.response.time_to_first_chunk":"1.25"},"duration":[3,0]}"#;
        let spans = parse_line(duration_only);
        assert_eq!(spans[0].1.first_token(), Some(Duration::from_millis(1250)));
        assert_eq!(spans[0].1.speed(), None);
    }

    #[test]
    fn test_what_doesnt_fit_is_skipped() {
        for line in [
            "",
            "not json",
            r#"{"resourceMetrics":[]}"#,
            // Not streamed: no first chunk, and so no speed either.
            r#"{"name":"chat gpt-5","attributes":{"gen_ai.usage.output_tokens":800},"duration":[3,0]}"#,
            r#"{"name":"chat gpt-5","attributes":{"gen_ai.response.time_to_first_chunk":-1}}"#,
            r#"{"name":"chatter","attributes":{"gen_ai.response.time_to_first_chunk":1}}"#,
        ] {
            assert!(parse_line(line).is_empty(), "{line}");
        }
        // The first chunk after the end: no speed.
        let late = r#"{"name":"chat","attributes":{"gen_ai.response.time_to_first_chunk":5,"gen_ai.usage.output_tokens":800},"duration":[3,0]}"#;
        let spans = parse_line(late);
        assert_eq!(spans[0].1.generation, None);
    }
}
