//! Raw Cloud Code streaming: envelope construction, SSE decoder priming and lazy tail
//! stream assembly for one endpoint.

use crate::accounts::ModelFamily;
use crate::constants::{HeaderStyle, headers_for_style};
use crate::service::{ActiveCredentials, AntigravityService, now_ms};
use crate::stream::{CloudCodeSseDecoder, StreamDecodeFailure};
use crate::transform::build_cloud_code_request;
use bytes::Bytes;
use futures_util::stream::{self, StreamExt};
use oc_llm::{LlmError, LlmEvent, LlmRequest, LlmStream};
use std::pin::Pin;
use std::sync::Arc;

use super::SingleAttempt;
use super::validation::try_single_endpoint;

type ByteStream =
    Pin<Box<dyn futures_util::Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Send>>;

pub(super) async fn send_stream_once(
    svc: &AntigravityService,
    request: &LlmRequest,
    creds: &ActiveCredentials,
    style: HeaderStyle,
    override_style: Option<HeaderStyle>,
    force_strip_thinking: bool,
) -> SingleAttempt {
    let cfg = svc.config();
    let session_key = request
        .model
        .model
        .id
        .split_once('#')
        .map_or_else(|| request.model.model.id.clone(), |(a, _)| a.to_owned());

    let transformed = {
        let sig_arc = svc.signature_cache();
        let mut sig_guard = sig_arc.lock().ok();
        build_cloud_code_request(
            request,
            &creds.project_id,
            &session_key,
            &cfg,
            override_style,
            sig_guard.as_deref_mut(),
            force_strip_thinking,
            now_ms(),
        )
    };

    let hub_version = svc.version().current();
    let header_set = headers_for_style(style, &hub_version);
    let mut last_failure: Option<SingleAttempt> = None;

    for base in svc.endpoints().for_style(style) {
        let attempt = try_single_endpoint(
            svc,
            base,
            &transformed.envelope,
            transformed.resolved.family,
            &session_key,
            creds,
            style,
            &header_set,
        )
        .await;
        match attempt {
            SingleAttempt::RetryableFailure { status, body } => {
                last_failure = Some(SingleAttempt::RetryableFailure { status, body });
            }
            other => return other,
        }
    }

    last_failure.unwrap_or_else(|| {
        SingleAttempt::Fatal(LlmError::Provider {
            message: "no upstream endpoint available for request".to_owned(),
        })
    })
}

pub(super) async fn prime_decoder_stream(
    resp: reqwest::Response,
    family: ModelFamily,
    session_key: String,
    sig_cache: Arc<std::sync::Mutex<crate::signature_cache::SignatureCache>>,
) -> SingleAttempt {
    let mut decoder = CloudCodeSseDecoder::new(family, session_key, Some(sig_cache), now_ms());
    let mut byte_stream: ByteStream = Box::pin(resp.bytes_stream());
    let mut initial_events: Vec<LlmEvent> = Vec::new();
    let mut eof = false;

    while initial_events.is_empty() {
        match byte_stream.next().await {
            Some(Ok(chunk)) => match decoder.push_bytes(&chunk) {
                Ok(evts) => initial_events.extend(evts),
                Err(StreamDecodeFailure::ThinkingBlockOrder(_)) => {
                    return SingleAttempt::RetryStripThinking;
                }
                Err(StreamDecodeFailure::Fatal(msg)) => {
                    return SingleAttempt::Fatal(LlmError::Provider { message: msg });
                }
                Err(StreamDecodeFailure::EmptyStream) => {
                    return SingleAttempt::RetryEmptyResponse;
                }
            },
            Some(Err(err)) => {
                return SingleAttempt::RetryableFailure {
                    status: 502,
                    body: format!("stream read error: {err}"),
                };
            }
            None => {
                eof = true;
                match decoder.finish() {
                    Ok(evts) => initial_events.extend(evts),
                    Err(StreamDecodeFailure::EmptyStream) => {
                        return SingleAttempt::RetryEmptyResponse;
                    }
                    Err(StreamDecodeFailure::ThinkingBlockOrder(_)) => {
                        return SingleAttempt::RetryStripThinking;
                    }
                    Err(StreamDecodeFailure::Fatal(msg)) => {
                        return SingleAttempt::Fatal(LlmError::Provider { message: msg });
                    }
                }
                break;
            }
        }
    }

    SingleAttempt::Ready(build_llm_stream(initial_events, eof, byte_stream, decoder))
}

struct TailState {
    bytes: ByteStream,
    decoder: CloudCodeSseDecoder,
    finished: bool,
}

fn build_llm_stream(
    initial_events: Vec<LlmEvent>,
    already_finished: bool,
    byte_stream: ByteStream,
    decoder: CloudCodeSseDecoder,
) -> LlmStream {
    let head = stream::iter(initial_events);
    if already_finished {
        return Box::pin(head);
    }

    let tail = stream::unfold(
        TailState {
            bytes: byte_stream,
            decoder,
            finished: false,
        },
        |mut st| async move {
            if st.finished {
                return None;
            }
            loop {
                match st.bytes.next().await {
                    Some(Ok(chunk)) => match st.decoder.push_bytes(&chunk) {
                        Ok(evts) if evts.is_empty() => {}
                        Ok(evts) => return Some((stream::iter(evts), st)),
                        Err(err) => {
                            st.finished = true;
                            let msg = match err {
                                StreamDecodeFailure::EmptyStream => {
                                    "upstream stream ended unexpectedly".to_owned()
                                }
                                StreamDecodeFailure::ThinkingBlockOrder(m)
                                | StreamDecodeFailure::Fatal(m) => m,
                            };
                            return Some((
                                stream::iter(vec![LlmEvent::ProviderError { message: msg }]),
                                st,
                            ));
                        }
                    },
                    Some(Err(err)) => {
                        st.finished = true;
                        return Some((
                            stream::iter(vec![LlmEvent::ProviderError {
                                message: format!("stream transport error: {err}"),
                            }]),
                            st,
                        ));
                    }
                    None => {
                        st.finished = true;
                        let final_evts = match st.decoder.finish() {
                            Ok(evts) => evts,
                            Err(StreamDecodeFailure::EmptyStream) => Vec::new(),
                            Err(
                                StreamDecodeFailure::ThinkingBlockOrder(m)
                                | StreamDecodeFailure::Fatal(m),
                            ) => {
                                vec![LlmEvent::ProviderError { message: m }]
                            }
                        };
                        return Some((stream::iter(final_evts), st));
                    }
                }
            }
        },
    )
    .flatten();

    Box::pin(head.chain(tail))
}
