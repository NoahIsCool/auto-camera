//! Codec tests: golden headers from the protocol text, round trips, every
//! strictness rule, truncation at every byte and seeded fuzzing.

use std::io::{self, Cursor, Read};

use super::*;

/// A valid hello.
fn hello() -> Message {
    Message::Hello(Hello {
        protocol: 1,
        model: "sam3.1-mlx".into(),
        weights_sha256: Some("ab".repeat(32)),
        input_side: 672,
        capabilities: Capabilities::default(),
    })
}

/// A valid 2x1 detect with two phrases.
fn detect() -> Message {
    Message::Detect(Detect {
        id: 7,
        source: "pulpit-cam".into(),
        seq: 41,
        pts_ns: 1_366_666_666,
        width: 2,
        height: 1,
        format: PixelFormat::Rgb8,
        phrases: vec![
            Phrase {
                text: "a person speaking".into(),
                threshold: 0.3,
            },
            Phrase {
                text: "a guitar".into(),
                threshold: 0.5,
            },
        ],
        want_masks: false,
    })
}

/// A valid result with two detections.
fn result() -> Message {
    Message::Result(DetectResult {
        id: 7,
        source: "pulpit-cam".into(),
        seq: 41,
        pts_ns: 1_366_666_666,
        latency_ms: 482.5,
        detections: vec![
            Found {
                phrase: 0,
                bbox: [0.25, 0.1, 0.2, 0.6],
                score: 0.71,
            },
            Found {
                phrase: 1,
                bbox: [0.0, 0.5, 1.0, 0.5],
                score: 0.5,
            },
        ],
    })
}

/// A valid per-request error.
fn error() -> Message {
    Message::Error(ErrorReply {
        id: Some(7),
        message: "out of memory".into(),
    })
}

/// The bytes of one frame.
fn frame_bytes(header: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    write_frame(&mut out, header, payload).unwrap();
    out
}

/// Decodes a header given as text.
fn parse(text: &str) -> Result<Message, ProtocolError> {
    decode_header(text.as_bytes())
}

/// `text` with `key`'s value replaced by `value` (a JSON fragment), or the
/// key removed when `value` is `None`.
fn with(text: &str, key: &str, value: Option<&str>) -> String {
    let mut map: Map<String, Value> = serde_json::from_str(text).unwrap();
    match value {
        Some(v) => map.insert(key.into(), serde_json::from_str(v).unwrap()),
        None => map.remove(key),
    };
    serde_json::to_string(&map).unwrap()
}

/// The protocol text's hello, verbatim apart from the placeholders.
const HELLO: &str = r#"{"type":"hello","protocol":1,"model":"sam3.1-mlx","weights_sha256":null,"input_side":672,"capabilities":{"masks":false,"parts":false}}"#;

/// The protocol text's detect.
const DETECT: &str = r#"{"type":"detect","id":7,"source":"pulpit-cam","seq":41,"pts_ns":1366666666,"width":2,"height":1,"format":"rgb8","phrases":[{"text":"a person speaking","threshold":0.3}],"want_masks":false}"#;

/// The protocol text's result.
const RESULT: &str = r#"{"type":"result","id":7,"source":"pulpit-cam","seq":41,"pts_ns":1366666666,"latency_ms":482.5,"detections":[{"phrase":0,"box":[0.25,0.1,0.2,0.6],"score":0.71}]}"#;

/// The protocol text's error.
const ERROR: &str = r#"{"type":"error","id":null,"message":"weights missing"}"#;

#[test]
fn golden_headers_decode() {
    let Message::Hello(h) = parse(HELLO).unwrap() else {
        panic!("not a hello")
    };
    assert_eq!((h.protocol, h.input_side, h.weights_sha256), (1, 672, None));
    assert_eq!(h.model, "sam3.1-mlx");
    let Message::Detect(d) = parse(DETECT).unwrap() else {
        panic!("not a detect")
    };
    assert_eq!(
        (d.id, d.seq, d.pts_ns, d.width, d.height),
        (7, 41, 1_366_666_666, 2, 1)
    );
    assert_eq!(d.payload_len(), 6);
    assert_eq!(d.phrases[0].threshold, 0.3);
    let Message::Result(r) = parse(RESULT).unwrap() else {
        panic!("not a result")
    };
    assert_eq!(r.detections[0].bbox, [0.25, 0.1, 0.2, 0.6]);
    assert_eq!(r.detections[0].score, 0.71);
    assert_eq!(
        parse(ERROR).unwrap(),
        Message::Error(ErrorReply {
            id: None,
            message: "weights missing".into()
        })
    );
}

#[test]
fn encoding_spells_out_nulls_and_the_type_tag() {
    let text = String::from_utf8(encode_header(&parse(HELLO).unwrap()).unwrap()).unwrap();
    assert_eq!(text, HELLO);
    let text = String::from_utf8(encode_header(&parse(ERROR).unwrap()).unwrap()).unwrap();
    assert_eq!(text, ERROR);
    let text = String::from_utf8(encode_header(&parse(DETECT).unwrap()).unwrap()).unwrap();
    assert_eq!(text, DETECT);
}

#[test]
fn every_message_round_trips_through_a_stream() {
    let messages = [hello(), detect(), result(), error()];
    let mut stream = Vec::new();
    for m in &messages {
        let payload = vec![9u8; m.payload_len() as usize];
        write_message(&mut stream, m, &payload).unwrap();
    }
    let mut r = Cursor::new(stream);
    for m in &messages {
        let (got, payload) = read_message(&mut r).unwrap().unwrap();
        assert_eq!(&got, m);
        assert_eq!(payload.len() as u64, m.payload_len());
    }
    assert!(read_message(&mut r).unwrap().is_none(), "clean end");
}

#[test]
fn frames_are_little_endian_length_prefixed() {
    let bytes = frame_bytes(b"{}", &[1, 2, 3]);
    assert_eq!(bytes, [2, 0, 0, 0, b'{', b'}', 3, 0, 0, 0, 1, 2, 3]);
    let frame = read_frame(&mut Cursor::new(bytes)).unwrap().unwrap();
    assert_eq!(frame.header, b"{}");
    assert_eq!(frame.payload, [1, 2, 3]);
}

#[test]
fn lengths_over_the_limits_are_refused_before_reading_them() {
    let mut huge_header = (MAX_HEADER_BYTES + 1).to_le_bytes().to_vec();
    huge_header.extend_from_slice(b"{}");
    assert!(matches!(
        read_frame(&mut Cursor::new(huge_header)),
        Err(ProtocolError::HeaderTooLarge(n)) if n == u64::from(MAX_HEADER_BYTES) + 1
    ));
    let mut huge_payload = frame_bytes(b"{}", &[]);
    huge_payload.truncate(6);
    huge_payload.extend_from_slice(&(MAX_PAYLOAD_BYTES + 1).to_le_bytes());
    assert!(matches!(
        read_frame(&mut Cursor::new(huge_payload)),
        Err(ProtocolError::PayloadTooLarge(_))
    ));
    // "this" read as a length is 1.9 GB.
    assert!(matches!(
        read_frame(&mut Cursor::new(b"this is not a frame".to_vec())),
        Err(ProtocolError::HeaderTooLarge(_))
    ));
}

#[test]
fn a_header_at_the_limit_is_accepted_and_one_over_is_not() {
    let at = vec![b' '; MAX_HEADER_BYTES as usize];
    let frame = read_frame(&mut Cursor::new(frame_bytes(&at, &[])))
        .unwrap()
        .unwrap();
    assert_eq!(frame.header.len(), MAX_HEADER_BYTES as usize);
    let over = vec![b' '; MAX_HEADER_BYTES as usize + 1];
    assert!(matches!(
        write_frame(&mut Vec::new(), &over, &[]),
        Err(ProtocolError::HeaderTooLarge(_))
    ));
    assert!(matches!(
        decode_header(&over),
        Err(ProtocolError::HeaderTooLarge(_))
    ));
}

#[test]
fn every_truncation_of_a_stream_is_reported() {
    let mut stream = Vec::new();
    write_message(&mut stream, &hello(), &[]).unwrap();
    let boundary = stream.len();
    write_message(&mut stream, &detect(), &[1, 2, 3, 4, 5, 6]).unwrap();
    for cut in 0..stream.len() {
        let mut r = Cursor::new(&stream[..cut]);
        let first = read_message(&mut r);
        match cut {
            0 => assert!(first.unwrap().is_none(), "empty stream is a clean end"),
            c if c < boundary => {
                assert!(matches!(first, Err(ProtocolError::Truncated(_))), "cut {c}")
            }
            c => {
                assert!(first.unwrap().is_some());
                let second = read_message(&mut r);
                if c == boundary {
                    assert!(second.unwrap().is_none(), "cut between frames");
                } else {
                    assert!(
                        matches!(second, Err(ProtocolError::Truncated(_))),
                        "cut {c}"
                    );
                }
            }
        }
    }
}

/// A reader that hands out one byte at a time and is interrupted between
/// bytes.
struct Stutter {
    /// The bytes still to read.
    data: Cursor<Vec<u8>>,
    /// Whether the next call is interrupted.
    interrupt: bool,
}

impl Read for Stutter {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.interrupt = !self.interrupt;
        if self.interrupt {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let n = buf.len().min(1);
        self.data.read(&mut buf[..n])
    }
}

#[test]
fn short_and_interrupted_reads_assemble_frames() {
    let mut stream = Vec::new();
    write_message(&mut stream, &result(), &[]).unwrap();
    let mut r = Stutter {
        data: Cursor::new(stream),
        interrupt: false,
    };
    assert_eq!(read_message(&mut r).unwrap().unwrap().0, result());
    assert!(read_message(&mut r).unwrap().is_none());
}

#[test]
fn malformed_headers_are_named() {
    assert!(matches!(
        decode_header(&[0xff, 0xfe]),
        Err(ProtocolError::NotUtf8)
    ));
    assert!(matches!(parse("{"), Err(ProtocolError::Json(_))));
    assert!(matches!(parse("[1]"), Err(ProtocolError::NotObject)));
    assert!(matches!(parse("{}"), Err(ProtocolError::MissingType)));
    assert!(matches!(
        parse(r#"{"type":5}"#),
        Err(ProtocolError::MissingType)
    ));
    assert!(matches!(
        parse(r#"{"type":"hullo"}"#),
        Err(ProtocolError::UnknownType(t)) if t == "hullo"
    ));
    assert!(matches!(
        parse(r#"{"type":"Hello"}"#),
        Err(ProtocolError::UnknownType(_))
    ));
}

#[test]
fn unknown_fields_are_rejected_at_every_level() {
    let top = with(RESULT, "masks", Some("[]"));
    assert!(matches!(
        parse(&top),
        Err(ProtocolError::UnknownField { kind: "result", field }) if field == "masks"
    ));
    let nested = with(
        DETECT,
        "phrases",
        Some(r#"[{"text":"x","threshold":0.5,"weight":2}]"#),
    );
    assert!(matches!(
        parse(&nested),
        Err(ProtocolError::BadField {
            field: "phrases",
            ..
        })
    ));
    let caps = with(
        HELLO,
        "capabilities",
        Some(r#"{"masks":false,"parts":false,"heads":true}"#),
    );
    assert!(matches!(
        parse(&caps),
        Err(ProtocolError::BadField {
            field: "capabilities",
            ..
        })
    ));
    let found = with(
        RESULT,
        "detections",
        Some(r#"[{"phrase":0,"box":[0,0,1,1],"score":0.5,"label":"x"}]"#),
    );
    assert!(matches!(parse(&found), Err(ProtocolError::BadField { .. })));
}

#[test]
fn every_field_is_required_even_when_it_may_be_null() {
    for (text, fields) in [
        (HELLO, HELLO_FIELDS),
        (DETECT, DETECT_FIELDS),
        (RESULT, RESULT_FIELDS),
        (ERROR, ERROR_FIELDS),
    ] {
        for field in fields {
            let missing = with(text, field, None);
            assert!(
                matches!(parse(&missing), Err(ProtocolError::MissingField { field: f, .. }) if f == *field),
                "{missing}"
            );
        }
    }
}

#[test]
fn wrong_json_types_are_rejected() {
    let cases = [
        (DETECT, "id", r#""7""#),
        (DETECT, "id", "-1"),
        (DETECT, "id", "7.5"),
        (DETECT, "seq", "null"),
        (DETECT, "width", "4294967296"),
        (DETECT, "format", r#""rgba8""#),
        (DETECT, "phrases", r#"["a person"]"#),
        (DETECT, "want_masks", "0"),
        (
            RESULT,
            "detections",
            r#"[{"phrase":0,"box":[0,0,1],"score":0.5}]"#,
        ),
        (
            RESULT,
            "detections",
            r#"[{"phrase":-1,"box":[0,0,1,1],"score":0.5}]"#,
        ),
        (
            RESULT,
            "detections",
            r#"[{"phrase":0,"box":[0,0,1,1],"score":"high"}]"#,
        ),
        (RESULT, "latency_ms", r#""fast""#),
        (HELLO, "weights_sha256", "12"),
        (HELLO, "model", "null"),
        (HELLO, "capabilities", r#"{"masks":false}"#),
        (ERROR, "id", r#""7""#),
        (ERROR, "message", "null"),
    ];
    for (text, field, value) in cases {
        let bad = with(text, field, Some(value));
        assert!(
            matches!(parse(&bad), Err(ProtocolError::BadField { field: f, .. }) if f == field),
            "{bad}: {:?}",
            parse(&bad)
        );
    }
}

#[test]
fn values_must_make_sense() {
    let invalid = [
        with(HELLO, "model", Some(r#""  ""#)),
        with(HELLO, "weights_sha256", Some(r#""not-a-sha""#)),
        with(HELLO, "input_side", Some("0")),
        with(DETECT, "source", Some(r#""""#)),
        with(DETECT, "width", Some("0")),
        with(DETECT, "phrases", Some("[]")),
        with(DETECT, "phrases", Some(r#"[{"text":" ","threshold":0.5}]"#)),
        with(DETECT, "phrases", Some(r#"[{"text":"x","threshold":1.5}]"#)),
        with(
            DETECT,
            "phrases",
            Some(r#"[{"text":"x","threshold":-0.1}]"#),
        ),
        with(DETECT, "want_masks", Some("true")),
        with(RESULT, "source", Some(r#""""#)),
        with(RESULT, "latency_ms", Some("-1")),
        with(
            RESULT,
            "detections",
            Some(r#"[{"phrase":0,"box":[0,0,1,1],"score":1e39}]"#),
        ),
    ];
    for bad in invalid {
        assert!(
            matches!(parse(&bad), Err(ProtocolError::Invalid { .. })),
            "{bad}: {:?}",
            parse(&bad)
        );
    }
    let huge = with(
        &with(DETECT, "width", Some("65536")),
        "height",
        Some("65536"),
    );
    assert!(matches!(
        parse(&huge),
        Err(ProtocolError::PayloadTooLarge(_))
    ));
}

#[test]
fn another_protocol_version_is_reported_as_such() {
    // A newer sidecar may add fields; the version is what to report.
    let v2 = with(
        &with(HELLO, "protocol", Some("2")),
        "masks_format",
        Some("1"),
    );
    assert!(matches!(
        parse(&v2),
        Err(ProtocolError::Version { got }) if got == "2"
    ));
    assert!(matches!(
        parse(&with(HELLO, "protocol", Some("1.0"))),
        Err(ProtocolError::Version { .. })
    ));
    assert!(matches!(
        parse(&with(HELLO, "protocol", Some(r#""1""#))),
        Err(ProtocolError::BadField {
            field: "protocol",
            ..
        })
    ));
    let Message::Hello(mut h) = hello() else {
        unreachable!()
    };
    h.protocol = 0;
    assert!(matches!(
        encode_header(&Message::Hello(h)),
        Err(ProtocolError::Version { .. })
    ));
}

#[test]
fn payloads_must_match_their_header() {
    let header = encode_header(&detect()).unwrap();
    let short = Frame {
        header: header.clone(),
        payload: vec![0; 5],
    };
    assert!(matches!(
        decode(short),
        Err(ProtocolError::PayloadSize {
            kind: "detect",
            expected: 6,
            got: 5
        })
    ));
    let hello_with_bytes = Frame {
        header: encode_header(&hello()).unwrap(),
        payload: vec![0],
    };
    assert!(matches!(
        decode(hello_with_bytes),
        Err(ProtocolError::PayloadSize { kind: "hello", .. })
    ));
    assert!(matches!(
        write_message(&mut Vec::new(), &detect(), &[0; 7]),
        Err(ProtocolError::PayloadSize { .. })
    ));
}

#[test]
fn encoding_refuses_what_decoding_refuses() {
    let Message::Detect(mut d) = detect() else {
        unreachable!()
    };
    d.phrases[0].threshold = f32::NAN;
    assert!(encode_header(&Message::Detect(d.clone())).is_err());
    d.phrases[0].threshold = 0.5;
    d.want_masks = true;
    assert!(encode_header(&Message::Detect(d)).is_err());
    let Message::Result(mut r) = result() else {
        unreachable!()
    };
    r.detections[0].bbox[2] = f64::INFINITY;
    assert!(encode_header(&Message::Result(r)).is_err());
}

#[test]
fn thresholds_survive_the_round_trip_exactly() {
    for t in [0.0f32, 0.3, 0.1 + 0.2, 0.5, 0.999_999_9, 1.0] {
        let Message::Detect(mut d) = detect() else {
            unreachable!()
        };
        d.phrases[0].threshold = t;
        let back = decode_header(&encode_header(&Message::Detect(d)).unwrap()).unwrap();
        let Message::Detect(back) = back else {
            unreachable!()
        };
        assert_eq!(back.phrases[0].threshold.to_bits(), t.to_bits());
    }
}

mod fuzz;
