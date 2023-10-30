use clap::{ArgGroup, Parser};
use std::fs::File;
use std::io::{Read, Write};

//const HEATSHRINK_APP_BUFFER_SIZE: usize = 4096;
const HEATSHRINK_APP_BUFFER_SIZE: usize = 64 * 1024;

#[derive(Parser)] // requires `derive` feature
#[clap(author, version, about, long_about = None)]
#[clap(group(ArgGroup::new("command").required(true).args(&["encode", "decode"])))]
struct Cli {
    #[clap(short = 'e', long = "encode", help = "Compress data")]
    encode: bool,

    #[clap(short = 'd', long = "decode", help = "Decompress data")]
    decode: bool,

    #[clap(
        short = 'v',
        long = "verbose",
        help = "Print input & output sizes, compression ratio, etc."
    )]
    verbose: bool,

    /// some regular input
    #[clap(group = "input")]
    input_file: Option<String>,

    /// some regular output
    #[clap(group = "output")]
    output_file: Option<String>,
}

fn report(file_name: &String, input_file: &File, output_file: &File) {
    // size of the input file
    let input_len = input_file.metadata().unwrap().len();
    // size of the output file
    let output_len = output_file.metadata().unwrap().len();

    println!(
        "{0:} {1:.2}% \t{2:} -> {3:} (-w {4:} -l {5:})",
        file_name,
        100.0 - (100.0 * output_len as f32) / input_len as f32,
        input_len,
        output_len,
        heatshrink::HEATSHRINK_WINDOWS_BITS,
        heatshrink::HEATSHRINK_LOOKAHEAD_BITS
    );
}

fn encode(mut input_file: &File, mut output_file: &File) {
    let mut input_buffer = [0u8; HEATSHRINK_APP_BUFFER_SIZE];
    let mut output_buffer = [0u8; HEATSHRINK_APP_BUFFER_SIZE];
    let mut encoding_is_complete = false;

    let mut enc: heatshrink::encoder::HeatshrinkEncoder = Default::default();

    loop {
        let input_bytes_read = input_file.read(&mut input_buffer[0..]).unwrap();

        let mut input_bytes_processed = 0;

        loop {
            if input_bytes_read > 0 {
                match enc.sink(&input_buffer[input_bytes_processed..input_bytes_read]) {
                    (heatshrink::HSsinkRes::SinkOK, segment_input_size) => {
                        // Data has been added to the encoder.
                        // Let's try to process/poll it
                        input_bytes_processed += segment_input_size;
                    }
                    (heatshrink::HSsinkRes::SinkFull, _) => {
                        // Hum ... no data was added to the encoder because
                        // the internal buffer was already full.
                        panic!("Input buffer is full and unprocessed");
                    }
                    (heatshrink::HSsinkRes::SinkErrorMisuse, _) => {
                        panic!("Error in HeatshrinkEncoder::sink()");
                    }
                }
            }

            let mut output_bytes_processed = 0;

            loop {
                // process the current input buffer
                match enc.poll(&mut output_buffer[0..]) {
                    (heatshrink::HSpollRes::PollMore, x) => {
                        if x != 0 {
                            output_bytes_processed = x;
                            let _ = output_file
                                .write(&output_buffer[0..output_bytes_processed])
                                .unwrap();
                        }
                        // Some more data is avaialble in input_buffer.
                        // Let's loop.
                    }
                    (heatshrink::HSpollRes::PollEmpty, x) => {
                        if x != 0 {
                            output_bytes_processed = x;
                            let _ = output_file
                                .write(&output_buffer[0..output_bytes_processed])
                                .unwrap();
                        }
                        // The input_buffer is consumed.
                        // Exit the poll loop.
                        break;
                    }
                    (heatshrink::HSpollRes::PollErrorMisuse, _) => {
                        panic!("Error in HeatshrinkEncoder::poll()");
                    }
                }
            }

            if input_bytes_read == 0 && output_bytes_processed == 0 {
                if let heatshrink::HSfinishRes::FinishDone = enc.finish() {
                    encoding_is_complete = true;
                    break;
                }
            }

            if input_bytes_read == input_bytes_processed {
                break;
            }
        }

        if encoding_is_complete {
            break;
        }
    }
}

fn decode(mut input_file: &File, mut output_file: &File) {
    let mut input_buffer = [0u8; HEATSHRINK_APP_BUFFER_SIZE];
    let mut output_buffer = [0u8; HEATSHRINK_APP_BUFFER_SIZE];

    let mut dec: heatshrink::decoder::HeatshrinkDecoder = Default::default();

    let mut output_bytes_processed = 0;

    loop {
        let input_bytes_read = input_file.read(&mut input_buffer).unwrap();

        if input_bytes_read == 0 {
            match dec.finish() {
                heatshrink::HSfinishRes::FinishDone => {
                    if output_bytes_processed != 0 {
                        let _ = output_file
                            .write(&output_buffer[0..output_bytes_processed])
                            .unwrap();
                    }
                    // the input input_buffer if empty now.
                    break;
                }
                heatshrink::HSfinishRes::FinishMore => {
                    // More data to be processed ?
                }
            }
        }

        let mut input_bytes_processed = 0;

        while input_bytes_processed < input_bytes_read {
            match dec.sink(&input_buffer[input_bytes_processed..input_bytes_read]) {
                (heatshrink::HSsinkRes::SinkOK, segment_input_size) => {
                    // Data has been added to the decoder.
                    // Let's try to process/poll it
                    input_bytes_processed += segment_input_size;
                }
                (heatshrink::HSsinkRes::SinkFull, _) => {
                    // Hum ... no data was added to the decoder because
                    // the internal buffer was already full.
                    panic!("Input buffer is full and unprocessed");
                }
                (heatshrink::HSsinkRes::SinkErrorMisuse, _) => {
                    // We should abort/assert/return
                    panic!("Error in HeatshrinkDecoder::sink()");
                }
            }

            loop {
                // process the current input buffer
                match dec.poll(&mut output_buffer[output_bytes_processed..]) {
                    (heatshrink::HSpollRes::PollMore, segment_output_size) => {
                        output_bytes_processed += segment_output_size;
                        let _ = output_file
                            .write(&output_buffer[0..output_bytes_processed])
                            .unwrap();
                        output_bytes_processed = 0;
                        // Some more data is avaialble in input_buffer.
                        // Let's loop.
                    }
                    (heatshrink::HSpollRes::PollEmpty, segment_output_size) => {
                        output_bytes_processed += segment_output_size;
                        // The input_buffer is consumed.
                        // Exit the loop.
                        break;
                    }
                    (heatshrink::HSpollRes::PollErrorMisuse, _) => {
                        // We should abort/assert/return
                        panic!("Error in HeatshrinkDecoder::poll()");
                    }
                }
            }
        }
    }
}

fn main() {
    // parse the command line parameters
    let args = Cli::parse();

    // Open input file for read
    let input_file = File::open(args.input_file.as_ref().unwrap()).unwrap();
    // Open output file for write
    let output_file = File::create(args.output_file.as_ref().unwrap()).unwrap();

    // Process the file
    if args.encode {
        encode(&input_file, &output_file);
    } else {
        decode(&input_file, &output_file);
    }

    // Output log if requested
    if args.verbose {
        report(&args.input_file.unwrap(), &input_file, &output_file);
    }
}
