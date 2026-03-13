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

/// [`embedded_io`](::embedded_io) adapters for the encoder and decoder.
///
/// Enabled by the `embedded-io` Cargo feature.
#[cfg(feature = "embedded-io")]
pub mod io;

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

/// Error returned by [`encoder::HeatshrinkEncoder::sink`] and
/// [`decoder::HeatshrinkDecoder::sink`].
#[derive(Debug, PartialEq, Eq)]
pub enum SinkError {
    /// Internal buffer is full; no data was consumed. Drain with `poll()` first.
    Full,
    /// API misuse: `sink()` called in the wrong state (e.g. after `finish()`).
    Misuse,
}

/// Error returned by [`encoder::HeatshrinkEncoder::poll`] and
/// [`decoder::HeatshrinkDecoder::poll`].
///
/// Only one variant exists: passing an empty output buffer is always a
/// programming error.
#[derive(Debug, PartialEq, Eq)]
pub enum PollError {
    /// API misuse: `poll()` was called with an empty output buffer.
    Misuse,
}

/// Outcome of a successful [`encoder::HeatshrinkEncoder::poll`] or
/// [`decoder::HeatshrinkDecoder::poll`] call.
#[derive(Debug, PartialEq, Eq)]
pub enum Poll {
    /// Output buffer is full; more compressed/decompressed data is available.
    /// Value is the number of bytes written into the output buffer.
    More(usize),
    /// Internal state is fully drained for now.
    /// Value is the number of bytes written into the output buffer.
    Empty(usize),
}

impl Poll {
    /// Number of bytes written into the output buffer, regardless of variant.
    #[inline]
    pub fn bytes_written(&self) -> usize {
        match self {
            Poll::More(n) | Poll::Empty(n) => *n,
        }
    }
}

/// Outcome of a [`encoder::HeatshrinkEncoder::finish`] or
/// [`decoder::HeatshrinkDecoder::finish`] call.
#[derive(Debug, PartialEq, Eq)]
pub enum Finish {
    /// Stream is complete; no further `poll()` calls are needed.
    Done,
    /// More output remains; call `poll()` until it returns [`Poll::Empty`],
    /// then call `finish()` again.
    More,
}

/// Error returned by the convenience functions [`encoder::encode`] and
/// [`decoder::decode`].
#[derive(Debug)]
pub enum EncodeError {
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
    use super::{Poll, SinkError};
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
                    Ok(n) => total_in += n,
                    Err(SinkError::Full) => {}
                    Err(SinkError::Misuse) => panic!("encoder sink misuse"),
                }
            }
            if total_in == src.len() {
                enc.finish();
            }
            match enc.poll(&mut dst[total_out..]) {
                Ok(Poll::More(n)) => total_out += n,
                Ok(Poll::Empty(n)) => {
                    total_out += n;
                    if total_in == src.len() {
                        break;
                    }
                }
                Err(_) => panic!("encoder poll misuse"),
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
                    Ok(n) => total_in += n,
                    Err(SinkError::Full) => {}
                    Err(SinkError::Misuse) => panic!("decoder sink misuse"),
                }
            }
            match dec.poll(&mut dst[total_out..]) {
                Ok(Poll::More(n)) => total_out += n,
                Ok(Poll::Empty(n)) => {
                    total_out += n;
                    if total_in == encoded.len() {
                        break;
                    }
                }
                Err(_) => panic!("decoder poll misuse"),
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

// ─── embedded-io adapter tests ───────────────────────────────────────────────

#[cfg(all(test, feature = "embedded-io"))]
mod test_io {
    use super::io::{
        DecoderReader, DecoderWriter, EncoderReader, EncoderWriter, SliceSink, SliceSource,
    };
    use super::{DefaultDecoder, DefaultEncoder};
    use embedded_io::{Read as _, Write as _};

    // ── helpers ──────────────────────────────────────────────────────────────

    /// Compress `src` via EncoderWriter into a heap-less slice, return byte count.
    fn ew_compress(src: &[u8], dst: &mut [u8]) -> usize {
        let mut sink = SliceSink::new(dst);
        let mut w: EncoderWriter<_, DefaultEncoder> = EncoderWriter::new(&mut sink);
        w.write_all(src).unwrap();
        w.finish().unwrap();
        sink.len()
    }

    /// Decompress `src` via DecoderWriter into a slice, return byte count.
    fn dw_decompress(src: &[u8], dst: &mut [u8]) -> usize {
        let mut sink = SliceSink::new(dst);
        let mut w: DecoderWriter<_, DefaultDecoder> = DecoderWriter::new(&mut sink);
        w.write_all(src).unwrap();
        w.finish().unwrap();
        sink.len()
    }

    /// Decompress `src` via DecoderReader into a slice, return byte count.
    fn dr_decompress(src: &[u8], dst: &mut [u8]) -> usize {
        let source = SliceSource::new(src);
        let mut r: DecoderReader<_, DefaultDecoder> = DecoderReader::new(source);
        let mut total = 0;
        loop {
            let n = r.read(&mut dst[total..]).unwrap();
            if n == 0 {
                break;
            }
            total += n;
        }
        total
    }

    /// Compress `src` via EncoderReader into a slice, return byte count.
    fn er_compress(src: &[u8], dst: &mut [u8]) -> usize {
        let source = SliceSource::new(src);
        let mut r: EncoderReader<_, DefaultEncoder> = EncoderReader::new(source);
        let mut total = 0;
        loop {
            let n = r.read(&mut dst[total..]).unwrap();
            if n == 0 {
                break;
            }
            total += n;
        }
        total
    }

    // ── round-trip tests ─────────────────────────────────────────────────────

    /// EncoderWriter → DecoderWriter round-trip.
    #[test]
    fn writer_roundtrip_short() {
        let src = b"hello heatshrink embedded-io";
        let mut compressed = [0u8; 256];
        let mut decompressed = [0u8; 256];

        let n_enc = ew_compress(src, &mut compressed);
        let n_dec = dw_decompress(&compressed[..n_enc], &mut decompressed);

        assert_eq!(src, &decompressed[..n_dec]);
    }

    /// EncoderWriter → DecoderWriter round-trip with repetitive data (back-refs).
    #[test]
    fn writer_roundtrip_repetitive() {
        let src: [u8; 4096] = core::array::from_fn(|i| (i % 251) as u8);
        let mut compressed = [0u8; 4096];
        let mut decompressed = [0u8; 4096];

        let n_enc = ew_compress(&src, &mut compressed);
        let n_dec = dw_decompress(&compressed[..n_enc], &mut decompressed);

        assert_eq!(&src[..], &decompressed[..n_dec]);
    }

    /// EncoderWriter → DecoderWriter round-trip with pseudo-random data (literals).
    #[test]
    fn writer_roundtrip_random() {
        let src: [u8; 4096] = core::array::from_fn(|i| {
            (i.wrapping_mul(6364136223846793005usize)
                .wrapping_add(1442695040888963407)
                >> 56) as u8
        });
        let mut compressed = [0u8; 8192];
        let mut decompressed = [0u8; 8192];

        let n_enc = ew_compress(&src, &mut compressed);
        let n_dec = dw_decompress(&compressed[..n_enc], &mut decompressed);

        assert_eq!(&src[..], &decompressed[..n_dec]);
    }

    /// EncoderWriter → DecoderReader round-trip.
    #[test]
    fn writer_reader_roundtrip() {
        let src = b"the quick brown fox jumps over the lazy dog";
        let mut compressed = [0u8; 256];
        let mut decompressed = [0u8; 256];

        let n_enc = ew_compress(src, &mut compressed);
        let n_dec = dr_decompress(&compressed[..n_enc], &mut decompressed);

        assert_eq!(src, &decompressed[..n_dec]);
    }

    /// EncoderReader → DecoderWriter round-trip.
    #[test]
    fn reader_writer_roundtrip() {
        let src = b"the quick brown fox jumps over the lazy dog";
        let mut compressed = [0u8; 256];
        let mut decompressed = [0u8; 256];

        let n_enc = er_compress(src, &mut compressed);
        let n_dec = dw_decompress(&compressed[..n_enc], &mut decompressed);

        assert_eq!(src, &decompressed[..n_dec]);
    }

    /// EncoderReader → DecoderReader round-trip.
    #[test]
    fn reader_reader_roundtrip() {
        let src: [u8; 4096] = core::array::from_fn(|i| (i % 251) as u8);
        let mut compressed = [0u8; 4096];
        let mut decompressed = [0u8; 4096];

        let n_enc = er_compress(&src, &mut compressed);
        let n_dec = dr_decompress(&compressed[..n_enc], &mut decompressed);

        assert_eq!(&src[..], &decompressed[..n_dec]);
    }

    /// Small caller buffer forces the Read adapters to exercise the internal
    /// output buffer (out_start / out_end bookkeeping).
    #[test]
    fn reader_small_output_buf() {
        let src: [u8; 512] = core::array::from_fn(|i| (i % 137) as u8);
        let mut compressed = [0u8; 1024];
        let mut decompressed = [0u8; 1024];

        // Compress with EncoderReader, read 1 byte at a time.
        let source = SliceSource::new(&src);
        let mut r: EncoderReader<_, DefaultEncoder> = EncoderReader::new(source);
        let mut n_enc = 0;
        loop {
            let n = r.read(&mut compressed[n_enc..n_enc + 1]).unwrap();
            if n == 0 {
                break;
            }
            n_enc += n;
        }

        // Decompress with DecoderReader, read 3 bytes at a time.
        let source2 = SliceSource::new(&compressed[..n_enc]);
        let mut r2: DecoderReader<_, DefaultDecoder> = DecoderReader::new(source2);
        let mut n_dec = 0;
        let mut tmp = [0u8; 3];
        loop {
            let n = r2.read(&mut tmp).unwrap();
            if n == 0 {
                break;
            }
            decompressed[n_dec..n_dec + n].copy_from_slice(&tmp[..n]);
            n_dec += n;
        }

        assert_eq!(&src[..], &decompressed[..n_dec]);
    }

    /// Empty input must produce a valid (empty) round-trip.
    #[test]
    fn writer_roundtrip_empty() {
        let src: &[u8] = &[];
        let mut compressed = [0u8; 64];
        let mut decompressed = [0u8; 64];

        let n_enc = ew_compress(src, &mut compressed);
        let n_dec = dw_decompress(&compressed[..n_enc], &mut decompressed);

        assert_eq!(src, &decompressed[..n_dec]);
    }
}
