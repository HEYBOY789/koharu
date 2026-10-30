use std::time::Duration;

use thiserror::Error;

use crate::Language;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{provider} does not support target language {language}")]
    UnsupportedLanguage {
        provider: &'static str,
        language: Language,
    },
    #[error("{provider} does not support source language {language}")]
    UnsupportedSourceLanguage {
        provider: &'static str,
        language: Language,
    },
    #[error("{provider} returned {actual} segments; expected {expected}")]
    SegmentCount {
        provider: &'static str,
        expected: usize,
        actual: usize,
    },
    #[error("{provider} quota or rate limit was exceeded")]
    QuotaExceeded { provider: &'static str },
    #[error(
        "{provider} returned empty text for segment(s) {segments:?}; the page remains unfinished. Check these OCR segments if retries are exhausted."
    )]
    EmptyTranslation {
        provider: &'static str,
        segments: Vec<usize>,
    },
    #[error("{provider} API request failed with HTTP {status}: {message}")]
    Api {
        provider: &'static str,
        status: u16,
        message: String,
    },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl Error {
    /// Only the compatible LLM endpoint participates in automatic recovery.
    /// Malformed output and account limits still require human action.
    pub fn retry_delay(&self, backoff: Duration) -> Option<Duration> {
        if matches!(
            self,
            Self::EmptyTranslation {
                provider: "openai-compatible",
                ..
            }
        ) {
            return Some(backoff);
        }
        let Self::Api {
            provider: "openai-compatible",
            status: status @ (502..=504 | 524),
            message,
        } = self
        else {
            return None;
        };
        let lower = message.to_ascii_lowercase();
        if [
            "insufficient_quota",
            "quota_exceeded",
            "resource_exhausted",
            "rate_limit",
            "rate limit",
            "credit balance",
            "invalid_api_key",
            "authentication_error",
        ]
        .iter()
        .any(|value| lower.contains(value))
        {
            return None;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(message) {
            let error = &value["error"];
            if error["type"] == "koharu_proxy_error" {
                if !matches!(
                    error["code"].as_str(),
                    Some("upstream_unreachable" | "upstream_timeout")
                ) {
                    return None;
                }
            }
        }
        // A Cloudflare read timeout needs a longer cooldown than a brief
        // connection outage, even on the first recovery attempt.
        Some(if *status == 524 {
            backoff.max(Duration::from_secs(120))
        } else {
            backoff
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Error;
    use std::time::Duration;
    #[test]
    fn empty_translation_retries_only_for_the_existing_compatible_recovery_path() {
        assert!(
            Error::EmptyTranslation {
                provider: "openai-compatible",
                segments: vec![2]
            }
            .retry_delay(Duration::from_secs(15))
            .is_some()
        );
        assert!(
            !Error::EmptyTranslation {
                provider: "deepl",
                segments: vec![2]
            }
            .retry_delay(Duration::from_secs(15))
            .is_some()
        );
        assert!(
            !Error::QuotaExceeded {
                provider: "openai-compatible"
            }
            .retry_delay(Duration::from_secs(15))
            .is_some()
        );
    }
    #[test]
    fn automatic_recovery_distinguishes_outages_from_quota_and_invalid_output() {
        for (provider, status, message, expected) in [
            (
                "openai-compatible",
                502,
                r#"{"error":{"type":"koharu_proxy_error","code":"upstream_unreachable"}}"#,
                true,
            ),
            (
                "openai-compatible",
                504,
                r#"{"error":{"type":"koharu_proxy_error","code":"upstream_timeout"}}"#,
                true,
            ),
            (
                "openai-compatible",
                503,
                "Gateway temporarily unavailable",
                true,
            ),
            (
                "openai-compatible",
                524,
                r#"{"title":"Error 524: A timeout occurred","status":524,"detail":"The origin web server did not return a complete response within the 120-second Proxy Read Timeout window."}"#,
                true,
            ),
            ("openai-compatible", 429, "rate limit", false),
            ("openai-compatible", 401, "unauthorized", false),
            ("openai-compatible", 503, "insufficient_quota", false),
            ("openai-compatible", 524, "insufficient_quota", false),
            ("openai-compatible", 524, "authentication_error", false),
            ("openai-compatible", 520, "unknown origin error", false),
            (
                "openai-compatible",
                502,
                r#"{"error":{"type":"koharu_proxy_error","code":"invalid_translation_json"}}"#,
                false,
            ),
            (
                "openai-compatible",
                502,
                r#"{"error":{"type":"koharu_proxy_error","code":"incomplete_completion"}}"#,
                false,
            ),
            ("deepl", 503, "Gateway temporarily unavailable", false),
            ("deepl", 524, "A timeout occurred", false),
        ] {
            assert_eq!(
                Error::Api {
                    provider,
                    status,
                    message: message.into()
                }
                .retry_delay(Duration::from_secs(15))
                .is_some(),
                expected,
                "{provider} {status} {message}"
            );
        }
        assert!(
            !Error::QuotaExceeded {
                provider: "openai-compatible"
            }
            .retry_delay(Duration::from_secs(15))
            .is_some()
        );
    }

    #[test]
    fn automatic_recovery_cloudflare_timeout_waits_at_least_two_minutes() {
        let failure = Error::Api {
            provider: "openai-compatible",
            status: 524,
            message: "Error 524: A timeout occurred".into(),
        };
        for (backoff, expected) in [(15, 120), (30, 120), (60, 120), (120, 120), (300, 300)] {
            assert_eq!(
                failure.retry_delay(Duration::from_secs(backoff)),
                Some(Duration::from_secs(expected))
            );
        }
        assert_eq!(
            Error::Api {
                provider: "openai-compatible",
                status: 502,
                message: "Gateway temporarily unavailable".into(),
            }
            .retry_delay(Duration::from_secs(15)),
            Some(Duration::from_secs(15))
        );
    }
}
