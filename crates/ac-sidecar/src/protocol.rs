//! Sidecar protocol v1: frames and the four messages, pure.
//!
//! # Theory
//!
//! The host and the sidecar are two processes joined by the child's stdin
//! (host to child) and stdout (child to host). The sidecar is Python, so
//! the protocol must be trivial to write there and easy to read in a log,
//! yet carry megabytes of pixels without base64. Each message is therefore
//! a *frame*: a small JSON header that says what the message is, and a raw
//! binary payload.
//!
//! ```text
//! u32 LE header_len | header_len bytes of UTF-8 JSON | u32 LE payload_len | payload_len bytes
//! ```
//!
//! The header is at most [`MAX_HEADER_BYTES`] (1 MiB) and the payload at
//! most [`MAX_PAYLOAD_BYTES`] (64 MiB); a larger length is a protocol error,
//! raised before anything is allocated, so a corrupt length cannot exhaust
//! memory. Only frames travel on stdout; the sidecar logs to stderr.
//!
//! | `type` | Direction | Header fields | Payload |
//! |---|---|---|---|
//! | `hello` | child to host, once, after the model loaded | `protocol` (1), `model`, `weights_sha256` (string or null), `input_side`, `capabilities` {`masks`, `parts`} | empty |
//! | `detect` | host to child | `id`, `source`, `seq`, `pts_ns`, `width`, `height`, `format` ("rgb8"), `phrases` [{`text`, `threshold`}], `want_masks` (false) | `width * height * 3` bytes, RGB8, row-major, top row first |
//! | `result` | child to host | `id`, `source`, `seq`, `pts_ns` (echoed), `latency_ms`, `detections` [{`phrase`, `box` [x, y, w, h], `score`}] | empty (masks are reserved for a later version) |
//! | `error` | child to host | `id` (u64, or null for a fatal error after which the child exits), `message` | empty |
//!
//! A result's boxes are normalized to `0..1` of the image sent, origin at
//! the top-left, and `phrase` indexes the request's `phrases`; only
//! detections at or above their phrase's threshold are returned.
//!
//! # Validation
//!
//! Decoding is strict, because the two sides are written in different
//! languages and drift must fail loudly at the first message instead of
//! surfacing as wrong boxes:
//!
//! * the header must be UTF-8 JSON holding an object with a string `type`
//!   naming one of the four messages;
//! * every field of that message must be present (`null` is spelled out,
//!   never omitted), have the right JSON type, and no other field may
//!   appear, in nested objects too;
//! * values must make sense: a hello speaks protocol 1 (checked first, so a
//!   newer sidecar is reported as such rather than as unknown fields), a
//!   SHA-256 is 64 hex digits, sizes are positive, thresholds lie in
//!   `0..=1`, numbers are finite, `want_masks` is false;
//! * the payload length must be the one the header implies.
//!
//! Range checks that a well-behaved model can miss by rounding (a box edge
//! at 1.0000001, a score of 1.0001) are left to the host, which clamps.
//!
//! # Invariants
//!
//! * Pure: no clock, no process, no global state. [`read_frame`] and
//!   [`write_frame`] work on any [`Read`] and [`Write`], so they are tested
//!   on byte buffers.
//! * Total: decoding returns an error, never panics, for any input bytes,
//!   and allocates at most the lengths it has been sent, capped by the
//!   limits.
//! * Round trip: [`decode_header`] of [`encode_header`] gives the message
//!   back, and [`encode_header`] refuses exactly the messages that
//!   [`decode_header`] would refuse, so the host never sends a request the
//!   sidecar must reject.
//! * A stream ending exactly between frames is a clean end
//!   ([`read_frame`] returns `None`); ending inside a frame is
//!   [`ProtocolError::Truncated`].

use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use thiserror::Error;

mod messages;

use messages::Tagged;

pub use messages::{
    Capabilities, Detect, DetectResult, ErrorReply, Found, Hello, Message, Phrase, PixelFormat,
};

/// The protocol version this crate speaks.
pub const PROTOCOL_VERSION: u32 = 1;

/// Largest header accepted or sent, in bytes (1 MiB).
pub const MAX_HEADER_BYTES: u32 = 1 << 20;

/// Largest payload accepted or sent, in bytes (64 MiB).
pub const MAX_PAYLOAD_BYTES: u32 = 64 << 20;

/// Header fields of a `hello`, in protocol order.
const HELLO_FIELDS: &[&str] = &[
    "protocol",
    "model",
    "weights_sha256",
    "input_side",
    "capabilities",
];

/// Header fields of a `detect`, in protocol order.
const DETECT_FIELDS: &[&str] = &[
    "id",
    "source",
    "seq",
    "pts_ns",
    "width",
    "height",
    "format",
    "phrases",
    "want_masks",
];

/// Header fields of a `result`, in protocol order.
const RESULT_FIELDS: &[&str] = &["id", "source", "seq", "pts_ns", "latency_ms", "detections"];

/// Header fields of an `error`, in protocol order.
const ERROR_FIELDS: &[&str] = &["id", "message"];

/// Everything that can be wrong with a frame or a message.
#[derive(Debug, Error)]
pub enum ProtocolError {
    /// A header length above [`MAX_HEADER_BYTES`].
    #[error("header of {0} bytes exceeds the {MAX_HEADER_BYTES} byte limit")]
    HeaderTooLarge(u64),
    /// A payload length above [`MAX_PAYLOAD_BYTES`].
    #[error("payload of {0} bytes exceeds the {MAX_PAYLOAD_BYTES} byte limit")]
    PayloadTooLarge(u64),
    /// The stream ended inside a frame.
    #[error("the stream ended inside a frame, in its {0}")]
    Truncated(&'static str),
    /// The header is not UTF-8.
    #[error("the header is not UTF-8")]
    NotUtf8,
    /// The header is not JSON.
    #[error("the header is not JSON: {0}")]
    Json(String),
    /// The header is JSON but not an object.
    #[error("the header is not a JSON object")]
    NotObject,
    /// The header has no `type` field, or it is not a string.
    #[error("the header has no string \"type\" field")]
    MissingType,
    /// The header's `type` names no v1 message.
    #[error("unknown message type {0:?}")]
    UnknownType(String),
    /// A hello speaks another protocol version.
    #[error("the sidecar speaks protocol {got}, this host speaks {PROTOCOL_VERSION}")]
    Version {
        /// The version it announced, as written.
        got: String,
    },
    /// A field the message does not have.
    #[error("{kind} message has unknown field {field:?}")]
    UnknownField {
        /// The message's type.
        kind: &'static str,
        /// The field's name.
        field: String,
    },
    /// A field the message must have is absent.
    #[error("{kind} message lacks field {field:?}")]
    MissingField {
        /// The message's type.
        kind: &'static str,
        /// The field's name.
        field: &'static str,
    },
    /// A field has the wrong JSON type or shape.
    #[error("{kind} message field {field:?}: {detail}")]
    BadField {
        /// The message's type.
        kind: &'static str,
        /// The field's name.
        field: &'static str,
        /// What the JSON decoder said.
        detail: String,
    },
    /// A field's value breaks a rule of the module docs.
    #[error("invalid {kind} message: {reason}")]
    Invalid {
        /// The message's type.
        kind: &'static str,
        /// Which rule, and the value.
        reason: String,
    },
    /// The payload is not the length the header implies.
    #[error("{kind} message carries {got} payload bytes, expected {expected}")]
    PayloadSize {
        /// The message's type.
        kind: &'static str,
        /// The length the header implies.
        expected: u64,
        /// The length received or given.
        got: u64,
    },
    /// Reading or writing the stream failed.
    #[error("stream I/O: {0}")]
    Io(#[from] io::Error),
}

/// One frame as read from the stream, not yet decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The JSON header's bytes.
    pub header: Vec<u8>,
    /// The payload's bytes.
    pub payload: Vec<u8>,
}

/// Serializes `message` into a header, after [`Message::validate`].
pub fn encode_header(message: &Message) -> Result<Vec<u8>, ProtocolError> {
    message.validate()?;
    let tagged = match message {
        Message::Hello(m) => Tagged::Hello(m),
        Message::Detect(m) => Tagged::Detect(m),
        Message::Result(m) => Tagged::Result(m),
        Message::Error(m) => Tagged::Error(m),
    };
    let header = serde_json::to_vec(&tagged).map_err(|e| ProtocolError::Json(e.to_string()))?;
    if header.len() > MAX_HEADER_BYTES as usize {
        return Err(ProtocolError::HeaderTooLarge(header.len() as u64));
    }
    Ok(header)
}

/// Parses and validates a header; see the module docs.
pub fn decode_header(bytes: &[u8]) -> Result<Message, ProtocolError> {
    if bytes.len() > MAX_HEADER_BYTES as usize {
        return Err(ProtocolError::HeaderTooLarge(bytes.len() as u64));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ProtocolError::NotUtf8)?;
    let value: Value =
        serde_json::from_str(text).map_err(|e| ProtocolError::Json(e.to_string()))?;
    let Value::Object(mut map) = value else {
        return Err(ProtocolError::NotObject);
    };
    let Some(Value::String(kind)) = map.remove("type") else {
        return Err(ProtocolError::MissingType);
    };
    let message = match kind.as_str() {
        "hello" => decode_hello(map)?,
        "detect" => decode_detect(map)?,
        "result" => decode_result(map)?,
        "error" => decode_error(map)?,
        _ => return Err(ProtocolError::UnknownType(kind)),
    };
    message.validate()?;
    Ok(message)
}

/// Fields of one message being decoded, consumed one by one.
struct Fields {
    /// The message's type.
    kind: &'static str,
    /// The fields not consumed yet.
    map: Map<String, Value>,
}

impl Fields {
    /// Starts decoding a `kind` message, rejecting fields not in `allowed`.
    fn new(
        kind: &'static str,
        map: Map<String, Value>,
        allowed: &[&str],
    ) -> Result<Self, ProtocolError> {
        if let Some(field) = map.keys().find(|k| !allowed.contains(&k.as_str())) {
            return Err(ProtocolError::UnknownField {
                kind,
                field: field.clone(),
            });
        }
        Ok(Self { kind, map })
    }

    /// Takes the required field `field` as a `T`.
    fn take<T: DeserializeOwned>(&mut self, field: &'static str) -> Result<T, ProtocolError> {
        let kind = self.kind;
        let value = self
            .map
            .remove(field)
            .ok_or(ProtocolError::MissingField { kind, field })?;
        serde_json::from_value(value).map_err(|e| ProtocolError::BadField {
            kind,
            field,
            detail: e.to_string(),
        })
    }
}

/// Decodes a `hello`'s fields, checking the version before anything else.
fn decode_hello(map: Map<String, Value>) -> Result<Message, ProtocolError> {
    if let Some(version) = map.get("protocol")
        && version.as_u64() != Some(u64::from(PROTOCOL_VERSION))
        && version.is_number()
    {
        return Err(ProtocolError::Version {
            got: version.to_string(),
        });
    }
    let mut f = Fields::new("hello", map, HELLO_FIELDS)?;
    Ok(Message::Hello(Hello {
        protocol: f.take("protocol")?,
        model: f.take("model")?,
        weights_sha256: f.take("weights_sha256")?,
        input_side: f.take("input_side")?,
        capabilities: f.take("capabilities")?,
    }))
}

/// Decodes a `detect`'s fields.
fn decode_detect(map: Map<String, Value>) -> Result<Message, ProtocolError> {
    let mut f = Fields::new("detect", map, DETECT_FIELDS)?;
    Ok(Message::Detect(Detect {
        id: f.take("id")?,
        source: f.take("source")?,
        seq: f.take("seq")?,
        pts_ns: f.take("pts_ns")?,
        width: f.take("width")?,
        height: f.take("height")?,
        format: f.take("format")?,
        phrases: f.take("phrases")?,
        want_masks: f.take("want_masks")?,
    }))
}

/// Decodes a `result`'s fields.
fn decode_result(map: Map<String, Value>) -> Result<Message, ProtocolError> {
    let mut f = Fields::new("result", map, RESULT_FIELDS)?;
    Ok(Message::Result(DetectResult {
        id: f.take("id")?,
        source: f.take("source")?,
        seq: f.take("seq")?,
        pts_ns: f.take("pts_ns")?,
        latency_ms: f.take("latency_ms")?,
        detections: f.take("detections")?,
    }))
}

/// Decodes an `error`'s fields.
fn decode_error(map: Map<String, Value>) -> Result<Message, ProtocolError> {
    let mut f = Fields::new("error", map, ERROR_FIELDS)?;
    Ok(Message::Error(ErrorReply {
        id: f.take("id")?,
        message: f.take("message")?,
    }))
}

/// Checks that `payload_len` bytes is what `message` needs.
pub fn check_payload(message: &Message, payload_len: usize) -> Result<(), ProtocolError> {
    let expected = message.payload_len();
    if payload_len as u64 != expected {
        return Err(ProtocolError::PayloadSize {
            kind: message.kind(),
            expected,
            got: payload_len as u64,
        });
    }
    Ok(())
}

/// Decodes a frame into its message and payload; see the module docs.
pub fn decode(frame: Frame) -> Result<(Message, Vec<u8>), ProtocolError> {
    let message = decode_header(&frame.header)?;
    check_payload(&message, frame.payload.len())?;
    Ok((message, frame.payload))
}

/// Writes one frame and flushes.
pub fn write_frame(w: &mut impl Write, header: &[u8], payload: &[u8]) -> Result<(), ProtocolError> {
    if header.len() > MAX_HEADER_BYTES as usize {
        return Err(ProtocolError::HeaderTooLarge(header.len() as u64));
    }
    if payload.len() > MAX_PAYLOAD_BYTES as usize {
        return Err(ProtocolError::PayloadTooLarge(payload.len() as u64));
    }
    // Both lengths fit in u32: they are below the limits.
    w.write_all(&(header.len() as u32).to_le_bytes())?;
    w.write_all(header)?;
    w.write_all(&(payload.len() as u32).to_le_bytes())?;
    w.write_all(payload)?;
    w.flush()?;
    Ok(())
}

/// Encodes and writes `message` with its `payload`.
pub fn write_message(
    w: &mut impl Write,
    message: &Message,
    payload: &[u8],
) -> Result<(), ProtocolError> {
    let header = encode_header(message)?;
    check_payload(message, payload.len())?;
    write_frame(w, &header, payload)
}

/// Reads one frame. `Ok(None)` when the stream ends cleanly before a
/// frame; see the module docs.
pub fn read_frame(r: &mut impl Read) -> Result<Option<Frame>, ProtocolError> {
    let Some(header_len) = read_len(r, None)? else {
        return Ok(None);
    };
    if header_len > MAX_HEADER_BYTES {
        return Err(ProtocolError::HeaderTooLarge(header_len.into()));
    }
    let header = read_exactly(r, header_len, "header")?;
    let payload_len =
        read_len(r, Some("payload length"))?.ok_or(ProtocolError::Truncated("payload length"))?;
    if payload_len > MAX_PAYLOAD_BYTES {
        return Err(ProtocolError::PayloadTooLarge(payload_len.into()));
    }
    let payload = read_exactly(r, payload_len, "payload")?;
    Ok(Some(Frame { header, payload }))
}

/// Reads and decodes one message. `Ok(None)` on a clean end of stream.
pub fn read_message(r: &mut impl Read) -> Result<Option<(Message, Vec<u8>)>, ProtocolError> {
    read_frame(r)?.map(decode).transpose()
}

/// Reads a little-endian `u32` length. A stream that ends before its first
/// byte gives `None` when `inside` is `None` (between frames) and
/// [`ProtocolError::Truncated`] otherwise; ending after it is always
/// truncation.
fn read_len(r: &mut impl Read, inside: Option<&'static str>) -> Result<Option<u32>, ProtocolError> {
    let what = inside.unwrap_or("header length");
    let mut buf = [0u8; 4];
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) if filled == 0 && inside.is_none() => return Ok(None),
            Ok(0) => return Err(ProtocolError::Truncated(what)),
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(Some(u32::from_le_bytes(buf)))
}

/// Reads exactly `len` bytes (already checked against a limit), growing
/// the buffer only as bytes arrive.
fn read_exactly(r: &mut impl Read, len: u32, what: &'static str) -> Result<Vec<u8>, ProtocolError> {
    let len = u64::from(len);
    let mut buf = Vec::with_capacity(len.min(1 << 16) as usize);
    r.by_ref().take(len).read_to_end(&mut buf)?;
    if (buf.len() as u64) < len {
        return Err(ProtocolError::Truncated(what));
    }
    Ok(buf)
}

#[cfg(test)]
mod tests;
