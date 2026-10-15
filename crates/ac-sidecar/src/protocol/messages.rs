//! The four protocol v1 messages and their value rules; see the
//! [`protocol`](super) module docs for the wire format.

use serde::{Deserialize, Serialize};

use super::{MAX_PAYLOAD_BYTES, PROTOCOL_VERSION, ProtocolError};

/// What the sidecar's model can return besides boxes. Informational in v1:
/// the host asks for boxes only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    /// The model can return instance masks.
    pub masks: bool,
    /// The model can segment parts ("guitar neck").
    pub parts: bool,
}

/// The sidecar's greeting, sent once its model has loaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hello {
    /// Protocol version; must be [`PROTOCOL_VERSION`].
    pub protocol: u32,
    /// Model name, such as "sam3.1-mlx"; the detector reports it as its
    /// name.
    pub model: String,
    /// SHA-256 of the weights, as 64 hex digits, when the sidecar checked
    /// them.
    pub weights_sha256: Option<String>,
    /// Side of the square the model resizes images to, in pixels.
    pub input_side: u32,
    /// What the model can return besides boxes.
    pub capabilities: Capabilities,
}

/// Pixel layout of a request's payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelFormat {
    /// Three bytes per pixel, red, green, blue; rows top to bottom, no
    /// padding.
    #[serde(rename = "rgb8")]
    Rgb8,
}

impl PixelFormat {
    /// Bytes per pixel.
    pub fn bytes_per_pixel(self) -> u64 {
        match self {
            PixelFormat::Rgb8 => 3,
        }
    }
}

/// One text prompt in a request, with the score its detections need.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Phrase {
    /// The prompt, such as "a person speaking".
    pub text: String,
    /// Lowest score to return, in `0..=1`.
    pub threshold: f32,
}

/// A request for one detector pass over the payload's image.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Detect {
    /// Request id, echoed by the answer.
    pub id: u64,
    /// Source the frame came from, echoed.
    pub source: String,
    /// The frame's per-source sequence number, echoed.
    pub seq: u64,
    /// The frame's presentation time in nanoseconds, echoed.
    pub pts_ns: u64,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Pixel layout of the payload.
    pub format: PixelFormat,
    /// Prompts to look for; results index into this list.
    pub phrases: Vec<Phrase>,
    /// Whether masks are wanted; always false in v1.
    pub want_masks: bool,
}

impl Detect {
    /// Length of the payload this request needs.
    pub fn payload_len(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height) * self.format.bytes_per_pixel()
    }
}

/// One instance found by the sidecar.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Found {
    /// Index of the phrase that matched, into the request's `phrases`.
    pub phrase: u32,
    /// `[x, y, w, h]`, normalized to the image sent, origin top-left.
    #[serde(rename = "box")]
    pub bbox: [f64; 4],
    /// Match score, nominally in `0..=1`.
    pub score: f32,
}

/// The answer to a [`Detect`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DetectResult {
    /// The request's id.
    pub id: u64,
    /// The request's source.
    pub source: String,
    /// The request's sequence number.
    pub seq: u64,
    /// The request's presentation time.
    pub pts_ns: u64,
    /// Time the sidecar spent on the pass, in milliseconds.
    pub latency_ms: f64,
    /// Instances at or above their phrase's threshold.
    pub detections: Vec<Found>,
}

/// A failure reported by the sidecar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorReply {
    /// The request that failed, or `None` for a fatal error after which
    /// the sidecar exits.
    pub id: Option<u64>,
    /// What went wrong.
    pub message: String,
}

/// Any protocol v1 message.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// Child to host: the model is loaded.
    Hello(Hello),
    /// Host to child: run one pass.
    Detect(Detect),
    /// Child to host: a pass's detections.
    Result(DetectResult),
    /// Child to host: a request (or the sidecar) failed.
    Error(ErrorReply),
}

/// The wire form of a [`Message`]: its fields with a `type` tag.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum Tagged<'a> {
    /// A `hello` header.
    Hello(&'a Hello),
    /// A `detect` header.
    Detect(&'a Detect),
    /// A `result` header.
    Result(&'a DetectResult),
    /// An `error` header.
    Error(&'a ErrorReply),
}

impl Message {
    /// The message's `type` tag.
    pub fn kind(&self) -> &'static str {
        match self {
            Message::Hello(_) => "hello",
            Message::Detect(_) => "detect",
            Message::Result(_) => "result",
            Message::Error(_) => "error",
        }
    }

    /// Length of the payload that must accompany the message.
    pub fn payload_len(&self) -> u64 {
        match self {
            Message::Detect(d) => d.payload_len(),
            _ => 0,
        }
    }

    /// Checks the value rules of the module docs.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let kind = self.kind();
        let invalid = |reason: String| Err(ProtocolError::Invalid { kind, reason });
        match self {
            Message::Hello(h) => {
                if h.protocol != PROTOCOL_VERSION {
                    return Err(ProtocolError::Version {
                        got: h.protocol.to_string(),
                    });
                }
                if h.model.trim().is_empty() {
                    return invalid("model is empty".into());
                }
                if let Some(sha) = &h.weights_sha256
                    && !(sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
                {
                    return invalid(format!("weights_sha256 {sha:?} is not 64 hex digits"));
                }
                if h.input_side == 0 {
                    return invalid("input_side is 0".into());
                }
            }
            Message::Detect(d) => {
                if d.source.is_empty() {
                    return invalid("source is empty".into());
                }
                if d.width == 0 || d.height == 0 {
                    return invalid(format!("image size {}x{} is empty", d.width, d.height));
                }
                if d.payload_len() > u64::from(MAX_PAYLOAD_BYTES) {
                    return Err(ProtocolError::PayloadTooLarge(d.payload_len()));
                }
                if d.phrases.is_empty() {
                    return invalid("phrases is empty".into());
                }
                for (i, p) in d.phrases.iter().enumerate() {
                    if p.text.trim().is_empty() {
                        return invalid(format!("phrase {i} is empty"));
                    }
                    if !(p.threshold.is_finite() && (0.0..=1.0).contains(&p.threshold)) {
                        return invalid(format!(
                            "phrase {i} threshold {} is not in 0..=1",
                            p.threshold
                        ));
                    }
                }
                if d.want_masks {
                    return invalid("want_masks must be false: protocol 1 has no masks".into());
                }
            }
            Message::Result(r) => {
                if r.source.is_empty() {
                    return invalid("source is empty".into());
                }
                if !(r.latency_ms.is_finite() && r.latency_ms >= 0.0) {
                    return invalid(format!("latency_ms {} is not a duration", r.latency_ms));
                }
                for (i, f) in r.detections.iter().enumerate() {
                    if !f.bbox.iter().all(|v| v.is_finite()) || !f.score.is_finite() {
                        return invalid(format!("detection {i} has a non-finite number"));
                    }
                }
            }
            Message::Error(_) => {}
        }
        Ok(())
    }
}
