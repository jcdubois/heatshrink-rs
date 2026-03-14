use super::CodecError;
use super::Finish;
use super::Poll;
use super::PollError;
use super::SinkError;

#[derive(Debug, Copy, Clone, PartialEq)]
enum HSDstate {
    TagBit,
    YieldLiteral,
    BackrefIndexMsb,
    BackrefIndexLsb,
    BackrefCountLsb,
    YieldBackref,
}

/// Heatshrink decoder.
///
/// # Type parameters
///
/// - `W`   : base-2 log of the LZSS sliding window size (must match the encoder).
/// - `L`   : number of bits for back-reference lengths (must match the encoder).
/// - `I`   : streaming input buffer size in bytes (>= 1, tunable for RAM/throughput).
/// - `WIN` : output (window) buffer size in bytes; **must equal `1 << W`**.
///   (Redundant parameter required because Rust stable does not yet
///   support arithmetic on const generics in array sizes.)
///
/// Use [`DefaultDecoder`](super::DefaultDecoder) or the dispatch helpers in
/// `heatshrink-bin` rather than setting `WIN` manually.
///
/// # Panics
///
/// [`new()`](HeatshrinkDecoder::new) panics if: `W < 4`, `L < 3`, `L >= W`,
/// `W > 14`, `I < 1`, or `WIN != 1 << W`.
#[derive(Debug)]
pub struct HeatshrinkDecoder<const W: usize, const L: usize, const I: usize, const WIN: usize> {
    input_size: usize,
    input_index: usize,
    output_index: usize,
    head_index: usize,
    output_count: u16,
    current_byte: u8,
    bit_index: u8,
    state: HSDstate,
    input_buffer: [u8; I],
    output_buffer: [u8; WIN],
}

/// Decompress `src` into `dst` using the default parameters (W=8, L=4, I=32).
pub fn decode<'a>(src: &[u8], dst: &'a mut [u8]) -> Result<&'a [u8], CodecError> {
    let mut dec = super::DefaultDecoder::new();
    run_decode(&mut dec, src, dst)
}

/// Internal decode loop, generic over all decoder configurations.
pub(crate) fn run_decode<'a, const W: usize, const L: usize, const I: usize, const WIN: usize>(
    dec: &mut HeatshrinkDecoder<W, L, I, WIN>,
    src: &[u8],
    dst: &'a mut [u8],
) -> Result<&'a [u8], CodecError> {
    let mut total_input_size = 0;
    let mut total_output_size = 0;

    while total_input_size < src.len() {
        match dec.sink(&src[total_input_size..]) {
            Ok(n) => total_input_size += n,
            Err(SinkError::Full) => {}
            Err(SinkError::Misuse) => return Err(CodecError::Internal),
        }

        if total_output_size == dst.len() {
            return Err(CodecError::OutputFull);
        }

        match dec.poll(&mut dst[total_output_size..]) {
            Ok(Poll::More(_)) => return Err(CodecError::OutputFull),
            Ok(Poll::Empty(n)) => total_output_size += n,
            Err(_) => return Err(CodecError::Internal),
        }

        if total_input_size == src.len() {
            match dec.finish() {
                Finish::Done => {}
                Finish::More => return Err(CodecError::OutputFull),
            }
        }
    }

    Ok(&dst[..total_output_size])
}

impl<const W: usize, const L: usize, const I: usize, const WIN: usize> Default
    for HeatshrinkDecoder<W, L, I, WIN>
{
    fn default() -> Self {
        Self::new()
    }
}

impl<const W: usize, const L: usize, const I: usize, const WIN: usize>
    HeatshrinkDecoder<W, L, I, WIN>
{
    /// Create a new decoder instance.
    ///
    /// # Panics
    ///
    /// Panics if `W < 4`, `L < 3`, `L >= W`, `W > 15`, `I < 1`,
    /// or `WIN != 1 << W`.
    pub fn new() -> Self {
        assert!(W >= 4, "W must be >= 4");
        assert!(L >= 3, "L must be >= 3");
        assert!(L < W, "L must be < W");
        assert!(W <= 15, "W must be <= 15 (search_index uses Option<u16>)");
        assert!(I >= 1, "I must be >= 1");
        assert!(WIN == 1 << W, "WIN must equal 1 << W");

        HeatshrinkDecoder {
            input_size: 0,
            input_index: 0,
            output_count: 0,
            output_index: 0,
            head_index: 0,
            current_byte: 0,
            bit_index: 0,
            state: HSDstate::TagBit,
            input_buffer: [0; I],
            output_buffer: [0; WIN],
        }
    }

    /// Reset the decoder to its initial state so it can be reused.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Feed compressed data into the decoder.
    pub fn sink(&mut self, input_buffer: &[u8]) -> Result<usize, SinkError> {
        // Compact: slide unconsumed bytes to the front so the whole buffer
        // is available for new data.  input_index bytes at the start have
        // already been consumed by get_bits and can be overwritten.
        // We must preserve current_byte / bit_index which hold up to 8 bits
        // that were pre-loaded from the last consumed byte — those bits are
        // NOT in input_buffer[input_index..] and must not be disturbed.
        let unconsumed = self.input_size - self.input_index;
        if self.input_index > 0 && unconsumed > 0 {
            self.input_buffer
                .copy_within(self.input_index..self.input_size, 0);
        }
        self.input_size = unconsumed;
        self.input_index = 0;

        let remaining_size = self.input_buffer.len() - self.input_size;
        if remaining_size == 0 {
            return Err(SinkError::Full);
        }

        let copy_size = remaining_size.min(input_buffer.len());
        self.input_buffer[self.input_size..self.input_size + copy_size]
            .copy_from_slice(&input_buffer[..copy_size]);
        self.input_size += copy_size;

        if self.bit_index == 0 {
            self.current_byte = self.input_buffer[self.input_index];
            self.input_index += 1;
            self.bit_index = 8;
        }

        Ok(copy_size)
    }

    /// Pull decompressed output out of the decoder into `output_buffer`.
    pub fn poll(&mut self, output_buffer: &mut [u8]) -> Result<Poll, PollError> {
        if output_buffer.is_empty() {
            return Err(PollError::Misuse);
        }

        let mut out_pos: usize = 0;

        loop {
            let previous_state = self.state;

            match previous_state {
                HSDstate::TagBit => {
                    self.state = self.st_tag_bit();
                }
                HSDstate::YieldLiteral => {
                    self.state = self.st_yield_literal(output_buffer, &mut out_pos);
                }
                HSDstate::BackrefIndexMsb => {
                    self.state = self.st_backref_index_msb();
                }
                HSDstate::BackrefIndexLsb => {
                    self.state = self.st_backref_index_lsb();
                }
                HSDstate::BackrefCountLsb => {
                    self.state = self.st_backref_count_lsb();
                }
                HSDstate::YieldBackref => {
                    self.state = self.st_yield_backref(output_buffer, &mut out_pos);
                }
            }

            if self.state == previous_state {
                return if out_pos < output_buffer.len() {
                    Ok(Poll::Empty(out_pos))
                } else {
                    Ok(Poll::More(out_pos))
                };
            }
        }
    }

    /// Signal end of input.
    pub fn finish(&self) -> Finish {
        if self.input_size == 0 {
            Finish::Done
        } else {
            Finish::More
        }
    }

    // ---- State machine helpers ----

    #[inline]
    fn st_tag_bit(&mut self) -> HSDstate {
        match self.get_bits(1) {
            None => HSDstate::TagBit,
            Some(0) => {
                self.output_index = 0;
                if W > 8 {
                    HSDstate::BackrefIndexMsb
                } else {
                    HSDstate::BackrefIndexLsb
                }
            }
            Some(_) => HSDstate::YieldLiteral,
        }
    }

    #[inline]
    fn st_yield_literal(&mut self, out: &mut [u8], pos: &mut usize) -> HSDstate {
        if *pos < out.len() {
            match self.get_bits(8) {
                None => HSDstate::YieldLiteral,
                Some(c) => {
                    let c = c as u8;
                    self.output_buffer[self.head_index % WIN] = c;
                    self.head_index += 1;
                    out[*pos] = c;
                    *pos += 1;
                    HSDstate::TagBit
                }
            }
        } else {
            HSDstate::YieldLiteral
        }
    }

    /// Only reached when W > 8: reads the (W - 8) most-significant index bits.
    #[inline]
    fn st_backref_index_msb(&mut self) -> HSDstate {
        match self.get_bits((W - 8) as u8) {
            None => HSDstate::BackrefIndexMsb,
            Some(x) => {
                self.output_index = (x as usize) << 8;
                HSDstate::BackrefIndexLsb
            }
        }
    }

    #[inline]
    fn st_backref_index_lsb(&mut self) -> HSDstate {
        // When W <= 8 we arrive here directly (no MSB state) and the encoder
        // wrote exactly W bits for the index.  When W > 8 we arrive after
        // st_backref_index_msb which already consumed (W-8) bits, so we need
        // the remaining 8 bits.  In both cases the right count is min(W, 8).
        let lsb_bits = W.min(8) as u8;
        match self.get_bits(lsb_bits) {
            None => HSDstate::BackrefIndexLsb,
            Some(x) => {
                self.output_index |= x as usize;
                self.output_index += 1;
                self.output_count = 0;
                HSDstate::BackrefCountLsb
            }
        }
    }

    #[inline]
    fn st_backref_count_lsb(&mut self) -> HSDstate {
        match self.get_bits(L as u8) {
            None => HSDstate::BackrefCountLsb,
            Some(x) => {
                self.output_count = x + 1;
                HSDstate::YieldBackref
            }
        }
    }

    #[inline]
    fn st_yield_backref(&mut self, out: &mut [u8], pos: &mut usize) -> HSDstate {
        if *pos == out.len() {
            return HSDstate::YieldBackref;
        }

        let output_index = self.output_index;
        let count = (out.len() - *pos).min(self.output_count as usize);

        // Prologue: back-reference points before the start of the stream
        // (output_index > head_index).  Emit zeroes — rare, only at the very
        // beginning of decoding.
        if output_index > self.head_index {
            let zero_count = count.min(output_index - self.head_index);
            let limit = self.head_index + zero_count;
            while self.head_index < limit {
                out[*pos] = 0;
                *pos += 1;
                self.output_buffer[self.head_index & (WIN - 1)] = 0;
                self.head_index += 1;
            }
            self.output_count -= zero_count as u16;
            if self.output_count == 0 {
                return HSDstate::TagBit;
            }
            if *pos == out.len() {
                return HSDstate::YieldBackref;
            }
        }

        // How many bytes remain to emit in this call.
        let count = (out.len() - *pos).min(self.output_count as usize);

        if output_index >= count {
            // ── Fast path: no self-referential overlap ────────────────────────
            // The source window [src_start .. src_start+count] does not overlap
            // with the destination window [dst_start .. dst_start+count] in the
            // circular buffer (they are at least `count` bytes apart).
            // We copy in at most two contiguous chunks to handle wrap-around,
            // then bulk-copy the same data into the caller's output buffer.
            let src_start = (self.head_index - output_index) & (WIN - 1);
            let dst_start = self.head_index & (WIN - 1);

            // --- copy into the circular output_buffer (1 or 2 chunks) ---
            let src_end = src_start + count;
            let dst_end = dst_start + count;

            if src_end <= WIN && dst_end <= WIN {
                // Neither source nor destination wraps: single copy.
                self.output_buffer
                    .copy_within(src_start..src_start + count, dst_start);
            } else {
                // At least one side wraps: copy byte by byte into the ring but
                // still avoid the byte-by-byte loop below by handling
                // the ring copy first, then using a slice copy for the caller.
                let limit = self.head_index + count;
                let mut h = self.head_index;
                while h < limit {
                    let s = (h - output_index) & (WIN - 1);
                    let d = h & (WIN - 1);
                    self.output_buffer[d] = self.output_buffer[s];
                    h += 1;
                }
            }

            // --- bulk copy from the (now updated) ring to the caller's buffer --
            // dst_start is where we just wrote `count` bytes (possibly wrapping).
            // Re-read from the ring to the output slice in 1 or 2 chunks.
            if dst_end <= WIN {
                out[*pos..*pos + count]
                    .copy_from_slice(&self.output_buffer[dst_start..dst_start + count]);
            } else {
                let first = WIN - dst_start;
                let second = count - first;
                out[*pos..*pos + first].copy_from_slice(&self.output_buffer[dst_start..WIN]);
                out[*pos + first..*pos + count].copy_from_slice(&self.output_buffer[..second]);
            }
            *pos += count;
            self.head_index += count;
        } else {
            // ── Slow path: self-referential match (output_index < count) ──────
            // Each byte written may immediately become the source of the next
            // read (e.g. run-length: output_index=1, count=50 → "aaaa…").
            // Must stay byte-by-byte.
            let limit = self.head_index + count;
            while self.head_index < limit {
                let c = self.output_buffer[(self.head_index - output_index) & (WIN - 1)];
                out[*pos] = c;
                *pos += 1;
                self.output_buffer[self.head_index & (WIN - 1)] = c;
                self.head_index += 1;
            }
        }

        self.output_count -= count as u16;
        if self.output_count == 0 {
            HSDstate::TagBit
        } else {
            HSDstate::YieldBackref
        }
    }

    /// Read the next `count` bits from the input buffer.
    ///
    /// Supports up to 15 bits (the maximum needed is `L` for back-reference
    /// lengths, and `L < W <= 15`).  Returns `None` if not enough bits are
    /// available.
    fn get_bits(&mut self, count: u8) -> Option<u16> {
        debug_assert!(count > 0 && count <= 15);

        let available = (self.input_size - self.input_index) * 8 + self.bit_index as usize;
        if available < count as usize {
            return None;
        }

        // u32 accumulator covers the worst case: bit_index=1, count=15,
        // which may require loading 2 extra bytes.
        let mut acc = (self.current_byte as u32) & ((1 << self.bit_index) - 1);
        let mut bits = self.bit_index;

        while bits < count {
            self.current_byte = self.input_buffer[self.input_index];
            self.input_index += 1;
            acc = (acc << 8) | self.current_byte as u32;
            bits += 8;
        }

        let remaining = bits - count;
        let result = (acc >> remaining) & ((1u32 << count) - 1);

        // Maintain the invariant: bit_index == 0 only when the buffer is fully
        // consumed.  If remaining == 0 and bytes remain, pre-load the next one
        // (same as sink() does on first fill), so sink() never wrongly
        // overwrites an unconsumed byte.
        if remaining == 0 {
            if self.input_index < self.input_size {
                self.current_byte = self.input_buffer[self.input_index];
                self.input_index += 1;
                self.bit_index = 8;
                // If we just consumed the last byte, reset so sink() can refill.
                if self.input_index == self.input_size {
                    self.input_index = 0;
                    self.input_size = 0;
                }
            } else {
                self.input_index = 0;
                self.input_size = 0;
                self.bit_index = 0;
                self.current_byte = 0;
            }
        } else {
            self.bit_index = remaining;
            self.current_byte = (acc & ((1 << remaining) - 1)) as u8;
            if self.input_index == self.input_size {
                self.input_index = 0;
                self.input_size = 0;
            }
        }

        Some(result as u16)
    }
}
