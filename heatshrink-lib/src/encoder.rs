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

/// The encoder instance
#[derive(Debug)]
pub struct HeatshrinkEncoder {
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
    search_index: [Option<u16>; 2 << HEATSHRINK_WINDOWS_BITS],
    input_buffer: [u8; 2 << HEATSHRINK_WINDOWS_BITS],
}

/// compress the src buffer to the destination buffer
pub fn encode<'a>(src: &[u8], dst: &'a mut [u8]) -> Result<&'a [u8], HSError> {
    let mut total_input_size = 0;
    let mut total_output_size = 0;

    let mut enc: HeatshrinkEncoder = Default::default();

    loop {
        // Feed in the encoder while data last
        if total_input_size < src.len() {
            match enc.sink(&src[total_input_size..]) {
                HSsinkRes::SinkOK(segment_input_size) => {
                    total_input_size += segment_input_size;
                }
                HSsinkRes::SinkFull => {}
                HSsinkRes::SinkErrorMisuse => {
                    return Err(HSError::Internal);
                }
            }
        }

        // Notify the end of stream when no more data to process
        if total_input_size == src.len() {
            enc.finish();
        }

        // Check that there is some available space on output
        if total_output_size == dst.len() {
            return Err(HSError::OutputFull);
        }

        match enc.poll(&mut dst[total_output_size..]) {
            HSpollRes::PollMore(segment_output_size) => {
                // There is more data to process but we are missing space on the output
                total_output_size += segment_output_size;
                if total_output_size == dst.len() {
                    return Err(HSError::OutputFull);
                }
            }
            HSpollRes::PollEmpty(segment_output_size) => {
                total_output_size += segment_output_size;
                // If all the input stream is consumed we are done.
                if total_input_size == src.len() {
                    break;
                }
            }
            HSpollRes::PollErrorMisuse => {
                return Err(HSError::Internal);
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
            search_index: [None; 2 << HEATSHRINK_WINDOWS_BITS],
            input_buffer: [0; 2 << HEATSHRINK_WINDOWS_BITS],
        }
    }

    /// Reset the current encoder instance
    pub fn reset(&mut self) {
        self.input_size = 0;
        self.match_scan_index = 0;
        self.match_length = 0;
        self.match_position = 0;
        self.outgoing_bits = 0;
        self.outgoing_bits_count = 0;
        self.is_finishing = false;
        self.current_byte = 0;
        self.bit_index = 8;
        self.state = HSEstate::NotFull;
        #[cfg(feature = "heatshrink-use-index")]
        {
            self.search_index.fill(None);
        }
    }

    /// Add an input buffer to be processed/compressed
    pub fn sink(&mut self, input_buffer: &[u8]) -> HSsinkRes {
        /* Sinking more content after saying the content is done, tsk tsk */
        if self.is_finishing {
            return HSsinkRes::SinkErrorMisuse;
        }

        /* Sinking more content before processing is done */
        if self.state != HSEstate::NotFull {
            return HSsinkRes::SinkFull;
        }

        let remaining_size = self.get_input_buffer_size() - self.input_size;

        if remaining_size == 0 {
            return HSsinkRes::SinkFull;
        }

        let copy_size = remaining_size.min(input_buffer.len());

        let write_offset = self.get_input_offset() + self.input_size;

        // memcpy content of input_buffer into self.input_buffer
        self.input_buffer[write_offset..write_offset + copy_size]
            .copy_from_slice(&input_buffer[0..copy_size]);
        self.input_size += copy_size;

        if self.input_size == self.get_input_buffer_size() {
            self.state = HSEstate::Filled;
        }

        HSsinkRes::SinkOK(copy_size)
    }

    /// function to process the input/internal buffer and put the compressed
    /// stream in the provided buffer.
    pub fn poll(&mut self, output_buffer: &mut [u8]) -> HSpollRes {
        if output_buffer.is_empty() {
            return HSpollRes::PollErrorMisuse;
        }

        let mut output_info = OutputInfo::new(output_buffer);

        loop {
            let previous_state = self.state;

            match previous_state {
                HSEstate::NotFull => {
                    return HSpollRes::PollEmpty(output_info.output_size);
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
                    return HSpollRes::PollEmpty(output_info.output_size);
                }
                HSEstate::Done => {
                    return HSpollRes::PollEmpty(output_info.output_size);
                }
            }

            // If the current state cannot advance, check if output
            // buffer is exhausted.
            if self.state == previous_state && !output_info.can_take_byte() {
                return HSpollRes::PollMore(output_info.output_size);
            }
        }
    }

    /// Finish the compression stream.
    pub fn finish(&mut self) -> HSfinishRes {
        self.is_finishing = true;

        if self.state == HSEstate::NotFull {
            self.state = HSEstate::Filled;
        }

        if self.state == HSEstate::Done {
            HSfinishRes::FinishDone
        } else {
            HSfinishRes::FinishMore
        }
    }

    #[inline]
    fn st_step_search(&mut self) -> HSEstate {
        if self.match_scan_index
            + (if self.is_finishing {
                1
            } else {
                self.get_lookahead_size()
            })
            > self.input_size
        {
            if self.is_finishing {
                HSEstate::FlushBits
            } else {
                HSEstate::SaveBacklog
            }
        } else {
            let end = self.get_input_offset() + self.match_scan_index;
            let start = end - self.get_input_buffer_size();
            let max_possible =
                if self.input_size < (self.get_lookahead_size() + self.match_scan_index) {
                    self.input_size - self.match_scan_index
                } else {
                    self.get_lookahead_size()
                };

            match self.find_longest_match(start, end, max_possible) {
                None => {
                    self.match_scan_index += 1;
                    self.match_length = 0;
                }
                Some(position_result) => {
                    self.match_position = position_result.0;
                    self.match_length = position_result.1;
                    assert!(self.match_position <= 1 << HEATSHRINK_WINDOWS_BITS);
                }
            }
            HSEstate::YieldTagBit
        }
    }

    #[inline]
    fn st_yield_tag_bit(&mut self, output_info: &mut OutputInfo) -> HSEstate {
        if output_info.can_take_byte() {
            if self.match_length == 0 {
                self.add_tag_bit(output_info, 0x1);
                HSEstate::YieldLiteral
            } else {
                self.add_tag_bit(output_info, 0);
                self.outgoing_bits = self.match_position as u16 - 1;
                self.outgoing_bits_count = HEATSHRINK_WINDOWS_BITS;
                HSEstate::YieldBrIndex
            }
        } else {
            HSEstate::YieldTagBit
        }
    }

    #[inline]
    fn st_yield_literal(&mut self, output_info: &mut OutputInfo) -> HSEstate {
        if output_info.can_take_byte() {
            self.push_literal_byte(output_info);
            HSEstate::Search
        } else {
            HSEstate::YieldLiteral
        }
    }

    #[inline]
    fn st_yield_br_index(&mut self, output_info: &mut OutputInfo) -> HSEstate {
        if output_info.can_take_byte() {
            if self.push_outgoing_bits(output_info) > 0 {
                HSEstate::YieldBrIndex
            } else {
                self.outgoing_bits = self.match_length as u16 - 1;
                self.outgoing_bits_count = HEATSHRINK_LOOKAHEAD_BITS;
                HSEstate::YieldBrLength
            }
        } else {
            HSEstate::YieldBrIndex
        }
    }

    #[inline]
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

    #[inline]
    fn st_save_backlog(&mut self) -> HSEstate {
        self.save_backlog();
        HSEstate::NotFull
    }

    #[inline]
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

    #[inline]
    fn add_tag_bit(&mut self, output_info: &mut OutputInfo, tag: u8) {
        self.push_bits(1, tag, output_info)
    }

    #[inline]
    fn get_input_offset(&self) -> usize {
        self.get_input_buffer_size()
    }

    #[inline]
    fn get_input_buffer_size(&self) -> usize {
        self.input_buffer.len() / 2
    }

    #[inline]
    fn get_lookahead_size(&self) -> usize {
        1 << HEATSHRINK_LOOKAHEAD_BITS
    }

    #[inline]
    fn do_indexing(&mut self) {
        #[cfg(feature = "heatshrink-use-index")]
        {
            /* Build an index array I that contains flattened linked lists
             * for the previous instances of every byte in the buffer.
             *
             * For example, if buf[200] == 'x', then index[200] will either
             * be an offset i such that buf[i] == 'x', or a None value
             * to indicate end-of-list. This significantly speeds up matching,
             * while only using sizeof(Option<u16>)*sizeof(buffer) bytes of RAM.
             */
            let mut last: [Option<u16>; 256] = [None; 256];
            let end = self.get_input_offset() + self.input_size - 1;

            for i in 0..end {
                let v: usize = self.input_buffer[i].into();
                self.search_index[i] = last[v];
                last[v] = Some(i as u16);
            }
        }
    }

    /// Return the longest match for the bytes at buf[end:end+maxlen] between
    /// buf[start] and buf[end-1]. If no match is found, return None.
    #[inline]
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
            let mut position = end - 1;

            while position >= start {
                if (self.input_buffer[position] == self.input_buffer[end])
                    && (self.input_buffer[position + match_maxlen]
                        == self.input_buffer[end + match_maxlen])
                {
                    let mut len = 1;
                    while len < maxlen {
                        if self.input_buffer[position + len] != self.input_buffer[end + len] {
                            break;
                        }
                        len += 1;
                    }

                    if len > match_maxlen {
                        match_maxlen = len;
                        match_index = position;
                        if len == maxlen {
                            // don't keep searching
                            break;
                        }
                    }
                }

                if position == 0 {
                    break;
                } else {
                    position -= 1;
                }
            }
        }

        #[cfg(feature = "heatshrink-use-index")]
        {
            let mut position = end;

            while let Some(next_position) = self.search_index[position] {
                position = next_position as usize;

                if position < start {
                    break;
                } else if self.input_buffer[position + match_maxlen]
                    != self.input_buffer[end + match_maxlen]
                {
                    continue;
                } else {
                    let mut len = 1;

                    while len < maxlen {
                        if self.input_buffer[position + len] != self.input_buffer[end + len] {
                            break;
                        }
                        len += 1;
                    }

                    if len > match_maxlen {
                        match_maxlen = len;
                        match_index = position;
                        if len == maxlen {
                            // don't keep searching
                            break;
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

    #[inline]
    fn push_outgoing_bits(&mut self, output_info: &mut OutputInfo) -> u8 {
        let (count, bits) = if self.outgoing_bits_count > 8 {
            (
                8,
                self.outgoing_bits as u8 >> (self.outgoing_bits_count - 8),
            )
        } else {
            (self.outgoing_bits_count, self.outgoing_bits as u8)
        };

        if count > 0 {
            self.push_bits(count, bits, output_info);
            self.outgoing_bits_count -= count;
        }

        count
    }

    /// Push COUNT (max 8) bits to the output buffer, which has room.
    /// Bytes are set from the lowest bits, up.
    #[inline]
    fn push_bits(&mut self, count: u8, bits: u8, output_info: &mut OutputInfo) {
        assert!(count > 0 && count <= 8);

        if count >= self.bit_index {
            let shift = count - self.bit_index;
            let tmp_byte = self.current_byte | (bits >> shift);
            output_info.push_byte(tmp_byte);
            self.bit_index = 8 - shift;
            if shift == 0 {
                self.current_byte = 0;
            } else {
                self.current_byte = bits << self.bit_index;
            }
        } else {
            self.bit_index -= count;
            self.current_byte |= bits << self.bit_index;
        }
    }

    #[inline]
    fn push_literal_byte(&mut self, output_info: &mut OutputInfo) {
        self.push_bits(
            8,
            self.input_buffer[self.get_input_offset() + self.match_scan_index - 1],
            output_info,
        );
    }

    #[inline]
    fn save_backlog(&mut self) {
        // Copy processed data to beginning of buffer, so it can be used for
        // future matches. Don't bother checking whether the input is less
        // than the maximum size, because if it isn't, we're done anyway.
        self.input_buffer.copy_within(self.match_scan_index.., 0);
        self.input_size -= self.match_scan_index;
        self.match_scan_index = 0;
    }
}
