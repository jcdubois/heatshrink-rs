#![crate_type = "rlib"]
#![no_std]
#![deny(warnings)]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Minimal compression & decompression library for embedded use.
//!
//! Implements the Heatshrink compression algorithm (LZSS-based).
//! See <https://github.com/atomicobject/heatshrink> for the original C library.
//!
//! # Parameters
//!
//! Both the encoder and decoder are parameterised by const generics:
//!
//! - `W`   : base-2 log of the LZSS sliding window size (4–14).
//! - `L`   : number of bits for back-reference lengths (3 ≤ L < W).
//! - `I`   : (decoder) streaming input buffer size in bytes (≥ 1).
//! - `BUF` : (encoder) total input buffer = `2 << W` bytes. **Must equal `2 << W`.**
//! - `WIN` : (decoder) window buffer = `1 << W` bytes. **Must equal `1 << W`.**
//!
//! The `BUF` and `WIN` parameters are a workaround for the current Rust stable
//! limitation that prevents arithmetic expressions on const generics in array
//! sizes. They are always derived from `W` and are hidden behind type aliases.
//!
//! # Convenience aliases
//!
//! [`DefaultEncoder`] and [`DefaultDecoder`] use W=8, L=4, I=32, matching the
//! original C library. Prefer these unless you need custom parameters.
//!
//! # Custom parameters example
//!
//! ```rust
//! use heatshrink::encoder::HeatshrinkEncoder;
//! use heatshrink::decoder::HeatshrinkDecoder;
//!
//! // W=10, L=5: BUF = 2<<10 = 2048, WIN = 1<<10 = 1024
//! type MyEncoder = HeatshrinkEncoder<10, 5, 2048>;
//! type MyDecoder = HeatshrinkDecoder<10, 5, 64, 1024>;
//! ```

/// Module to decompress compressed data.
pub mod decoder;
/// Module to compress data.
pub mod encoder;

/// Default window size in bits — matches the original C library.
pub const DEFAULT_WINDOW_BITS: usize = 8;
/// Default lookahead size in bits — matches the original C library.
pub const DEFAULT_LOOKAHEAD_BITS: usize = 4;
/// Default decoder input buffer size in bytes — matches the original C library.
pub const DEFAULT_INPUT_BUFFER_SIZE: usize = 32;

/// Encoder using the original C library parameters (W=8, L=4).
///
/// `BUF = 2 << 8 = 512`.
pub type DefaultEncoder = encoder::HeatshrinkEncoder<
    DEFAULT_WINDOW_BITS,
    DEFAULT_LOOKAHEAD_BITS,
    512, // 2 << DEFAULT_WINDOW_BITS
>;

/// Decoder using the original C library parameters (W=8, L=4, I=32).
///
/// `WIN = 1 << 8 = 256`.
pub type DefaultDecoder = decoder::HeatshrinkDecoder<
    DEFAULT_WINDOW_BITS,
    DEFAULT_LOOKAHEAD_BITS,
    DEFAULT_INPUT_BUFFER_SIZE,
    256, // 1 << DEFAULT_WINDOW_BITS
>;

/// Return code for `sink()` calls.
#[derive(Debug)]
pub enum HSsinkRes {
    /// Instance is not in the correct state (e.g. `sink()` called after `finish()`).
    SinkErrorMisuse,
    /// Internal buffer is full; no data was added. Drain with `poll()` first.
    SinkFull,
    /// Data successfully added. Value is the number of bytes consumed.
    SinkOK(usize),
}

/// Return code for `poll()` calls.
#[derive(Debug, PartialEq, Eq)]
pub enum HSpollRes {
    /// Invalid call (e.g. empty output buffer).
    PollErrorMisuse,
    /// Output buffer is full; more data is available. Value is bytes written.
    PollMore(usize),
    /// Internal buffer fully drained. Value is bytes written.
    PollEmpty(usize),
}

/// Return code for `finish()` calls.
#[derive(Debug)]
pub enum HSfinishRes {
    /// More data remains; call `poll()` then `finish()` again.
    FinishMore,
    /// Stream is complete.
    FinishDone,
}

/// Error returned by [`encoder::encode`] and [`decoder::decode`].
#[derive(Debug)]
pub enum HSError {
    /// Output buffer was too small to hold the result.
    OutputFull,
    /// Internal error (should not occur in normal use).
    Internal,
}

/// Internal helper: tracks how much of an output slice has been written.
pub struct OutputInfo<'a> {
    output_buffer: &'a mut [u8],
    /// Number of bytes written so far.
    pub output_size: usize,
}

impl<'a> OutputInfo<'a> {
    #[inline]
    pub(crate) fn new(output_buffer: &'a mut [u8]) -> Self {
        OutputInfo {
            output_buffer,
            output_size: 0,
        }
    }

    #[inline]
    pub(crate) fn push_byte(&mut self, byte: u8) {
        self.output_buffer[self.output_size] = byte;
        self.output_size += 1;
    }

    #[inline]
    pub(crate) fn can_take_byte(&self) -> bool {
        self.output_size < self.output_buffer.len()
    }

    #[inline]
    pub(crate) fn remaining_free_size(&self) -> usize {
        self.output_buffer.len() - self.output_size
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────
//
// Three layers of coverage:
//
//  1. `test`          — regression fixtures for the default parameters (W=8,
//                       L=4) plus a clib compatibility vector.
//
//  2. `test_streaming` — a generic streaming round-trip harness used for every
//                        (W, L) pair exercised below.  Uses a small I (16) to
//                        force many sink/poll cycles and stress buffer-refill
//                        paths, plus I=32 which matches heatshrink-bin.
//
//  3. `test_params`   — spot-checks a representative set of (W, L) pairs,
//                       including the boundary cases that triggered some bugs.

#[cfg(test)]
mod test {
    use super::{decoder, encoder};

    /// Round-trip via the convenience `encode` / `decode` free functions
    /// (DefaultEncoder / DefaultDecoder, W=8 L=4).
    fn compare(src: &[u8]) {
        let mut compressed_buffer: [u8; 512] = [0; 512];
        let mut uncompressed_buffer: [u8; 1024] = [0; 1024];
        let out1 = encoder::encode(src, &mut compressed_buffer).unwrap();
        let out2 = decoder::decode(out1, &mut uncompressed_buffer).unwrap();
        assert_eq!(src, out2);
    }

    #[test]
    fn alpha() {
        let src = [
            33, 82, 149, 84, 52, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 147, 2, 0, 0, 0, 0, 0, 0, 242, 2, 241, 2, 240,
            2, 0, 0, 0, 0, 0, 0, 47, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0,
        ];
        compare(&src);
    }

    #[test]
    fn alpha2() {
        let src = [
            33, 82, 149, 84, 52, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 147, 2, 0, 0, 0, 0, 0, 0, 242, 2, 241, 2, 240,
            2, 0, 0, 0, 0, 0, 0, 47, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            12, 17,
        ];
        compare(&src);
    }

    #[test]
    fn beta() {
        let src = [
            189, 160, 51, 163, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 199, 0, 0, 0, 0, 0, 0, 0, 166, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 154, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0,
        ];
        compare(&src);
    }

    #[test]
    fn beta2() {
        // Two full 0..=255 ramps concatenated.
        let src: [u8; 512] = core::array::from_fn(|i| i as u8);
        compare(&src);
    }

    #[test]
    fn clib_compatibility() {
        use hex_literal::hex;
        let src = hex!("90D4B2B549A4082BE00F000E4C46DF2817C605F005B4BE0825F00280");
        let expected = hex!(
            "21529554340200000000000000000000000000000000000000000000000000000000000000000 0009302000000000000F202F102F0020000000000002F0400000000000000000000000000000000000000000000"
        );
        let mut dst = [0u8; 100];
        let out = decoder::decode(&src, &mut dst).unwrap();
        assert_eq!(expected, out);
    }
}

// ─── Generic streaming harness ────────────────────────────────────────────────

#[cfg(test)]
mod test_streaming {
    use super::*;
    use decoder::HeatshrinkDecoder;
    use encoder::HeatshrinkEncoder;

    /// Encode `src` with `HeatshrinkEncoder<W,L,BUF>` using the streaming API.
    /// Returns the number of compressed bytes written into `dst`.
    pub(super) fn stream_encode<const W: usize, const L: usize, const BUF: usize>(
        src: &[u8],
        dst: &mut [u8],
    ) -> usize {
        let mut enc = HeatshrinkEncoder::<W, L, BUF>::new();
        let mut total_in = 0;
        let mut total_out = 0;
        loop {
            if total_in < src.len() {
                match enc.sink(&src[total_in..]) {
                    HSsinkRes::SinkOK(n) => total_in += n,
                    HSsinkRes::SinkFull => {}
                    HSsinkRes::SinkErrorMisuse => panic!("encoder sink misuse"),
                }
            }
            if total_in == src.len() {
                enc.finish();
            }
            match enc.poll(&mut dst[total_out..]) {
                HSpollRes::PollMore(n) => total_out += n,
                HSpollRes::PollEmpty(n) => {
                    total_out += n;
                    if total_in == src.len() {
                        break;
                    }
                }
                HSpollRes::PollErrorMisuse => panic!("encoder poll misuse"),
            }
        }
        total_out
    }

    /// Decode `encoded` with `HeatshrinkDecoder<W,L,I,WIN>` using the streaming
    /// API.  `I` is deliberately kept small (caller's choice) to maximise the
    /// number of sink/poll cycles and stress buffer-refill edge cases.
    ///
    /// Returns the number of decompressed bytes written into `dst`.
    /// Panics with a descriptive message if the loop runs more than 1 000 000
    /// iterations (infinite-loop guard).
    pub(super) fn stream_decode<
        const W: usize,
        const L: usize,
        const I: usize,
        const WIN: usize,
    >(
        encoded: &[u8],
        dst: &mut [u8],
    ) -> usize {
        let mut dec = HeatshrinkDecoder::<W, L, I, WIN>::new();
        let mut total_in = 0;
        let mut total_out = 0;
        let mut iters = 0usize;
        loop {
            iters += 1;
            assert!(
                iters < 1_000_000,
                "stream_decode infinite loop (W={W} L={L} I={I}): \
                 iter={iters} total_in={total_in}/{} total_out={total_out}",
                encoded.len()
            );
            if total_in < encoded.len() {
                match dec.sink(&encoded[total_in..]) {
                    HSsinkRes::SinkOK(n) => total_in += n,
                    HSsinkRes::SinkFull => {}
                    HSsinkRes::SinkErrorMisuse => panic!("decoder sink misuse"),
                }
            }
            match dec.poll(&mut dst[total_out..]) {
                HSpollRes::PollMore(n) => total_out += n,
                HSpollRes::PollEmpty(n) => {
                    total_out += n;
                    if total_in == encoded.len() {
                        break;
                    }
                }
                HSpollRes::PollErrorMisuse => panic!("decoder poll misuse"),
            }
        }
        total_out
    }

    /// Full round-trip: encode then decode with two different values of I
    /// (16 and 32) to cover both tight and relaxed buffer-refill paths.
    pub(super) fn roundtrip<const W: usize, const L: usize, const BUF: usize, const WIN: usize>(
        src: &[u8],
    ) {
        let mut compressed = [0u8; 32768];
        let mut decompressed = [0u8; 32768];

        let n_enc = stream_encode::<W, L, BUF>(src, &mut compressed);
        let encoded = &compressed[..n_enc];

        // I=16 — many short sink() calls, stresses buffer-boundary handling.
        let n_dec16 = stream_decode::<W, L, 16, WIN>(encoded, &mut decompressed);
        assert_eq!(
            src,
            &decompressed[..n_dec16],
            "W={W} L={L} I=16 roundtrip failed"
        );

        // I=32 — matches the value used by heatshrink-bin.
        decompressed = [0u8; 32768];
        let n_dec32 = stream_decode::<W, L, 32, WIN>(encoded, &mut decompressed);
        assert_eq!(
            src,
            &decompressed[..n_dec32],
            "W={W} L={L} I=32 roundtrip failed"
        );
    }
}

// ─── Parametric round-trip tests ─────────────────────────────────────────────
//
// Each test exercises one (W, L) pair with three payloads:
//   • a short human-readable string  (< 32 bytes, fits in one sink)
//   • an 8 KB repetitive sequence    (many back-references)
//   • an 8 KB pseudo-random sequence (mostly literals, stresses get_bits)
//
// The pairs are chosen to cover:
//   • the default configuration         (W=8,  L=4)
//   • the minimum configuration         (W=4,  L=3)
//   • the maximum configuration         (W=15, L=14)
//   • W≤8 one-state index path          (W=6,  L=3)
//   • L>8 multi-byte count path         (W=10, L=9)
//   • large W and large L               (W=11, L=10)
//   • a mid-range pair                  (W=12, L=7)

#[cfg(test)]
mod test_params {
    use super::test_streaming::roundtrip;

    fn payloads() -> (&'static [u8], [u8; 8192], [u8; 8192]) {
        let short = b"hello heatshrink - parametric test" as &[u8];
        let repetitive: [u8; 8192] = core::array::from_fn(|i| (i % 251) as u8);
        let pseudo_random: [u8; 8192] = core::array::from_fn(|i| {
            (i.wrapping_mul(6364136223846793005usize)
                .wrapping_add(1442695040888963407)
                >> 56) as u8
        });
        (short, repetitive, pseudo_random)
    }

    macro_rules! param_test {
        ($name:ident, W=$w:literal, L=$l:literal) => {
            #[test]
            fn $name() {
                const BUF: usize = 2 << $w;
                const WIN: usize = 1 << $w;
                let (short, rep, rnd) = payloads();
                roundtrip::<$w, $l, BUF, WIN>(short);
                roundtrip::<$w, $l, BUF, WIN>(&rep);
                roundtrip::<$w, $l, BUF, WIN>(&rnd);
            }
        };
    }

    param_test!(default_w8_l4, W = 8, L = 4);
    param_test!(minimum_w4_l3, W = 4, L = 3);
    param_test!(maximum_w15_l14, W = 15, L = 14);
    param_test!(bug_b5_w6_l3, W = 6, L = 3);
    param_test!(bug_b6_w10_l9, W = 10, L = 9);
    param_test!(bug_b8_w11_l10, W = 11, L = 10);
    param_test!(midrange_w12_l7, W = 12, L = 7);
}
