use super::HSError;
use super::HSfinishRes;
use super::HSpollRes;
use super::HSsinkRes;
use super::OutputInfo;
use super::HEATSHRINK_INPUT_BUFFER_SIZE;
use super::HEATSHRINK_LOOKAHEAD_BITS;
use super::HEATSHRINK_WINDOWS_BITS;

#[derive(Debug, Copy, Clone, PartialEq)]
enum HSDstate {
    TagBit,          /* tag bit */
    YieldLiteral,    /* ready to yield literal byte */
    BackrefIndexMsb, /* most significant byte of index */
    BackrefIndexLsb, /* least significant byte of index */
    BackrefCountLsb, /* least significant byte of count */
    YieldBackref,    /* ready to yield back-reference */
}

/// the decoder instance
#[derive(Debug)]
pub struct HeatshrinkDecoder {
    input_size: usize,
    input_index: usize,
    output_index: usize,
    head_index: usize,
    output_count: u16,
    current_byte: u8,
    bit_index: u8,
    state: HSDstate,
    input_buffer: [u8; HEATSHRINK_INPUT_BUFFER_SIZE],
    output_buffer: [u8; 1 << HEATSHRINK_WINDOWS_BITS],
}

/// uncompress the src buffer to the destination buffer
pub fn decode<'a>(src: &[u8], dst: &'a mut [u8]) -> Result<&'a [u8], HSError> {
    let mut total_input_size = 0;
    let mut total_output_size = 0;

    let mut dec: HeatshrinkDecoder = Default::default();

    while total_input_size < src.len() {
        // Fill the input buffer from the src buffer
        match dec.sink(&src[total_input_size..]) {
            (HSsinkRes::SinkOK, segment_input_size) => {
                total_input_size += segment_input_size;
            }
            (HSsinkRes::SinkFull, _) => {}
            (HSsinkRes::SinkErrorMisuse, _) => {
                return Err(HSError::Internal);
            }
        }

        if total_output_size == dst.len() {
            return Err(HSError::OutputFull);
        } else {
            // process the current input buffer
            match dec.poll(&mut dst[total_output_size..]) {
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

            // if all the src buffer is processed, finish the uncompress stream
            if total_input_size == src.len() {
                match dec.finish() {
                    HSfinishRes::FinishDone => {}
                    HSfinishRes::FinishMore => {
                        return Err(HSError::OutputFull);
                    }
                }
            }
        }
    }

    Ok(&dst[..total_output_size])
}

impl Default for HeatshrinkDecoder {
    fn default() -> Self {
        HeatshrinkDecoder::new()
    }
}

impl HeatshrinkDecoder {
    /// Create a new decoder instance
    pub fn new() -> Self {
        HeatshrinkDecoder {
            input_size: 0,
            input_index: 0,
            output_count: 0,
            output_index: 0,
            head_index: 0,
            current_byte: 0,
            bit_index: 0,
            state: HSDstate::TagBit,
            input_buffer: [0; HEATSHRINK_INPUT_BUFFER_SIZE],
            output_buffer: [0; 1 << HEATSHRINK_WINDOWS_BITS],
        }
    }

    /// Reset the current decoder instance
    pub fn reset(&mut self) {
        self.input_size = 0;
        self.input_index = 0;
        self.output_count = 0;
        self.output_index = 0;
        self.head_index = 0;
        self.current_byte = 0;
        self.bit_index = 0;
        self.state = HSDstate::TagBit;
        // memset self.buffer to 0
        self.input_buffer.fill(0);
        self.output_buffer.fill(0);
    }

    /// Add an input buffer to be processed/uncompressed
    pub fn sink(&mut self, input_buffer: &[u8]) -> (HSsinkRes, usize) {
        let remaining_size = self.input_buffer.len() - self.input_size;

        if remaining_size == 0 {
            return (HSsinkRes::SinkFull, 0);
        }

        let copy_size = if remaining_size < input_buffer.len() {
            remaining_size
        } else {
            input_buffer.len()
        };

        // memcpy content of input_buffer into self.input_buffer.
        self.input_buffer[self.input_size..(self.input_size + copy_size)]
            .copy_from_slice(&input_buffer[0..copy_size]);
        self.input_size += copy_size;

        if self.bit_index == 0 {
            self.current_byte = self.input_buffer[self.input_index];
            self.input_index += 1;
            self.bit_index = 8;
        }

        (HSsinkRes::SinkOK, copy_size)
    }

    /// function to process the input/internal buffer and put the uncompressed
    /// stream in the provided buffer.
    pub fn poll(&mut self, output_buffer: &mut [u8]) -> (HSpollRes, usize) {
        if output_buffer.is_empty() {
            (HSpollRes::PollErrorMisuse, 0)
        } else {
            let mut output_size: usize = 0;

            let mut output_info = OutputInfo::new(output_buffer, &mut output_size);

            loop {
                let in_state = self.state;

                match in_state {
                    HSDstate::TagBit => {
                        self.state = self.st_tag_bit();
                    }
                    HSDstate::YieldLiteral => {
                        self.state = self.st_yield_literal(&mut output_info);
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
                        self.state = self.st_yield_backref(&mut output_info);
                    }
                }

                // If the current state cannot advance, check if input or
                // output buffer are exhausted.
                if self.state == in_state {
                    if output_info.can_take_byte() {
                        return (HSpollRes::PollEmpty, output_size);
                    } else {
                        return (HSpollRes::PollMore, output_size);
                    }
                }
            }
        }
    }

    fn st_tag_bit(&mut self) -> HSDstate {
        match self.get_bits(1) {
            None => HSDstate::TagBit,
            Some(0) => {
                self.output_index = 0;
                HSDstate::BackrefIndexLsb
            }
            Some(_) => HSDstate::YieldLiteral,
        }
    }

    fn st_yield_literal(&mut self, output_info: &mut OutputInfo) -> HSDstate {
        // Emit a repeated section from the window buffer, and add it (again)
        // to the window buffer. (Note that the repetition can include itself)
        if output_info.can_take_byte() {
            match self.get_bits(8) {
                None => HSDstate::YieldLiteral, // input_buffer is consumed
                Some(x) => {
                    let c: u8 = x;
                    let len = self.output_buffer.len();
                    self.output_buffer[self.head_index % len] = c;
                    self.head_index += 1;
                    output_info.push_byte(c);
                    HSDstate::TagBit
                }
            }
        } else {
            HSDstate::YieldLiteral
        }
    }

    fn st_backref_index_msb(&mut self) -> HSDstate {
        match self.get_bits(0) {
            None => HSDstate::BackrefIndexMsb,
            Some(x) => {
                self.output_index = (x as usize) << 8;
                HSDstate::BackrefIndexLsb
            }
        }
    }

    fn st_backref_index_lsb(&mut self) -> HSDstate {
        match self.get_bits(8) {
            None => HSDstate::BackrefIndexLsb,
            Some(x) => {
                self.output_index |= x as usize;
                self.output_index += 1;
                self.output_count = 0;
                HSDstate::BackrefCountLsb
            }
        }
    }

    fn st_backref_count_lsb(&mut self) -> HSDstate {
        match self.get_bits(HEATSHRINK_LOOKAHEAD_BITS) {
            None => HSDstate::BackrefCountLsb,
            Some(x) => {
                self.output_count |= x as u16;
                self.output_count += 1;
                HSDstate::YieldBackref
            }
        }
    }

    fn st_yield_backref(&mut self, output_info: &mut OutputInfo) -> HSDstate {
        if output_info.can_take_byte() {
            let mut i: usize = 0;
            let len = self.output_buffer.len();
            let mut head_index = self.head_index;
            let output_index = self.output_index;

            let count = if output_info.remaining_free_size() > usize::from(self.output_count) {
                usize::from(self.output_count)
            } else {
                output_info.remaining_free_size()
            };

            while i < count {
                let c = if output_index > head_index {
                    0
                } else {
                    self.output_buffer[(head_index - output_index) % len]
                };
                output_info.push_byte(c);
                self.output_buffer[head_index % len] = c;
                head_index += 1;
                i += 1;
            }

            self.head_index = head_index;
            self.output_count -= count as u16;

            if self.output_count == 0 {
                return HSDstate::TagBit;
            }
        }
        HSDstate::YieldBackref
    }

    /// Get the next COUNT bits from the input buffer, saving incremental
    /// progress. Returns None on end of input.
    fn get_bits(&mut self, count: u8) -> Option<u8> {
        assert!(count <= 8);

        // If we aren't able to get COUNT bits, suspend immediately, because
        // we don't track how many bits of COUNT we've accumulated before
        // suspend.
        if (((self.input_size - self.input_index) * 8) + self.bit_index as usize) < count as usize {
            return None;
        }

        // Get the current byte in the accumulator
        let mut accumulator = self.current_byte as u16;
        // mask upper bits (already consumed)
        accumulator %= 1 << self.bit_index;

        if count < self.bit_index {
            // enough bits left in the current_byte
            // shift accumulator right
            accumulator >>= self.bit_index - count;
            // update bit_index
            self.bit_index -= count;
        } else if count == self.bit_index {
            // We are consuming exactly the bits left in current_byte
            if self.input_size == self.input_index {
                // we should load the next byte but the buffer is consumed
                // So let's set the bit_index to 0 to show there is nothning
                // left to consume.
                self.bit_index = 0;
                // This will be set to 8 on next sink
            } else {
                // load next byte.
                self.current_byte = self.input_buffer[self.input_index];
                // increase the consumed index
                self.input_index += 1;
                // reset the bit index
                self.bit_index = 8;
            }
        } else {
            // count > self.bit_index
            // we need to take some bits from next byte
            // shift accumulator (8 bits) left
            accumulator <<= 8;
            // consume next byte from the input buffer
            self.current_byte = self.input_buffer[self.input_index];
            // increase the consumed index
            self.input_index += 1;
            // add the byte read to the accumulator
            accumulator += self.current_byte as u16;
            // update bit_index
            self.bit_index += 8 - count;
            // shift accumulator right
            accumulator >>= self.bit_index;
        }

        // if we reach the end of buffer, reset input_index and input_size
        if self.input_index == self.input_size {
            self.input_index = 0;
            self.input_size = 0;
            // Next call to poll will likely return None (depending on
            // bit_index) and require a call to sink to continue.
        }

        Some(accumulator as u8)
    }

    /// Finish the uncompress stream
    pub fn finish(&self) -> HSfinishRes {
        // Return Done if input_buffer is consumed. Else return More.
        if self.input_size == 0 {
            HSfinishRes::FinishDone
        } else {
            HSfinishRes::FinishMore
        }
    }
}
