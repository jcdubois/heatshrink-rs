use super::CodecError;
use super::Finish;
use super::Poll;
use super::PollError;
use super::SinkError;

#[derive(Debug, Copy, Clone, PartialEq)]
enum HSEstate {
    NotFull,
    Filled,
    Search,
    YieldTagBit,
    YieldLiteral,
    YieldBrIndex,
    YieldBrLength,
    SaveBacklog,
    FlushBits,
    Done,
}

/// Heatshrink encoder.
///
/// # Type parameters
///
/// - `W`   : base-2 log of the LZSS sliding window size.
/// - `L`   : number of bits used to encode back-reference lengths (must be < W).
/// - `BUF` : total input buffer size in bytes; **must equal `2 << W`**.
///   (This redundant parameter is required because Rust stable does not
///   yet support arithmetic on const generics in array sizes.)
///
/// Use the [`DefaultEncoder`](super::DefaultEncoder) type alias or construct
/// via [`HeatshrinkEncoder::new`] when using the convenience dispatch in
/// `heatshrink-bin`; do not set `BUF` manually — always use `2 << W`.
///
/// # Panics
///
/// [`new()`](HeatshrinkEncoder::new) panics if: `W < 4`, `L < 3`, `L >= W`,
/// `W > 14`, or `BUF != 2 << W`.
#[derive(Debug)]
pub struct HeatshrinkEncoder<const W: usize, const L: usize, const BUF: usize> {
    input_size: usize,
    match_scan_index: usize,
    match_length: usize,
    match_position: usize,
    outgoing_bits: u16,
    outgoing_bits_count: u8,
    is_finishing: bool,
    current_byte: u8,
    bit_index: u8,
    state: HSEstate,
    #[cfg(feature = "heatshrink-use-index")]
    search_index: [u16; BUF],
    input_buffer: [u8; BUF],
}

/// Compress `src` into `dst` using the default parameters (W=8, L=4).
pub fn encode<'a>(src: &[u8], dst: &'a mut [u8]) -> Result<&'a [u8], CodecError> {
    let mut enc = super::DefaultEncoder::new();
    run_encode(&mut enc, src, dst)
}

/// Internal encode loop, generic over all encoder configurations.
pub(crate) fn run_encode<'a, const W: usize, const L: usize, const BUF: usize>(
    enc: &mut HeatshrinkEncoder<W, L, BUF>,
    src: &[u8],
    dst: &'a mut [u8],
) -> Result<&'a [u8], CodecError> {
    let mut total_input_size = 0;
    let mut total_output_size = 0;

    loop {
        if total_input_size < src.len() {
            match enc.sink(&src[total_input_size..]) {
                Ok(n) => total_input_size += n,
                Err(SinkError::Full) => {}
                Err(SinkError::Misuse) => return Err(CodecError::Internal),
            }
        }

        if total_input_size == src.len() {
            enc.finish();
        }

        if total_output_size == dst.len() {
            return Err(CodecError::OutputFull);
        }

        match enc.poll(&mut dst[total_output_size..]) {
            Ok(Poll::More(n)) => {
                total_output_size += n;
                if total_output_size == dst.len() {
                    return Err(CodecError::OutputFull);
                }
            }
            Ok(Poll::Empty(n)) => {
                total_output_size += n;
                if total_input_size == src.len() {
                    break;
                }
            }
            Err(_) => return Err(CodecError::Internal),
        }
    }

    Ok(&dst[..total_output_size])
}

impl<const W: usize, const L: usize, const BUF: usize> Default for HeatshrinkEncoder<W, L, BUF> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const W: usize, const L: usize, const BUF: usize> HeatshrinkEncoder<W, L, BUF> {
    /// Create a new encoder instance.
    ///
    /// # Panics
    ///
    /// Panics if `W < 4`, `L < 3`, `L >= W`, `W > 15`, or `BUF != 2 << W`.
    pub fn new() -> Self {
        assert!(W >= 4, "W must be >= 4");
        assert!(L >= 3, "L must be >= 3");
        assert!(L < W, "L must be < W");
        assert!(
            W <= 15,
            "W must be <= 15 (BUF = 2<<W, max index u16::MAX-1 = 65534 >= 2<<15)"
        );
        assert!(BUF == 2 << W, "BUF must equal 2 << W");

        HeatshrinkEncoder {
            input_size: 0,
            match_scan_index: 0,
            match_length: 0,
            match_position: 0,
            outgoing_bits: 0,
            outgoing_bits_count: 0,
            is_finishing: false,
            current_byte: 0,
            bit_index: 8,
            state: HSEstate::NotFull,
            #[cfg(feature = "heatshrink-use-index")]
            search_index: [u16::MAX; BUF],
            input_buffer: [0; BUF],
        }
    }

    /// Reset the encoder to its initial state so it can be reused.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Feed input data into the encoder.
    pub fn sink(&mut self, input_buffer: &[u8]) -> Result<usize, SinkError> {
        if self.is_finishing {
            return Err(SinkError::Misuse);
        }
        if self.state != HSEstate::NotFull {
            return Err(SinkError::Full);
        }

        let remaining_size = self.get_input_buffer_size() - self.input_size;
        if remaining_size == 0 {
            return Err(SinkError::Full);
        }

        let copy_size = remaining_size.min(input_buffer.len());
        let write_offset = self.get_input_offset() + self.input_size;

        self.input_buffer[write_offset..write_offset + copy_size]
            .copy_from_slice(&input_buffer[..copy_size]);
        self.input_size += copy_size;

        if self.input_size == self.get_input_buffer_size() {
            self.state = HSEstate::Filled;
        }

        Ok(copy_size)
    }

    /// Pull compressed output out of the encoder into `output_buffer`.
    pub fn poll(&mut self, output_buffer: &mut [u8]) -> Result<Poll, PollError> {
        if output_buffer.is_empty() {
            return Err(PollError::Misuse);
        }

        let mut out_pos: usize = 0;

        loop {
            let previous_state = self.state;

            match previous_state {
                HSEstate::NotFull => return Ok(Poll::Empty(out_pos)),
                HSEstate::Filled => {
                    self.do_indexing();
                    self.state = HSEstate::Search;
                }
                HSEstate::Search => {
                    self.state = self.st_step_search();
                }
                HSEstate::YieldTagBit => {
                    self.state = self.st_yield_tag_bit(output_buffer, &mut out_pos);
                }
                HSEstate::YieldLiteral => {
                    self.state = self.st_yield_literal(output_buffer, &mut out_pos);
                }
                HSEstate::YieldBrIndex => {
                    self.state = self.st_yield_br_index(output_buffer, &mut out_pos);
                }
                HSEstate::YieldBrLength => {
                    self.state = self.st_yield_br_length(output_buffer, &mut out_pos);
                }
                HSEstate::SaveBacklog => {
                    self.state = self.st_save_backlog();
                }
                HSEstate::FlushBits => {
                    self.state = self.st_flush_bit_buffer(output_buffer, &mut out_pos);
                    return Ok(Poll::Empty(out_pos));
                }
                HSEstate::Done => return Ok(Poll::Empty(out_pos)),
            }

            if self.state == previous_state && out_pos == output_buffer.len() {
                return Ok(Poll::More(out_pos));
            }
        }
    }

    /// Signal that all input has been provided.
    pub fn finish(&mut self) -> Finish {
        self.is_finishing = true;
        if self.state == HSEstate::NotFull {
            self.state = HSEstate::Filled;
        }
        if self.state == HSEstate::Done {
            Finish::Done
        } else {
            Finish::More
        }
    }

    // ---- State machine helpers ----

    #[inline]
    fn st_step_search(&mut self) -> HSEstate {
        let lookahead = if self.is_finishing {
            1
        } else {
            self.get_lookahead_size()
        };
        if self.match_scan_index + lookahead > self.input_size {
            return if self.is_finishing {
                HSEstate::FlushBits
            } else {
                HSEstate::SaveBacklog
            };
        }

        let end = self.get_input_offset() + self.match_scan_index;
        let start = end - self.get_input_buffer_size();
        let max_possible = if self.input_size < self.get_lookahead_size() + self.match_scan_index {
            self.input_size - self.match_scan_index
        } else {
            self.get_lookahead_size()
        };

        match self.find_longest_match(start, end, max_possible) {
            None => {
                self.match_scan_index += 1;
                self.match_length = 0;
            }
            Some((position, length)) => {
                self.match_position = position;
                self.match_length = length;
                assert!(self.match_position <= 1 << W);
            }
        }
        HSEstate::YieldTagBit
    }

    #[inline]
    fn st_yield_tag_bit(&mut self, out: &mut [u8], pos: &mut usize) -> HSEstate {
        if *pos < out.len() {
            if self.match_length == 0 {
                self.add_tag_bit(out, pos, 0x1);
                HSEstate::YieldLiteral
            } else {
                self.add_tag_bit(out, pos, 0);
                self.outgoing_bits = self.match_position as u16 - 1;
                self.outgoing_bits_count = W as u8;
                HSEstate::YieldBrIndex
            }
        } else {
            HSEstate::YieldTagBit
        }
    }

    #[inline]
    fn st_yield_literal(&mut self, out: &mut [u8], pos: &mut usize) -> HSEstate {
        if *pos < out.len() {
            self.push_literal_byte(out, pos);
            HSEstate::Search
        } else {
            HSEstate::YieldLiteral
        }
    }

    #[inline]
    fn st_yield_br_index(&mut self, out: &mut [u8], pos: &mut usize) -> HSEstate {
        if *pos < out.len() {
            if self.push_outgoing_bits(out, pos) > 0 {
                HSEstate::YieldBrIndex
            } else {
                self.outgoing_bits = self.match_length as u16 - 1;
                self.outgoing_bits_count = L as u8;
                HSEstate::YieldBrLength
            }
        } else {
            HSEstate::YieldBrIndex
        }
    }

    #[inline]
    fn st_yield_br_length(&mut self, out: &mut [u8], pos: &mut usize) -> HSEstate {
        if *pos < out.len() {
            if self.push_outgoing_bits(out, pos) > 0 {
                HSEstate::YieldBrLength
            } else {
                self.match_scan_index += self.match_length;
                self.match_length = 0;
                HSEstate::Search
            }
        } else {
            HSEstate::YieldBrLength
        }
    }

    #[inline]
    fn st_save_backlog(&mut self) -> HSEstate {
        self.save_backlog();
        HSEstate::NotFull
    }

    #[inline]
    fn st_flush_bit_buffer(&self, out: &mut [u8], pos: &mut usize) -> HSEstate {
        if self.bit_index == 8 {
            HSEstate::Done
        } else if *pos < out.len() {
            out[*pos] = self.current_byte;
            *pos += 1;
            HSEstate::Done
        } else {
            HSEstate::FlushBits
        }
    }

    #[inline]
    fn add_tag_bit(&mut self, out: &mut [u8], pos: &mut usize, tag: u8) {
        self.push_bits(1, tag, out, pos)
    }

    #[inline]
    fn get_input_offset(&self) -> usize {
        self.get_input_buffer_size()
    }

    #[inline]
    fn get_input_buffer_size(&self) -> usize {
        BUF / 2
    }

    #[inline]
    fn get_lookahead_size(&self) -> usize {
        1 << L
    }

    #[inline]
    fn do_indexing(&mut self) {
        #[cfg(feature = "heatshrink-use-index")]
        {
            // last[v] = most recent position where byte v was seen, u16::MAX = none.
            // enumerate() lets the compiler elide bounds-checks on both slices.
            let mut last: [u16; 256] = [u16::MAX; 256];
            let end = self.get_input_offset() + self.input_size - 1;
            self.input_buffer[..end]
                .iter()
                .zip(self.search_index[..end].iter_mut())
                .enumerate()
                .for_each(|(i, (&v, slot))| {
                    let v = v as usize;
                    *slot = last[v];
                    last[v] = i as u16;
                });
        }
    }

    #[inline]
    fn find_longest_match(
        &self,
        start: usize,
        end: usize,
        maxlen: usize,
    ) -> Option<(usize, usize)> {
        let mut match_maxlen: usize = 0;
        let mut match_index: usize = 0;

        // Pre-slice the buffer once so every inner access is relative to a
        // slice whose length the compiler knows.  This lets LLVM prove that
        // needle[len] and candidate[len] are always in-bounds (len < maxlen
        // is the loop invariant) and eliminate the bounds-checks entirely —
        // without any `unsafe`.
        //
        // window covers [start .. end+maxlen]: the window bytes that can be
        // referenced as back-reference sources, plus the lookahead we compare
        // against.  All candidate positions satisfy start <= pos < end, so
        // pos - start + maxlen <= end - start + maxlen = window.len().
        let window = &self.input_buffer[start..end + maxlen];
        // needle is the lookahead we are trying to match, relative to window.
        let needle_off = end - start; // offset of `end` inside `window`
        let needle = &window[needle_off..needle_off + maxlen];

        #[cfg(not(feature = "heatshrink-use-index"))]
        {
            let mut position = end - 1;
            loop {
                let cand_off = position - start;
                let candidate = &window[cand_off..cand_off + maxlen];
                if candidate[0] == needle[0] && candidate[match_maxlen] == needle[match_maxlen] {
                    let mut len = 1;
                    while len < maxlen {
                        if candidate[len] != needle[len] {
                            break;
                        }
                        len += 1;
                    }
                    if len > match_maxlen {
                        match_maxlen = len;
                        match_index = position;
                        if len == maxlen {
                            break;
                        }
                    }
                }
                if position == start {
                    break;
                }
                position -= 1;
            }
        }

        #[cfg(feature = "heatshrink-use-index")]
        {
            let mut position = self.search_index[end];
            while position != u16::MAX {
                let pos = position as usize;
                if pos < start {
                    break;
                }
                let cand_off = pos - start;
                let candidate = &window[cand_off..cand_off + maxlen];
                // Quick-reject on the current best length before the inner loop.
                if candidate[match_maxlen] != needle[match_maxlen] {
                    position = self.search_index[pos];
                    continue;
                }
                let mut len = 1;
                while len < maxlen {
                    if candidate[len] != needle[len] {
                        break;
                    }
                    len += 1;
                }
                if len > match_maxlen {
                    match_maxlen = len;
                    match_index = pos;
                    if len == maxlen {
                        break;
                    }
                }
                position = self.search_index[pos];
            }
        }

        let break_even_point: usize = (1 + W + L) / 8;
        if match_maxlen > break_even_point {
            Some((end - match_index, match_maxlen))
        } else {
            None
        }
    }

    #[inline]
    fn push_outgoing_bits(&mut self, out: &mut [u8], pos: &mut usize) -> u8 {
        let (count, bits) = if self.outgoing_bits_count > 8 {
            (
                8u8,
                (self.outgoing_bits >> (self.outgoing_bits_count - 8)) as u8,
            )
        } else {
            (self.outgoing_bits_count, self.outgoing_bits as u8)
        };
        if count > 0 {
            self.push_bits(count, bits, out, pos);
            self.outgoing_bits_count -= count;
        }
        count
    }

    #[inline]
    fn push_bits(&mut self, count: u8, bits: u8, out: &mut [u8], pos: &mut usize) {
        debug_assert!(count > 0 && count <= 8);
        // Fast path: pushing a full aligned byte (the common case for literals).
        if count == 8 && self.bit_index == 8 {
            out[*pos] = bits;
            *pos += 1;
            return;
        }
        if count >= self.bit_index {
            let shift = count - self.bit_index;
            let tmp_byte = self.current_byte | (bits >> shift);
            out[*pos] = tmp_byte;
            *pos += 1;
            self.bit_index = 8 - shift;
            self.current_byte = if shift == 0 {
                0
            } else {
                bits << self.bit_index
            };
        } else {
            self.bit_index -= count;
            self.current_byte |= bits << self.bit_index;
        }
    }

    #[inline]
    fn push_literal_byte(&mut self, out: &mut [u8], pos: &mut usize) {
        let byte = self.input_buffer[self.get_input_offset() + self.match_scan_index - 1];
        self.push_bits(8, byte, out, pos);
    }

    #[inline]
    fn save_backlog(&mut self) {
        self.input_buffer.copy_within(self.match_scan_index.., 0);
        self.input_size -= self.match_scan_index;
        self.match_scan_index = 0;
    }
}
