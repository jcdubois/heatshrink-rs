#![crate_type = "rlib"]
#![no_std]
#![deny(warnings)]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Minimal compression & decompression library for embedded use
//! Implements the Heatshrink compression algorithm
//! described here <https://github.com/atomicobject/heatshrink>
//! and here <https://spin.atomicobject.com/2013/03/14/heatshrink-embedded-data-compression/>

/// module to uncompress some compressed data
pub mod decoder;
/// module to compress data
pub mod encoder;

const HEATSHRINK_WINDOWS_BITS: u8 = 8;
const HEATSHRINK_LOOKAHEAD_BITS: u8 = 4;
const HEATSHRINK_INPUT_BUFFER_SIZE: usize = 32;

/// Return code for sink finction call
#[derive(Debug)]
pub enum HSsinkRes {
    /// instance is not in correct state.
    SinkErrorMisuse,
    /// Internal buffer is full, no data was added
    SinkFull,
    /// Data was correctly added to internal buffer
    SinkOK,
}

/// Return code for poll function call
#[derive(Debug, PartialEq, Eq)]
pub enum HSpollRes {
    /// Error in input parameters
    PollErrorMisuse,
    /// More data available to be processed
    PollMore,
    /// No more data to process
    PollEmpty,
}

/// Return code for finish function call
#[derive(Debug)]
pub enum HSfinishRes {
    /// More data availble in input buffer
    FinishMore,
    /// Operation is done
    FinishDone,
}

/// Error that can be encountered while (un)compresing data
#[derive(Debug)]
pub enum HSError {
    /// The output buffer was not large enough to hold output data
    OutputFull,
    /// Some internal error did occur
    Internal,
}

/// Structure to manage the output buffer and keep track of how much it is
/// filled
pub struct OutputInfo<'a, 'b> {
    output_buffer: &'a mut [u8],
    output_size: &'b mut usize,
}

impl<'a, 'b> OutputInfo<'a, 'b> {
    /// Create a new OutputInfo instance from provided parameters
    fn new(output_buffer: &'a mut [u8], output_size: &'b mut usize) -> Self {
        OutputInfo {
            output_buffer,
            output_size,
        }
    }

    /// Add a byte to the OutputInfo referenced buffer
    fn push_byte(&mut self, byte: u8) {
        self.output_buffer[*self.output_size] = byte;
        *self.output_size += 1;
    }

    /// Check if there is space left in the OutputInfo buffer
    fn can_take_byte(&self) -> bool {
        *self.output_size < self.output_buffer.len()
    }

    /// get the free space in the buffer
    fn remaining_free_size(&self) -> usize {
        self.output_buffer.len() - *self.output_size
    }
}

#[cfg(test)]
mod test {
    use super::{decoder, encoder};

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
        let src = [
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
            24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45,
            46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67,
            68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89,
            90, 91, 92, 93, 94, 95, 96, 97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108,
            109, 110, 111, 112, 113, 114, 115, 116, 117, 118, 119, 120, 121, 122, 123, 124, 125,
            126, 127, 128, 129, 130, 131, 132, 133, 134, 135, 136, 137, 138, 139, 140, 141, 142,
            143, 144, 145, 146, 147, 148, 149, 150, 151, 152, 153, 154, 155, 156, 157, 158, 159,
            160, 161, 162, 163, 164, 165, 166, 167, 168, 169, 170, 171, 172, 173, 174, 175, 176,
            177, 178, 179, 180, 181, 182, 183, 184, 185, 186, 187, 188, 189, 190, 191, 192, 193,
            194, 195, 196, 197, 198, 199, 200, 201, 202, 203, 204, 205, 206, 207, 208, 209, 210,
            211, 212, 213, 214, 215, 216, 217, 218, 219, 220, 221, 222, 223, 224, 225, 226, 227,
            228, 229, 230, 231, 232, 233, 234, 235, 236, 237, 238, 239, 240, 241, 242, 243, 244,
            245, 246, 247, 248, 249, 250, 251, 252, 253, 254, 255, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9,
            10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
            32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53,
            54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75,
            76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97,
            98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115,
            116, 117, 118, 119, 120, 121, 122, 123, 124, 125, 126, 127, 128, 129, 130, 131, 132,
            133, 134, 135, 136, 137, 138, 139, 140, 141, 142, 143, 144, 145, 146, 147, 148, 149,
            150, 151, 152, 153, 154, 155, 156, 157, 158, 159, 160, 161, 162, 163, 164, 165, 166,
            167, 168, 169, 170, 171, 172, 173, 174, 175, 176, 177, 178, 179, 180, 181, 182, 183,
            184, 185, 186, 187, 188, 189, 190, 191, 192, 193, 194, 195, 196, 197, 198, 199, 200,
            201, 202, 203, 204, 205, 206, 207, 208, 209, 210, 211, 212, 213, 214, 215, 216, 217,
            218, 219, 220, 221, 222, 223, 224, 225, 226, 227, 228, 229, 230, 231, 232, 233, 234,
            235, 236, 237, 238, 239, 240, 241, 242, 243, 244, 245, 246, 247, 248, 249, 250, 251,
            252, 253, 254, 255,
        ];
        compare(&src);
    }

    #[test]
    fn clib_compatibility() {
        let src = hex_literal::hex!("90D4B2B549A4082BE00F000E4C46DF2817C605F005B4BE0825F00280");
        let expected = hex_literal::hex!                                        ("21529554340200000000000000000000000000000000000000000000000000000000000000000 0009302000000000000F202F102F0020000000000002F0400000000000000000000000000000000000000000000");
        let mut dst: [u8; 100] = [0; 100];

        let out = decoder::decode(&src, &mut dst).unwrap();

        assert_eq!(expected, out);
    }
}
