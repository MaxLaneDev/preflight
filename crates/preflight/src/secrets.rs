//! In-memory credential redaction, including matches split across transport or pipe chunks.
use crate::config::Config;
use axum::{
    body::{Body, Bytes},
    http::header,
    response::Response,
};
use futures_core::Stream;
use std::{
    pin::Pin,
    task::{Context, Poll},
};

pub fn configured(config: &Config) -> Vec<String> {
    config
        .access_tokens
        .iter()
        .map(|v| v.token_env.as_str())
        .chain(
            config
                .ci_sources
                .iter()
                .filter_map(|v| v.token_env.as_deref()),
        )
        .chain(std::iter::once("PREFLIGHT_ADMIN_TOKEN"))
        .filter_map(|name| std::env::var(name).ok())
        .chain(
            config
                .admin_token_file
                .as_deref()
                .and_then(|path| crate::settings::file_credential(path).ok()),
        )
        .filter(|value| value.len() >= 16)
        .collect()
}
pub fn variants(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut result = std::collections::BTreeSet::new();
    for value in values {
        if value.is_empty() {
            continue;
        }
        if let Ok(encoded) = serde_json::to_string(&value) {
            result.insert(encoded[1..encoded.len() - 1].to_owned());
        }
        result.insert(value);
    }
    result.into_iter().collect()
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Checkpoint {
    digest: String,
    prefix_bytes: usize,
}
pub struct Filter {
    values: Vec<Vec<u8>>,
    pending: Vec<u8>,
}
impl Filter {
    pub fn new(values: Vec<String>) -> Self {
        Self {
            values: values
                .into_iter()
                .map(String::into_bytes)
                .filter(|v| !v.is_empty())
                .collect(),
            pending: vec![],
        }
    }
    pub(crate) fn checkpoint(&self) -> Option<Checkpoint> {
        use sha2::{Digest, Sha256};
        if self.pending.is_empty() {
            return None;
        }
        self.values
            .iter()
            .find(|v| v.starts_with(&self.pending))
            .map(|v| Checkpoint {
                digest: crate::hashing::hex(Sha256::digest(v)),
                prefix_bytes: self.pending.len(),
            })
    }
    pub(crate) fn restore(values: Vec<String>, checkpoint: Option<&Checkpoint>) -> (Self, bool) {
        use sha2::{Digest, Sha256};
        let mut filter = Self::new(values);
        if let Some(checkpoint) = checkpoint {
            let value = filter.values.iter().find(|v| {
                checkpoint.prefix_bytes > 0
                    && checkpoint.prefix_bytes < v.len()
                    && crate::hashing::hex(Sha256::digest(v)) == checkpoint.digest
            });
            if let Some(value) = value {
                filter.pending = value[..checkpoint.prefix_bytes].to_vec();
            } else {
                return (filter, false);
            }
        }
        (filter, true)
    }
    pub fn push(&mut self, bytes: &[u8], last: bool) -> Vec<u8> {
        self.pending.extend_from_slice(bytes);
        let mut result = Vec::new();
        let mut position = 0;
        while position < self.pending.len() {
            let tail = &self.pending[position..];
            if !last
                && self
                    .values
                    .iter()
                    .any(|v| tail.len() < v.len() && v.starts_with(tail))
            {
                break;
            }
            if let Some(value) = self
                .values
                .iter()
                .filter(|v| tail.starts_with(v))
                .max_by_key(|v| v.len())
            {
                result.extend_from_slice(b"[REDACTED]");
                position += value.len();
            } else {
                result.push(self.pending[position]);
                position += 1;
            }
        }
        self.pending.drain(..position);
        result
    }
    pub fn truncate(&mut self) -> Vec<u8> {
        if self.pending.is_empty() {
            vec![]
        } else {
            self.pending.clear();
            b"[REDACTED]".to_vec()
        }
    }
}
struct Redacted {
    inner: Pin<Box<axum::body::BodyDataStream>>,
    filter: Filter,
    done: bool,
}
impl Stream for Redacted {
    type Item = Result<Bytes, axum::Error>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        loop {
            match this.inner.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(bytes))) => {
                    let value = this.filter.push(&bytes, false);
                    if !value.is_empty() {
                        return Poll::Ready(Some(Ok(Bytes::from(value))));
                    }
                }
                Poll::Ready(Some(Err(error))) => return Poll::Ready(Some(Err(error))),
                Poll::Ready(None) => {
                    this.done = true;
                    let value = this.filter.push(&[], true);
                    return Poll::Ready((!value.is_empty()).then(|| Ok(Bytes::from(value))));
                }
            }
        }
    }
}
pub fn response(mut response: Response, values: Vec<String>) -> Response {
    if values.is_empty()
        || !response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                v.starts_with("application/json") || v.starts_with("text/event-stream")
            })
    {
        return response;
    }
    response.headers_mut().remove(header::CONTENT_LENGTH);
    let (parts, body) = response.into_parts();
    Response::from_parts(
        parts,
        Body::from_stream(Redacted {
            inner: Box::pin(body.into_data_stream()),
            filter: Filter::new(values),
            done: false,
        }),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_split_redacts_the_same_credential() {
        let secret = "private_credential_01234567890123456789";
        let text = format!("before {secret} after\n");
        for split in 0..=text.len() {
            let mut filter = Filter::new(vec![secret.into()]);
            let mut actual = filter.push(&text.as_bytes()[..split], false);
            actual.extend(filter.push(&text.as_bytes()[split..], false));
            actual.extend(filter.push(&[], true));
            assert_eq!(
                String::from_utf8(actual).unwrap(),
                "before [REDACTED] after\n"
            );
        }
    }
    #[test]
    fn normal_streams_do_not_wait_for_a_large_lookbehind() {
        let mut filter = Filter::new(vec!["private_credential_01234567890123456789".into()]);
        assert_eq!(filter.push(b"data: {}\n\n", false), b"data: {}\n\n");
        assert_eq!(filter.push(b"priv", false), b"");
        assert_eq!(filter.truncate(), b"[REDACTED]");
    }
    #[test]
    fn longest_match_unicode_and_json_escaped_values_are_preserved() {
        let values = variants(vec![
            "short-secret".into(),
            "short-secret-extended".into(),
            "quote\"secret".into(),
        ]);
        let mut filter = Filter::new(values);
        assert_eq!(
            filter.push("λ short-secret".as_bytes(), false),
            "λ ".as_bytes()
        );
        assert_eq!(
            filter.push(b"-extended \"quote\\\"secret\"\n", true),
            b"[REDACTED] \"[REDACTED]\"\n"
        );
    }
}
