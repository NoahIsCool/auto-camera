//! Seeded fuzzing of the codec: mutated frames, mutated headers and random
//! bytes never panic, and whatever decodes satisfies the value rules.

use std::io::Cursor;

use super::*;

/// A small deterministic generator (xorshift64*), so fuzz failures replay.
struct Rng(u64);

impl Rng {
    /// The next pseudo-random number.
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A number below `n` (`n > 0`).
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn fuzzed_frames_never_panic_and_never_decode_wrongly() {
    let mut rng = Rng(0x5eed_1234_abcd_0001);
    let mut valid = Vec::new();
    for m in [hello(), detect(), result(), error()] {
        let mut bytes = Vec::new();
        write_message(&mut bytes, &m, &vec![0u8; m.payload_len() as usize]).unwrap();
        valid.push(bytes);
    }
    for round in 0..4000 {
        let mut bytes = valid[round % valid.len()].clone();
        for _ in 0..1 + rng.below(4) {
            let i = rng.below(bytes.len());
            match rng.below(3) {
                0 => bytes[i] = rng.next() as u8,
                1 => {
                    bytes.remove(i);
                }
                _ => bytes.insert(i, rng.next() as u8),
            }
        }
        let mut r = Cursor::new(bytes);
        // Whatever decodes must still satisfy the value rules.
        while let Ok(Some((m, payload))) = read_message(&mut r) {
            m.validate().unwrap();
            assert_eq!(payload.len() as u64, m.payload_len());
        }
    }
}

#[test]
fn fuzzed_headers_never_panic() {
    let mut rng = Rng(0xfeed_beef_0000_0042);
    let alphabet = br#"{}[]":,0123456789.-eEtruefalsn typeidhelodcrs"#;
    for text in [HELLO, DETECT, RESULT, ERROR] {
        for _ in 0..2000 {
            let mut bytes = text.as_bytes().to_vec();
            for _ in 0..1 + rng.below(3) {
                let i = rng.below(bytes.len());
                bytes[i] = alphabet[rng.below(alphabet.len())];
            }
            if let Ok(m) = decode_header(&bytes) {
                m.validate().unwrap();
                let again = decode_header(&encode_header(&m).unwrap()).unwrap();
                assert_eq!(again, m);
            }
        }
    }
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = Rng(7);
    for _ in 0..2000 {
        let len = rng.below(64);
        let bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        let _ = read_message(&mut Cursor::new(&bytes));
        let _ = decode_header(&bytes);
    }
}
