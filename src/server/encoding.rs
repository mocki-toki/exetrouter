//! Decode native HTTP bodies after authentication, before JSON extraction.
use super::{error, AppState, RequestId, MAX_INFERENCE_REQUEST_BYTES};
use axum::{
    body::Body,
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use std::{
    io::Read,
    sync::Arc,
    time::{Duration, Instant},
};

const READ_TIMEOUT: Duration = Duration::from_secs(10);
const DECODE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, PartialEq)]
enum Failure {
    Invalid,
    Large,
    Timeout,
}

pub(super) struct Rejection {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}

impl IntoResponse for Rejection {
    fn into_response(self) -> Response {
        error(self.status, self.code, self.message)
    }
}

fn reject(status: StatusCode, code: &'static str, message: &'static str) -> Rejection {
    Rejection {
        status,
        code,
        message,
    }
}

fn decode(wire: &[u8]) -> Result<Vec<u8>, Failure> {
    let mut decoder = zstd::stream::read::Decoder::new(wire).map_err(|_| Failure::Invalid)?;
    // Bound the decoder's history allocation independently of decoded output.
    decoder.window_log_max(23).map_err(|_| Failure::Invalid)?;
    let started = Instant::now();
    let mut output = Vec::new();
    let mut chunk = [0; 32 * 1024];
    loop {
        if started.elapsed() >= DECODE_TIMEOUT {
            return Err(Failure::Timeout);
        }
        let count = decoder.read(&mut chunk).map_err(|_| Failure::Invalid)?;
        if count == 0 {
            return Ok(output);
        }
        if output.len() + count > MAX_INFERENCE_REQUEST_BYTES {
            return Err(Failure::Large);
        }
        output.extend_from_slice(&chunk[..count]);
    }
}

pub(super) async fn prepare(state: &Arc<AppState>, request: Request) -> Result<Request, Rejection> {
    let encodings = request.headers().get_all("content-encoding");
    let mut values = encodings.iter();
    let encoding = values.next();
    if encoding.is_none() {
        return Ok(request);
    }
    let single = values.next().is_none();
    if single && encoding.is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"identity")) {
        return Ok(request);
    }
    if !single || !encoding.is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"zstd")) {
        return Err(reject(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_content_encoding",
            "supported request content encodings are identity and zstd",
        ));
    }
    let permit = state
        .decode_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            reject(
                StatusCode::SERVICE_UNAVAILABLE,
                "server_busy",
                "request decoders are busy",
            )
        })?;
    let (mut parts, body) = request.into_parts();
    let wire = match tokio::time::timeout(READ_TIMEOUT, async {
        let mut stream = body.into_data_stream();
        let mut wire = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| Failure::Invalid)?;
            if wire.len() + chunk.len() > MAX_INFERENCE_REQUEST_BYTES {
                return Err(Failure::Large);
            }
            wire.extend_from_slice(&chunk);
        }
        Ok(wire)
    })
    .await
    {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(failure)) => {
            let large = failure == Failure::Large;
            return Err(reject(
                if large {
                    StatusCode::PAYLOAD_TOO_LARGE
                } else {
                    StatusCode::BAD_REQUEST
                },
                if large {
                    "request_too_large"
                } else {
                    "invalid_request_error"
                },
                "compressed request body could not be read within the 16 MiB limit",
            ));
        }
        Err(_) => {
            return Err(reject(
                StatusCode::REQUEST_TIMEOUT,
                "request_timeout",
                "compressed request body read timed out",
            ))
        }
    };
    // No queue. The permit stays with the blocking work even if the caller leaves.
    let decoded = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        decode(&wire)
    })
    .await;
    let bytes = match decoded {
        Ok(Ok(bytes)) => bytes,
        outcome => {
            let failure = match outcome {
                Ok(Err(failure)) => failure,
                _ => Failure::Invalid,
            };
            let (status, code, message) = match failure {
                Failure::Invalid => (
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    "invalid zstd request body",
                ),
                Failure::Large => (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "request_too_large",
                    "decoded request body exceeds the 16 MiB limit",
                ),
                Failure::Timeout => (
                    StatusCode::REQUEST_TIMEOUT,
                    "request_timeout",
                    "request decoding timed out",
                ),
            };
            let id = parts
                .extensions
                .get::<RequestId>()
                .expect("request middleware");
            tracing::info!(event="encoded_request_rejected", request_id=%id.0, status=status.as_u16(), code);
            return Err(reject(status, code, message));
        }
    };
    parts.headers.remove("content-length");
    Ok(Request::from_parts(parts, Body::from(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn corrupt_truncated_and_expanding_frames_fail() {
        assert_eq!(
            decode(b"synthetic private invalid frame"),
            Err(Failure::Invalid)
        );
        let frame = zstd::stream::encode_all(&b"{}"[..], 3).unwrap();
        assert_eq!(decode(&frame), Ok(b"{}".to_vec()));
        assert_eq!(decode(&frame[..frame.len() - 1]), Err(Failure::Invalid));
        let frame =
            zstd::stream::encode_all(&vec![b' '; MAX_INFERENCE_REQUEST_BYTES + 1][..], 3).unwrap();
        assert_eq!(decode(&frame), Err(Failure::Large));
    }
}
