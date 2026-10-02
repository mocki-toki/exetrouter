//! Independent payload bounds for inference input, upstream events and projection.
//! Larger image/history inputs must not implicitly enlarge output buffering.
//! These bound wire/transient payloads, not process RSS or model context tokens.

pub(crate) const INFERENCE_REQUEST_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const UPSTREAM_WS_MESSAGE_BYTES: usize = 1024 * 1024;
pub(crate) const UPSTREAM_SSE_EVENT_BYTES: usize = 1024 * 1024;
pub(crate) const PROJECTED_OUTPUT_BYTES: usize = 1024 * 1024;
pub(crate) const SSE_QUEUE_CHUNKS: usize = 8;
