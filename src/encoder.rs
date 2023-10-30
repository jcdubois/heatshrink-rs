use super::HSError;
use super::HSfinishRes;
use super::HSpollRes;
use super::HSsinkRes;
use super::OutputInfo;
use super::HEATSHRINK_LOOKAHEAD_BITS;
use super::HEATSHRINK_WINDOWS_BITS;

#[derive(Debug, Copy, Clone, PartialEq)]
enum HSEstate {
    NotFull,       /* input buffer not full enough */
    Filled,        /* buffer is full */
    Search,        /* searching for patterns */
    YieldTagBit,   /* yield tag bit */
    YieldLiteral,  /* emit literal byte */
    YieldBrIndex,  /* yielding backref index */
    YieldBrLength, /* yielding backref length */
    SaveBacklog,   /* copying buffer to backlog */
    FlushBits,     /* flush bit buffer */
    Done,          /* done */
}

#[cfg(not(feature = "heatshrink-use-index"))]
/// The encoder instance
#[derive(Debug)]
pub struct HeatshrinkEncoder {
    input_size: usize,
    match_scan_index: usize,
    match_length: usize,
    match_pos: usize,
    outgoing_bits: u16,
    outgoing_bits_count: u8,
    flags: u8,
    current_byte: u8,
    bit_index: u8,
    state: HSEstate,
    input_buffer: [u8; 2 << HEATSHRINK_WINDOWS_BITS],
}

#[cfg(feature = "heatshrink-use-index")]
/// The encoder instance
#[derive(Debug)]
pub struct HeatshrinkEncoder {
    input_size: usize,
    match_scan_index: usize,
    match_length: usize,
    match_pos: usize,
    outgoing_bits: u16,
    outgoing_bits_count: u8,
    flags: u8,
    current_byte: u8,
    bit_index: u8,
    state: HSEstate,
    search_index: [Option<usize>; 2 << HEATSHRINK_WINDOWS_BITS],
    input_buffer: [u8; 2 << HEATSHRINK_WINDOWS_BITS],
}

/// A constant flag to set an encoder as finishing
const FLAG_IS_FINISHING: u8 = 1;

/// compress the src buffer to the destination buffer
pub fn encode<'a>(src: &[u8], dst: &'a mut [u8]) -> Result<&'a [u8], HSError> {
    let mut total_input_size = 0;
    let mut total_output_size = 0;

    let mut enc: HeatshrinkEncoder = Default::default();

    while total_input_size < src.len() {
        // Fill the input buffer from the src buffer
        match enc.sink(&src[total_input_size..]) {
            (HSsinkRes::SinkOK, segment_input_size) => {
                total_input_size += segment_input_size;
            }
            (HSsinkRes::SinkFull, _) => {
                return Err(HSError::Internal);
            }
            (HSsinkRes::SinkErrorMisuse, _) => {
                return Err(HSError::Internal);
            }
        }

        // if all the src buffer is processed, finish the compress stream
        if total_input_size == src.len() {
            match enc.finish() {
                HSfinishRes::FinishDone => {}
                HSfinishRes::FinishMore => {}
            }
        }

        if total_output_size == dst.len() {
            return Err(HSError::OutputFull);
        } else {
            // process the current input buffer
            match enc.poll(&mut dst[total_output_size..]) {
                (HSpollRes::PollMore, _) => {
                    return Err(HSError::OutputFull);
                }
                (HSpollRes::PollEmpty, segment_output_size) => {
                    total_output_size += segment_output_size;
                }
                (HSpollRes::PollErrorMisuse, _) => {
                    return Err(HSError::Internal);
                }
            }
        }
    }

    Ok(&dst[..total_output_size])
}

impl Default for HeatshrinkEncoder {
    fn default() -> Self {
        HeatshrinkEncoder::new()
    }
}

impl HeatshrinkEncoder {
    /// Create a new encoder instance
    pub fn new() -> Self {
        #[cfg(feature = "heatshrink-use-index")]
        {
            HeatshrinkEncoder {
                input_size: 0,
                match_scan_index: 0,
                match_length: 0,
                match_pos: 0,
                outgoing_bits: 0,
                outgoing_bits_count: 0,
                flags: 0,
                current_byte: 0,
                bit_index: 8,
                state: HSEstate::NotFull,
                search_index: [None; 2 << HEATSHRINK_WINDOWS_BITS],
                input_buffer: [0; 2 << HEATSHRINK_WINDOWS_BITS],
            }
        }

        #[cfg(not(feature = "heatshrink-use-index"))]
        {
            HeatshrinkEncoder {
                input_size: 0,
                match_scan_index: 0,
                match_length: 0,
                match_pos: 0,
                outgoing_bits: 0,
                outgoing_bits_count: 0,
                flags: 0,
                current_byte: 0,
                bit_index: 8,
                state: HSEstate::NotFull,
                input_buffer: [0; 2 << HEATSHRINK_WINDOWS_BITS],
            }
        }
    }

    /// Reset the current encoder instance
    pub fn reset(&mut self) {
        self.input_size = 0;
        self.match_scan_index = 0;
        self.match_length = 0;
        self.match_pos = 0;
        self.outgoing_bits = 0;
        self.outgoing_bits_count = 0;
        self.flags = 0;
        self.current_byte = 0;
        self.bit_index = 8;
        self.state = HSEstate::NotFull;
        // memset self.buffer to 0
        self.input_buffer.iter_mut().for_each(|m| *m = 0);
        #[cfg(feature = "heatshrink-use-index")]
        {
            // memset self.search_index to 0
            self.search_index.iter_mut().for_each(|m| *m = None);
        }
    }

    /// Add an input buffer to be processed/compressed
    pub fn sink(&mut self, input_buffer: &[u8]) -> (HSsinkRes, usize) {
        /* Sinking more content after saying the content is done, tsk tsk */
        if self.is_finishing() {
            return (HSsinkRes::SinkErrorMisuse, 0);
        }

        /* Sinking more content before processing is done */
        if self.state != HSEstate::NotFull {
            return (HSsinkRes::SinkErrorMisuse, 0);
        }

        let remaining_size = self.get_input_buffer_size() - self.input_size;

        if remaining_size == 0 {
            return (HSsinkRes::SinkFull, 0);
        }

        let copy_size = if remaining_size < input_buffer.len() {
            remaining_size
        } else {
            input_buffer.len()
        };

        let write_offset = self.get_input_offset() + self.input_size;

        // memcpy content of input_buffer into self.input_buffer
        self.input_buffer[write_offset..write_offset + copy_size]
            .copy_from_slice(&input_buffer[0..copy_size]);
        self.input_size += copy_size;

        if self.input_size == self.get_input_buffer_size() {
            self.state = HSEstate::Filled;
        }

        (HSsinkRes::SinkOK, copy_size)
    }

    /// function to process the input/internal buffer and put the compressed
    /// stream in the provided buffer.
    pub fn poll(&mut self, output_buffer: &mut [u8]) -> (HSpollRes, usize) {
        if output_buffer.is_empty() {
            (HSpollRes::PollMore, 0)
        } else {
            let mut output_size: usize = 0;
            let mut output_info = OutputInfo::new(output_buffer, &mut output_size);

            loop {
                let in_state = self.state;

                match in_state {
                    HSEstate::NotFull => {
                        return (HSpollRes::PollEmpty, output_size);
                    }
                    HSEstate::Filled => {
                        self.do_indexing();
                        self.state = HSEstate::Search;
                    }
                    HSEstate::Search => {
                        self.state = self.st_step_search();
                    }
                    HSEstate::YieldTagBit => {
                        self.state = self.st_yield_tag_bit(&mut output_info);
                    }
                    HSEstate::YieldLiteral => {
                        self.state = self.st_yield_literal(&mut output_info);
                    }
                    HSEstate::YieldBrIndex => {
                        self.state = self.st_yield_br_index(&mut output_info);
                    }
                    HSEstate::YieldBrLength => {
                        self.state = self.st_yield_br_length(&mut output_info);
                    }
                    HSEstate::SaveBacklog => {
                        self.state = self.st_save_backlog();
                    }
                    HSEstate::FlushBits => {
                        self.state = self.st_flush_bit_buffer(&mut output_info);
                        return (HSpollRes::PollEmpty, output_size);
                    }
                    HSEstate::Done => {
                        return (HSpollRes::PollEmpty, output_size);
                    }
                }

                // If the current state cannot advance, check if output
                // buffer is exhausted.
                if self.state == in_state && !output_info.can_take_byte() {
                    return (HSpollRes::PollMore, output_size);
                }
            }
        }
    }

    /// Finish the compression stream.
    pub fn finish(&mut self) -> HSfinishRes {
        self.flags |= FLAG_IS_FINISHING;

        if self.state == HSEstate::NotFull {
            self.state = HSEstate::Filled;
        }

        if self.state == HSEstate::Done {
            HSfinishRes::FinishDone
        } else {
            HSfinishRes::FinishMore
        }
    }

    fn st_step_search(&mut self) -> HSEstate {
        if self.match_scan_index
            > self.input_size
                - (if self.is_finishing() {
                    1
                } else {
                    self.get_lookahead_size()
                })
        {
            if self.is_finishing() {
                HSEstate::FlushBits
            } else {
                HSEstate::SaveBacklog
            }
        } else {
            let end = self.get_input_offset() + self.match_scan_index;
            let start = end - self.get_input_buffer_size();
            let mut max_possible = self.get_lookahead_size();
            if (self.input_size - self.match_scan_index) < max_possible {
                max_possible = self.input_size - self.match_scan_index;
            }
            match self.find_longest_match(start, end, max_possible) {
                None => {
                    self.match_scan_index += 1;
                    self.match_length = 0;
                }
                Some(match_pos) => {
                    self.match_pos = match_pos.0;
                    self.match_length = match_pos.1;
                    assert!(self.match_pos <= 1 << HEATSHRINK_WINDOWS_BITS);
                }
            }
            HSEstate::YieldTagBit
        }
    }

    fn st_yield_tag_bit(&mut self, output_info: &mut OutputInfo) -> HSEstate {
        if output_info.can_take_byte() {
            if self.match_length == 0 {
                self.add_tag_bit(output_info, 0x1);
                HSEstate::YieldLiteral
            } else {
                self.add_tag_bit(output_info, 0);
                self.outgoing_bits = self.match_pos as u16 - 1;
                self.outgoing_bits_count = 8;
                HSEstate::YieldBrIndex
            }
        } else {
            HSEstate::YieldTagBit
        }
    }

    fn st_yield_literal(&mut self, output_info: &mut OutputInfo) -> HSEstate {
        if output_info.can_take_byte() {
            self.push_literal_byte(output_info);
            HSEstate::Search
        } else {
            HSEstate::YieldLiteral
        }
    }

    fn st_yield_br_index(&mut self, output_info: &mut OutputInfo) -> HSEstate {
        if output_info.can_take_byte() {
            if self.push_outgoing_bits(output_info) > 0 {
                HSEstate::YieldBrIndex
            } else {
                self.outgoing_bits = self.match_length as u16 - 1;
                self.outgoing_bits_count = 4;
                HSEstate::YieldBrLength
            }
        } else {
            HSEstate::YieldBrIndex
        }
    }

    fn st_yield_br_length(&mut self, output_info: &mut OutputInfo) -> HSEstate {
        if output_info.can_take_byte() {
            if self.push_outgoing_bits(output_info) > 0 {
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

    fn st_save_backlog(&mut self) -> HSEstate {
        self.save_backlog();
        HSEstate::NotFull
    }

    fn st_flush_bit_buffer(&self, output_info: &mut OutputInfo) -> HSEstate {
        if self.bit_index == 8 {
            HSEstate::Done
        } else if output_info.can_take_byte() {
            output_info.push_byte(self.current_byte);
            HSEstate::Done
        } else {
            HSEstate::FlushBits
        }
    }

    fn add_tag_bit(&mut self, output_info: &mut OutputInfo, tag: u8) {
        self.push_bits(1, tag, output_info)
    }

    fn get_input_offset(&self) -> usize {
        self.get_input_buffer_size()
    }

    fn get_input_buffer_size(&self) -> usize {
        self.input_buffer.len() / 2
    }

    fn get_lookahead_size(&self) -> usize {
        1 << HEATSHRINK_LOOKAHEAD_BITS
    }

    fn is_finishing(&self) -> bool {
        (self.flags & FLAG_IS_FINISHING) == FLAG_IS_FINISHING
    }

    fn do_indexing(&mut self) {
        #[cfg(feature = "heatshrink-use-index")]
        {
            /* Build an index array I that contains flattened linked lists
             * for the previous instances of every byte in the buffer.
             *
             * For example, if buf[200] == 'x', then index[200] will either
             * be an offset i such that buf[i] == 'x', or a negative offset
             * to indicate end-of-list. This significantly speeds up matching,
             * while only using sizeof(Option<u16>)*sizeof(buffer) bytes of
             * RAM.
             *
             * Future optimization options:
             * -  The last lookahead_sz bytes of the index will not be
             *    usable, so temporary data could be stored there to
             *    dynamically improve the index.
             * */
            let mut last: [Option<usize>; 256] = [None; 256];
            let end = self.get_input_offset() + self.input_size - 1;

            for i in 0..end {
                let v: usize = self.input_buffer[i].into();
                self.search_index[i] = last[v];
                last[v] = Some(i);
            }
        }
    }

    /// Return the longest match for the bytes at buf[end:end+maxlen] between
    /// buf[start] and buf[end-1]. If no match is found, return -1.
    fn find_longest_match(
        &self,
        start: usize,
        end: usize,
        maxlen: usize,
    ) -> Option<(usize, usize)> {
        let mut match_maxlen: usize = 0;
        let mut match_index: usize = 0;

        #[cfg(not(feature = "heatshrink-use-index"))]
        {
            let mut pos = end - 1;

            while pos >= start {
                if (self.input_buffer[pos] == self.input_buffer[end])
                    && (self.input_buffer[pos + match_maxlen]
                        == self.input_buffer[end + match_maxlen])
                {
                    let mut len: usize = 1;
                    while len < maxlen {
                        if self.input_buffer[pos + len] != self.input_buffer[end + len] {
                            break;
                        }
                        len += 1;
                    }
                    if len > match_maxlen {
                        match_maxlen = len;
                        match_index = pos;
                        if len == maxlen {
                            // don't keep searching
                            break;
                        }
                    }
                }

                if pos == 0 {
                    break;
                } else {
                    pos -= 1;
                }
            }
        }

        #[cfg(feature = "heatshrink-use-index")]
        {
            let mut pos = end;

            loop {
                match self.search_index[pos] {
                    None => {
                        break;
                    }
                    Some(x) => {
                        pos = x;

                        if pos < start {
                            break;
                        }

                        let mut len: usize = 1;

                        if self.input_buffer[pos + match_maxlen]
                            != self.input_buffer[end + match_maxlen]
                        {
                            continue;
                        }

                        while len < maxlen {
                            if self.input_buffer[pos + len] != self.input_buffer[end + len] {
                                break;
                            }
                            len += 1;
                        }

                        if len > match_maxlen {
                            match_maxlen = len;
                            match_index = pos;
                            if len == maxlen {
                                // don't keep searching
                                break;
                            }
                        }
                    }
                }
            }
        }

        let break_even_point: usize =
            (1 + HEATSHRINK_WINDOWS_BITS + HEATSHRINK_LOOKAHEAD_BITS).into();

        // Instead of comparing break_even_point against 8*match_maxlen,
        // compare match_maxlen against break_even_point/8 to avoid
        // overflow. Since MIN_WINDOW_BITS and MIN_LOOKAHEAD_BITS are 4 and
        // 3, respectively, break_even_point/8 will always be at least 1.
        if match_maxlen > (break_even_point / 8) {
            Some((end - match_index, match_maxlen))
        } else {
            None
        }
    }

    fn push_outgoing_bits(&mut self, output_info: &mut OutputInfo) -> u8 {
        let count: u8;
        let bits: u8;

        if self.outgoing_bits_count > 8 {
            count = 8;
            bits = (self.outgoing_bits >> (self.outgoing_bits_count - 8)) as u8;
        } else {
            count = self.outgoing_bits_count;
            bits = self.outgoing_bits as u8;
        }

        if count > 0 {
            self.push_bits(count, bits, output_info);
            self.outgoing_bits_count -= count;
        }

        count
    }

    /// Push COUNT (max 8) bits to the output buffer, which has room.
    /// Bytes are set from the lowest bits, up.
    fn push_bits(&mut self, count: u8, bits: u8, output_info: &mut OutputInfo) {
        assert!(count > 0 && count <= 8);

        if count >= self.bit_index {
            let mut shift: u8 = count - self.bit_index;
            let tmp_byte: u8 = self.current_byte | bits >> shift;
            output_info.push_byte(tmp_byte);
            if shift == 0 {
                self.bit_index = 8;
                self.current_byte = 0;
            } else {
                shift = 8 - shift;
                self.bit_index = shift;
                self.current_byte = bits << shift;
            }
        } else {
            self.bit_index -= count;
            self.current_byte |= bits << self.bit_index;
        }
    }

    fn push_literal_byte(&mut self, output_info: &mut OutputInfo) {
        let input_offset = self.match_scan_index - 1;
        let c = self.input_buffer[self.get_input_offset() + input_offset];
        self.push_bits(8, c, output_info);
    }

    fn save_backlog(&mut self) {
        // Copy processed data to beginning of buffer, so it can be used for
        // future matches. Don't bother checking whether the input is less
        // than the maximum size, because if it isn't, we're done anyway.
        let remaining_size = self.get_input_buffer_size() - self.match_scan_index; // unprocessed bytes
        let shift_size = self.get_input_buffer_size() + remaining_size;
        self.input_buffer
            .copy_within(self.match_scan_index..self.match_scan_index + shift_size, 0);
        self.match_scan_index = 0;
        self.input_size -= self.get_input_buffer_size() - remaining_size;
    }
}
