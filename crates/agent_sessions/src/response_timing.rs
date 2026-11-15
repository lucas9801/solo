//! SOLO: how fast an agent's model answers: the time to its first token and
//! its output speed, per model request, from whatever source can tell for
//! the agent (its transcript, its telemetry, Solo's plugin for it).
//!
//! A value that can't be told for sure isn't guessed: a request without one
//! leaves it out, and a summary without any has none.

use std::time::Duration;

use collections::HashMap;

/// Output speed is only worked out for a response at least this long:
/// shorter ones are mostly overhead.
pub const MIN_SPEED_OUTPUT_TOKENS: u64 = 200;
/// Nor for one generated faster than this, whose time is too short to measure.
pub const MIN_GENERATION: Duration = Duration::from_millis(500);
/// A first token later than this is taken as a measurement gone wrong (a
/// clock change, a request that waited for something else).
const MAX_FIRST_TOKEN: Duration = Duration::from_secs(30 * 60);
/// How many requests a session keeps; older ones drop out of the average.
const MAX_REQUESTS: usize = 20_000;

/// What's known of one model request's timing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ResponseTiming {
    /// From sending the request to the first token of the response.
    pub first_token: Option<Duration>,
    /// The tokens the response had (thinking included).
    pub output_tokens: Option<u64>,
    /// From the first token to the last.
    pub generation: Option<Duration>,
}

impl ResponseTiming {
    /// The first token, when it's plausible.
    pub fn first_token(&self) -> Option<Duration> {
        self.first_token
            .filter(|first_token| *first_token <= MAX_FIRST_TOKEN)
    }

    /// Output tokens per second, for a response long enough to tell.
    pub fn speed(&self) -> Option<f64> {
        let tokens = self.output_tokens?;
        let generation = self.generation?;
        if tokens < MIN_SPEED_OUTPUT_TOKENS || generation < MIN_GENERATION {
            return None;
        }
        Some(tokens as f64 / generation.as_secs_f64())
    }
}

/// The first token and speed of the latest request that had them, and their
/// arithmetic means over every request that had them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ResponseSummary {
    pub latest_first_token: Option<Duration>,
    pub latest_speed: Option<f64>,
    pub mean_first_token: Option<Duration>,
    pub mean_speed: Option<f64>,
}

impl ResponseSummary {
    pub fn is_empty(&self) -> bool {
        self.latest_first_token.is_none()
            && self.latest_speed.is_none()
            && self.mean_first_token.is_none()
            && self.mean_speed.is_none()
    }
}

/// The requests of a session, in the order they were sent.
#[derive(Clone, Debug, Default)]
pub struct ResponseTimings {
    requests: Vec<Request>,
    by_key: HashMap<String, usize>,
    /// How many requests dropped out at the front.
    dropped: usize,
}

#[derive(Clone, Debug)]
struct Request {
    timing: ResponseTiming,
    /// Made by a sub-agent: counts for the average, not as the latest.
    sidechain: bool,
}

impl ResponseTimings {
    pub fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }

    pub fn len(&self) -> usize {
        self.requests.len()
    }

    /// Records request `key`, or updates it with what's known now.
    pub fn record(&mut self, key: &str, timing: ResponseTiming, sidechain: bool) {
        if let Some(ix) = self.by_key.get(key).copied()
            && let Some(request) = ix
                .checked_sub(self.dropped)
                .and_then(|ix| self.requests.get_mut(ix))
        {
            request.timing = timing;
            request.sidechain = sidechain;
            return;
        }
        if self.requests.len() >= MAX_REQUESTS {
            self.requests.remove(0);
            self.dropped += 1;
            let dropped = self.dropped;
            self.by_key.retain(|_, ix| *ix >= dropped);
        }
        self.by_key
            .insert(key.to_string(), self.dropped + self.requests.len());
        self.requests.push(Request { timing, sidechain });
    }

    pub fn summary(&self) -> ResponseSummary {
        let mut summary = ResponseSummary::default();
        let (mut first_token_sum, mut first_token_count) = (Duration::ZERO, 0u32);
        let (mut speed_sum, mut speed_count) = (0., 0u32);
        for request in &self.requests {
            let first_token = request.timing.first_token();
            let speed = request.timing.speed();
            if let Some(first_token) = first_token {
                first_token_sum += first_token;
                first_token_count += 1;
            }
            if let Some(speed) = speed {
                speed_sum += speed;
                speed_count += 1;
            }
            if !request.sidechain {
                summary.latest_first_token = first_token.or(summary.latest_first_token);
                summary.latest_speed = speed.or(summary.latest_speed);
            }
        }
        if first_token_count > 0 {
            summary.mean_first_token = Some(first_token_sum / first_token_count);
        }
        if speed_count > 0 {
            summary.mean_speed = Some(speed_sum / speed_count as f64);
        }
        summary
    }
}

/// A timestamp as agents record it (RFC 3339, e.g.
/// `2026-11-01T13:44:36.229Z`), in milliseconds since the epoch.
pub fn parse_timestamp_ms(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text.trim())
        .ok()
        .map(|time| time.timestamp_millis())
}

/// The time between two millisecond timestamps, if `to` isn't before `from`.
pub(crate) fn millis_between(from: i64, to: i64) -> Option<Duration> {
    u64::try_from(to.checked_sub(from)?)
        .ok()
        .map(Duration::from_millis)
}

/// An effort level as agents name it ("medium", "xhigh"): short, letters,
/// digits and `-` or `_` only.
pub(crate) fn effort_level(text: &str) -> Option<String> {
    let text = text.trim();
    ((1..=24).contains(&text.len())
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')))
    .then(|| text.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timing(
        first_token_ms: Option<u64>,
        tokens: Option<u64>,
        generation_ms: Option<u64>,
    ) -> ResponseTiming {
        ResponseTiming {
            first_token: first_token_ms.map(Duration::from_millis),
            output_tokens: tokens,
            generation: generation_ms.map(Duration::from_millis),
        }
    }

    #[test]
    fn test_speed_needs_a_long_enough_response() {
        assert_eq!(timing(None, Some(1280), Some(10_000)).speed(), Some(128.));
        assert_eq!(timing(None, Some(199), Some(10_000)).speed(), None);
        assert_eq!(timing(None, Some(1000), Some(499)).speed(), None);
        assert_eq!(timing(None, None, Some(10_000)).speed(), None);
        assert_eq!(timing(None, Some(1000), None).speed(), None);
        assert_eq!(timing(Some(31 * 60 * 1000), None, None).first_token(), None);
    }

    #[test]
    fn test_summary() {
        let mut timings = ResponseTimings::default();
        assert!(timings.summary().is_empty());
        timings.record("a", timing(Some(5000), Some(1000), Some(10_000)), false);
        // Updated as more of it is known.
        timings.record("b", timing(Some(3000), None, None), false);
        timings.record("b", timing(Some(3000), Some(1500), Some(10_000)), false);
        // Nothing known.
        timings.record("c", timing(None, Some(50), Some(1000)), false);
        // A sub-agent's: in the average only.
        timings.record("d", timing(Some(1000), Some(2000), Some(10_000)), true);
        assert_eq!(timings.len(), 4);
        let summary = timings.summary();
        assert_eq!(summary.latest_first_token, Some(Duration::from_secs(3)));
        assert_eq!(summary.latest_speed, Some(150.));
        assert_eq!(summary.mean_first_token, Some(Duration::from_secs(3)));
        assert_eq!(summary.mean_speed, Some(150.));
    }

    #[test]
    fn test_old_requests_drop_out() {
        let mut timings = ResponseTimings::default();
        for ix in 0..MAX_REQUESTS + 2 {
            timings.record(&ix.to_string(), timing(Some(1000), None, None), false);
        }
        assert_eq!(timings.len(), MAX_REQUESTS);
        // An update of a kept one still finds it.
        timings.record(
            &(MAX_REQUESTS + 1).to_string(),
            timing(Some(2000), None, None),
            false,
        );
        assert_eq!(timings.len(), MAX_REQUESTS);
        assert_eq!(
            timings.summary().latest_first_token,
            Some(Duration::from_secs(2))
        );
    }

    #[test]
    fn test_timestamps_and_effort() {
        assert_eq!(
            parse_timestamp_ms("2026-11-01T13:44:36.229Z"),
            Some(1_793_540_676_229)
        );
        assert_eq!(
            parse_timestamp_ms("2026-11-01T21:44:36.229+08:00"),
            Some(1_793_540_676_229)
        );
        assert_eq!(parse_timestamp_ms("yesterday"), None);
        assert_eq!(
            millis_between(1000, 2500),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(millis_between(2500, 1000), None);
        assert_eq!(effort_level(" xhigh "), Some("xhigh".to_string()));
        assert_eq!(effort_level("High"), Some("high".to_string()));
        assert_eq!(effort_level(""), None);
        assert_eq!(effort_level("a b"), None);
    }
}
