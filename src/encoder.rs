use super::HSError;
use super::HSfinishRes;
use super::HSpollRes;
use super::HSsinkRes;
use super::OutputInfo;
use super::HEATSHRINK_LOOKAHEAD_BITS;
use super::HEATSHRINK_WINDOWS_BITS;

#[derive(Debug, Copy, Clone, PartialEq)]
enum HSEstate {
    HSESNotFull,       /* input buffer not full enough */
    HSESFilled,        /* buffer is full */
    HSESSearch,        /* searching for patterns */
    HSESYieldTagBit,   /* yield tag bit */
    HSESYieldLiteral,  /* emit literal byte */
    HSESYieldBrIndex,  /* yielding backref index */
    HSESYieldBrLength, /* yielding backref length */
    HSESSaveBacklog,   /* copying buffer to backlog */
    HSESFlushBits,     /* flush bit buffer */
    HSESDone,          /* done */
}

#[cfg(not(feature = "heatshrink-use-index"))]
/// The encoder instance
#[derive(Debug)]
pub struct HeatshrinkEncoder {
    input_size: u16,
    match_scan_index: u16,
    match_length: u16,
    match_pos: u16,
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
    input_size: u16,
    match_scan_index: u16,
    match_length: u16,
    match_pos: u16,
    outgoing_bits: u16,
    outgoing_bits_count: u8,
    flags: u8,
    current_byte: u8,
    bit_index: u8,
    state: HSEstate,
    search_index: [i16; 2 << HEATSHRINK_WINDOWS_BITS],
    input_buffer: [u8; 2 << HEATSHRINK_WINDOWS_BITS],
}

/// A constant flag to set an encoder as finishing
const FLAG_IS_FINISHING: u8 = 1;

/// compress the src buffer to the destination buffer
pub fn encode<'a>(src: &[u8], dst: &'a mut [u8]) -> Result<&'a [u8], HSError> {
    let mut total_input_size = 0;
    let mut total_output_size = 0;

    let mut enc = HeatshrinkEncoder::new();

    while total_input_size < src.len() {
        let mut segment_input_size = 0;

        // Fill the input buffer from the src buffer
        match enc.sink(&src[total_input_size..], &mut segment_input_size) {
            HSsinkRes::HSRSinkOK => {
                total_input_size += segment_input_size;
            }
            HSsinkRes::HSRSinkFull => {}
            HSsinkRes::HSRSinkErrorMisuse => {
                return Err(HSError::HSErrorInternal);
            }
        }

        // if all the src buffer is processed, finish the compress stream
        if total_input_size == src.len() {
            match enc.finish() {
                HSfinishRes::HSRFinishDone => {}
                HSfinishRes::HSRFinishMore => {}
            }
        }

        if total_output_size == dst.len() {
            return Err(HSError::HSErrorOutputFull);
        } else {
            // process the current input buffer
            loop {
                let mut segment_output_size = 0;

                match enc.poll(&mut dst[total_output_size..], &mut segment_output_size) {
                    HSpollRes::HSRPollMore => {
                        return Err(HSError::HSErrorOutputFull);
                    }
                    HSpollRes::HSRPollEmpty => {
                        total_output_size += segment_output_size;
                        break;
                    }
                    HSpollRes::HSRPollErrorMisuse => {
                        return Err(HSError::HSErrorInternal);
                    }
                }
            }
        }
    }

    Ok(&dst[..total_output_size])
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
                bit_index: 0x80,
                state: HSEstate::HSESNotFull,
                search_index: [0; 2 << HEATSHRINK_WINDOWS_BITS],
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
                bit_index: 0x80,
                state: HSEstate::HSESNotFull,
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
        self.bit_index = 0x80;
        self.state = HSEstate::HSESNotFull;
        // memset self.buffer to 0
        self.input_buffer.iter_mut().for_each(|m| *m = 0);
        #[cfg(feature = "heatshrink-use-index")]
        {
            // memset self.search_index to 0
            self.search_index.iter_mut().for_each(|m| *m = 0);
        }
    }

    /// Add an input buffer to be processed/compressed
    pub fn sink(&mut self, input_buffer: &[u8], input_size: &mut usize) -> HSsinkRes {
        /* Sinking more content after saying the content is done, tsk tsk */
        if self.is_finishing() {
            return HSsinkRes::HSRSinkErrorMisuse;
        }

        /* Sinking more content before processing is done */
        if self.state != HSEstate::HSESNotFull {
            return HSsinkRes::HSRSinkErrorMisuse;
        }

        let write_offset: usize = (self.get_input_offset() + self.input_size).into();
        let remaining_size: usize = (self.get_input_buffer_size() - self.input_size).into();

        if remaining_size == 0 {
            *input_size = 0;
            return HSsinkRes::HSRSinkFull;
        }

        let copy_size = if remaining_size < input_buffer.len() {
            remaining_size
        } else {
            input_buffer.len()
        };

        // memcpy content of input_buffer into self.input_buffer
        self.input_buffer[write_offset..write_offset + copy_size]
            .copy_from_slice(&input_buffer[0..copy_size]);
        self.input_size += copy_size as u16;
        *input_size = copy_size;

        if self.input_size == self.get_input_buffer_size() {
            self.state = HSEstate::HSESFilled;
        }

        return HSsinkRes::HSRSinkOK;
    }

    /// function to process the input/internal buffer and put the compressed
    /// stream in the provided buffer.
    pub fn poll(&mut self, output_buffer: &mut [u8], output_size: &mut usize) -> HSpollRes {
        *output_size = 0;

        if output_buffer.len() == 0 {
            return HSpollRes::HSRPollMore;
        } else {
            let mut output_info = OutputInfo::new(output_buffer, output_size);

            loop {
                let in_state = self.state;

                match in_state {
                    HSEstate::HSESNotFull => {
                        return HSpollRes::HSRPollEmpty;
                    }
                    HSEstate::HSESFilled => {
                        self.do_indexing();
                        self.state = HSEstate::HSESSearch;
                    }
                    HSEstate::HSESSearch => {
                        self.state = self.st_step_search();
                    }
                    HSEstate::HSESYieldTagBit => {
                        self.state = self.st_yield_tag_bit(&mut output_info);
                    }
                    HSEstate::HSESYieldLiteral => {
                        self.state = self.st_yield_literal(&mut output_info);
                    }
                    HSEstate::HSESYieldBrIndex => {
                        self.state = self.st_yield_br_index(&mut output_info);
                    }
                    HSEstate::HSESYieldBrLength => {
                        self.state = self.st_yield_br_length(&mut output_info);
                    }
                    HSEstate::HSESSaveBacklog => {
                        self.state = self.st_save_backlog();
                    }
                    HSEstate::HSESFlushBits => {
                        self.state = self.st_flush_bit_buffer(&mut output_info);
                    }
                    HSEstate::HSESDone => {
                        return HSpollRes::HSRPollEmpty;
                    }
                }

                // If the current state cannot advance, check if output
                // buffer is exhausted.
                if self.state == in_state {
                    if !output_info.can_take_byte() {
                        return HSpollRes::HSRPollMore;
                    }
                }
            }
        }
    }

    /// Finish the compression stream.
    pub fn finish(&mut self) -> HSfinishRes {
        self.flags |= FLAG_IS_FINISHING;

        if self.state == HSEstate::HSESNotFull {
            self.state = HSEstate::HSESFilled;
        }

        if self.state == HSEstate::HSESDone {
            HSfinishRes::HSRFinishDone
        } else {
            HSfinishRes::HSRFinishMore
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
                HSEstate::HSESFlushBits
            } else {
                HSEstate::HSESSaveBacklog
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
                }
            }
            HSEstate::HSESYieldTagBit
        }
    }

    fn st_yield_tag_bit(&mut self, output_info: &mut OutputInfo) -> HSEstate {
        if output_info.can_take_byte() {
            if self.match_length == 0 {
                self.add_tag_bit(output_info, 0x1);
                HSEstate::HSESYieldLiteral
            } else {
                self.add_tag_bit(output_info, 0);
                self.outgoing_bits = self.match_pos - 1;
                self.outgoing_bits_count = 8;
                HSEstate::HSESYieldBrIndex
            }
        } else {
            HSEstate::HSESYieldTagBit
        }
    }

    fn st_yield_literal(&mut self, output_info: &mut OutputInfo) -> HSEstate {
        if output_info.can_take_byte() {
            self.push_literal_byte(output_info);
            HSEstate::HSESSearch
        } else {
            HSEstate::HSESYieldLiteral
        }
    }

    fn st_yield_br_index(&mut self, output_info: &mut OutputInfo) -> HSEstate {
        if output_info.can_take_byte() {
            if self.push_outgoing_bits(output_info) > 0 {
                HSEstate::HSESYieldBrIndex
            } else {
                self.outgoing_bits = self.match_length - 1;
                self.outgoing_bits_count = 4;
                HSEstate::HSESYieldBrLength
            }
        } else {
            HSEstate::HSESYieldBrIndex
        }
    }

    fn st_yield_br_length(&mut self, output_info: &mut OutputInfo) -> HSEstate {
        if output_info.can_take_byte() {
            if self.push_outgoing_bits(output_info) > 0 {
                HSEstate::HSESYieldBrLength
            } else {
                self.match_scan_index += self.match_length;
                self.match_length = 0;
                HSEstate::HSESSearch
            }
        } else {
            HSEstate::HSESYieldBrLength
        }
    }

    fn st_save_backlog(&mut self) -> HSEstate {
        self.save_backlog();
        HSEstate::HSESNotFull
    }

    fn st_flush_bit_buffer(&self, output_info: &mut OutputInfo) -> HSEstate {
        if self.bit_index == 0x80 {
            HSEstate::HSESDone
        } else if output_info.can_take_byte() {
            output_info.push_byte(self.current_byte);
            HSEstate::HSESDone
        } else {
            HSEstate::HSESFlushBits
        }
    }

    fn add_tag_bit(&mut self, output_info: &mut OutputInfo, tag: u8) {
        self.push_bits(1, tag, output_info)
    }

    fn get_input_offset(&self) -> u16 {
        self.get_input_buffer_size()
    }

    fn get_input_buffer_size(&self) -> u16 {
        (self.input_buffer.len() / 2) as u16
    }

    fn get_lookahead_size(&self) -> u16 {
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
             * while only using sizeof(int16_t)*sizeof(buffer) bytes of RAM.
             *
             * Future optimization options:
             * 1. Since any negative value represents end-of-list, the other
             *    15 bits could be used to improve the index dynamically.
             *
             * 2. Likewise, the last lookahead_sz bytes of the index will
             *    not be usable, so temporary data could be stored there to
             *    dynamically improve the index.
             * */
            let mut last: [i16; 256] = [-1; 256];
            let end : usize = (self.get_input_offset() + self.input_size - 1).into();

            for i in 0..end {
                let v: usize = self.input_buffer[i].into();
                self.search_index[i] = last[v];
                last[v] = i.into();
            }
        }
    }

    fn find_longest_match(
        &self,
        start: u16,
        end: u16,
        maxlen: u16,
    ) -> Option<(u16, u16)> {
        let mut match_maxlen: usize = 0;
        let mut match_index: usize = 0;

        #[cfg(not(feature = "heatshrink-use-index"))]
        {
            let mut pos: usize = (end - 1).into();

            while pos >= start.into() {
                if (self.input_buffer[pos + match_maxlen]
                    == self.input_buffer[end as usize + match_maxlen])
                    && (self.input_buffer[pos] == self.input_buffer[end as usize])
                {
                    let mut len: usize = 1;
                    while len < maxlen.into() {
                        if self.input_buffer[pos + len] != self.input_buffer[end as usize + len] {
                            break;
                        }
                        len += 1;
                    }
                    if len > match_maxlen {
                        match_maxlen = len;
                        match_index = pos;
                        if len == maxlen.into() {
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
            let mut pos = self.search_index[end as usize];

            while pos >= start.into() {
                let mut len: usize = 1;

                if self.input_buffer[pos as usize + match_maxlen]
                    != self.input_buffer[end as usize + match_maxlen]
                {
                    pos = self.search_index[pos as usize];
                    continue;
                }

                while len < maxlen as usize {
                    if self.input_buffer[pos as usize + len]
                        != self.input_buffer[end as usize + len]
                    {
                        break;
                    }
                    len += 1;
                }

                if len > match_maxlen {
                    match_maxlen = len as usize;
                    match_index = pos as usize;
                    if len == maxlen as usize {
                        break;
                    }
                }

                pos = self.search_index[pos as usize];
            }
        }

        let break_even_point: usize =
            (1 + HEATSHRINK_WINDOWS_BITS + HEATSHRINK_LOOKAHEAD_BITS).into();

        if match_maxlen > (break_even_point / 8) {
            Some((end - match_index as u16, match_maxlen as u16))
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

    fn push_bits(&mut self, count: u8, bits: u8, output_info: &mut OutputInfo) {
        if count == 8 && self.bit_index == 0x80 {
            output_info.push_byte(bits);
        } else {
            let mut i: u8 = count;
            while i != 0 {
                if (bits & (1 << (i - 1))) != 0 {
                    self.current_byte |= self.bit_index;
                }
                self.bit_index = self.bit_index >> 1;
                if self.bit_index == 0 {
                    self.bit_index = 0x80;
                    output_info.push_byte(self.current_byte);
                    self.current_byte = 0;
                }
                i -= 1;
            }
        }
    }

    fn push_literal_byte(&mut self, output_info: &mut OutputInfo) {
        let input_offset = self.match_scan_index - 1;
        let c = self.input_buffer[(self.get_input_offset() + input_offset) as usize];
        self.push_bits(8, c, output_info);
    }

    fn save_backlog(&mut self) {
        // Copy processed data to beginning of buffer, so it can be used for
        // future matches. Don't bother checking whether the input is less
        // than the maximum size, because if it isn't, we're done anyway.
        let remaining_size = self.get_input_buffer_size() - self.match_scan_index; // unprocessed bytes
        let shift_size = self.get_input_buffer_size() + remaining_size;
        self.input_buffer.copy_within(
            self.match_scan_index as usize..(self.match_scan_index + shift_size) as usize,
            0,
        );
        self.match_scan_index = 0;
        self.input_size -= self.get_input_buffer_size() - remaining_size;
    }
}
