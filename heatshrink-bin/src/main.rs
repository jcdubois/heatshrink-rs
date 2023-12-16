use clap::{ArgGroup, Parser};
use std::fs::File;
use std::io;
use std::io::{BufReader, BufWriter};
use std::io::{Read, Write};

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
        help = "Print input & output sizes, compression ratio, etc"
    )]
    verbose: bool,

    #[clap(
        short = 'w',
        long = "window",
        help = "Base-2 log of LZSS sliding window size",
        default_value_t = 8
    )]
    size: u8,

    #[clap(
        short = 'l',
        long = "length",
        help = "Number of bits used for back-reference lengths",
        default_value_t = 4
    )]
    bits: u8,

    /// some regular input. It will default to stdin if unspecified.
    #[clap(group = "input")]
    input_file: Option<String>,

    /// some regular output. It will default to stdout if unspecified.
    #[clap(group = "output")]
    output_file: Option<String>,
}

fn report(use_stderr: bool, file_name: &String, input_len: usize, output_len: usize) {
    if use_stderr {
        eprintln!(
            "{0:} {1:.2}% \t{2:} -> {3:} (-w {4:} -l {5:})",
            file_name,
            100.0 - (100.0 * output_len as f32) / input_len as f32,
            input_len,
            output_len,
            heatshrink::HEATSHRINK_WINDOWS_BITS,
            heatshrink::HEATSHRINK_LOOKAHEAD_BITS
        );
    } else {
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
}

fn encode(
    input_file: &mut Box<dyn Read>,
    output_file: &mut Box<dyn Write>,
) -> Result<(usize, usize), io::Error> {
    let mut input_buffer = [0u8; HEATSHRINK_APP_BUFFER_SIZE];
    let mut output_buffer = [0u8; HEATSHRINK_APP_BUFFER_SIZE];
    let mut encoding_is_complete = false;
    let mut total_input_byte_size = 0;
    let mut total_output_byte_size = 0;

    let mut enc: heatshrink::encoder::HeatshrinkEncoder = Default::default();

    let mut output_bytes_processed = 0;

    loop {
        match input_file.read(&mut input_buffer[0..]) {
            Err(err) => return Err(err),
            Ok(input_bytes_read) => {
                total_input_byte_size += input_bytes_read;

                let mut input_bytes_processed = 0;

                loop {
                    if input_bytes_read > 0 {
                        match enc.sink(&input_buffer[input_bytes_processed..input_bytes_read]) {
                            heatshrink::HSsinkRes::SinkOK(segment_input_size) => {
                                // Data has been added to the encoder.
                                // Let's try to process/poll it
                                input_bytes_processed += segment_input_size;
                            }
                            heatshrink::HSsinkRes::SinkFull => {
                                // Hum ... no data was added to the encoder because
                                // the internal buffer was already full.
                                eprintln!("Input buffer is full and unprocessed");
                                return Err(io::ErrorKind::Other.into());
                            }
                            heatshrink::HSsinkRes::SinkErrorMisuse => {
                                eprintln!("Error in HeatshrinkEncoder::sink()");
                                return Err(io::ErrorKind::Other.into());
                            }
                        }
                    }

                    loop {
                        // process the current input buffer
                        match enc.poll(&mut output_buffer[output_bytes_processed..]) {
                            heatshrink::HSpollRes::PollMore(segment_output_size) => {
                                output_bytes_processed += segment_output_size;
                                let mut buf_begin = 0;
                                while buf_begin != output_bytes_processed {
                                    match output_file
                                        .write(&output_buffer[buf_begin..output_bytes_processed])
                                    {
                                        Err(err) => return Err(err),
                                        Ok(bytes_written) => {
                                            buf_begin += bytes_written;
                                        }
                                    }
                                }
                                total_output_byte_size += output_bytes_processed;
                                output_bytes_processed = 0;
                                // Some more data is avaialble in input_buffer.
                                // Let's loop.
                            }
                            heatshrink::HSpollRes::PollEmpty(segment_output_size) => {
                                output_bytes_processed += segment_output_size;
                                // The input_buffer is consumed.
                                // Exit the loop.
                                break;
                            }
                            heatshrink::HSpollRes::PollErrorMisuse => {
                                eprintln!("Error in HeatshrinkEncoder::poll()");
                                return Err(io::ErrorKind::Other.into());
                            }
                        }
                    }

                    if input_bytes_read == 0 {
                        if output_bytes_processed != 0 {
                            let mut buf_begin = 0;
                            while buf_begin != output_bytes_processed {
                                match output_file
                                    .write(&output_buffer[buf_begin..output_bytes_processed])
                                {
                                    Err(err) => return Err(err),
                                    Ok(bytes_written) => {
                                        buf_begin += bytes_written;
                                    }
                                }
                            }
                            total_output_byte_size += output_bytes_processed;
                            output_bytes_processed = 0;
                        }
                        if let heatshrink::HSfinishRes::FinishDone = enc.finish() {
                            encoding_is_complete = true;
                            break;
                        }
                    }

                    if input_bytes_read == input_bytes_processed {
                        break;
                    }
                }
            }
        }

        if encoding_is_complete {
            break;
        }
    }

    Ok((total_input_byte_size, total_output_byte_size))
}

fn decode(
    input_file: &mut Box<dyn Read>,
    output_file: &mut Box<dyn Write>,
) -> Result<(usize, usize), io::Error> {
    let mut input_buffer = [0u8; HEATSHRINK_APP_BUFFER_SIZE];
    let mut output_buffer = [0u8; HEATSHRINK_APP_BUFFER_SIZE];
    let mut total_input_byte_size = 0;
    let mut total_output_byte_size = 0;

    let mut dec: heatshrink::decoder::HeatshrinkDecoder = Default::default();

    let mut output_bytes_processed = 0;

    loop {
        match input_file.read(&mut input_buffer) {
            Err(err) => return Err(err),
            Ok(input_bytes_read) => {
                total_input_byte_size += input_bytes_read;

                if input_bytes_read == 0 {
                    match dec.finish() {
                        heatshrink::HSfinishRes::FinishDone => {
                            if output_bytes_processed != 0 {
                                let mut buf_begin = 0;
                                while buf_begin != output_bytes_processed {
                                    match output_file
                                        .write(&output_buffer[buf_begin..output_bytes_processed])
                                    {
                                        Err(err) => return Err(err),
                                        Ok(bytes_written) => {
                                            buf_begin += bytes_written;
                                        }
                                    }
                                }
                                total_output_byte_size += output_bytes_processed;
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
                        heatshrink::HSsinkRes::SinkOK(segment_input_size) => {
                            // Data has been added to the decoder.
                            // Let's try to process/poll it
                            input_bytes_processed += segment_input_size;
                        }
                        heatshrink::HSsinkRes::SinkFull => {
                            // Hum ... no data was added to the decoder because
                            // the internal buffer was already full.
                            eprintln!("Input buffer is full and unprocessed");
                            return Err(io::ErrorKind::Other.into());
                        }
                        heatshrink::HSsinkRes::SinkErrorMisuse => {
                            // We should abort/assert/return
                            eprintln!("Error in HeatshrinkDecoder::sink()");
                            return Err(io::ErrorKind::Other.into());
                        }
                    }

                    loop {
                        // process the current input buffer
                        match dec.poll(&mut output_buffer[output_bytes_processed..]) {
                            heatshrink::HSpollRes::PollMore(segment_output_size) => {
                                output_bytes_processed += segment_output_size;
                                let mut buf_begin = 0;
                                while buf_begin != output_bytes_processed {
                                    match output_file
                                        .write(&output_buffer[buf_begin..output_bytes_processed])
                                    {
                                        Err(err) => return Err(err),
                                        Ok(bytes_written) => {
                                            buf_begin += bytes_written;
                                        }
                                    }
                                }
                                total_output_byte_size += output_bytes_processed;
                                output_bytes_processed = 0;
                                // Some more data is avaialble in input_buffer.
                                // Let's loop.
                            }
                            heatshrink::HSpollRes::PollEmpty(segment_output_size) => {
                                output_bytes_processed += segment_output_size;
                                // The input_buffer is consumed.
                                // Exit the loop.
                                break;
                            }
                            heatshrink::HSpollRes::PollErrorMisuse => {
                                // We should abort/assert/return
                                eprintln!("Error in HeatshrinkDecoder::poll()");
                                return Err(io::ErrorKind::Other.into());
                            }
                        }
                    }
                }
            }
        }
    }
    Ok((total_input_byte_size, total_output_byte_size))
}

fn main() {
    // parse the command line parameters
    let args = Cli::parse();

    if args.size != heatshrink::HEATSHRINK_WINDOWS_BITS {
        eprintln!(
            "For now only the default value [{0:}] is supported for window size",
            heatshrink::HEATSHRINK_WINDOWS_BITS
        );
        std::process::exit(1);
    }

    if args.bits != heatshrink::HEATSHRINK_LOOKAHEAD_BITS {
        eprintln!(
            "For now only the default value [{0:}] is supported for back-reference length",
            heatshrink::HEATSHRINK_LOOKAHEAD_BITS
        );
        std::process::exit(1);
    }

    // Open input file for read
    let mut input_file: Box<dyn Read> = match args.input_file {
        // if no file name was provided use stdin instead
        None => Box::new(BufReader::new(io::stdin())),
        Some(ref filename) => match File::open(filename) {
            Ok(file) => Box::new(BufReader::new(file)),
            Err(err) => {
                eprintln!("Could not open file \"{}\" : {}", filename, err);
                std::process::exit(1)
            }
        },
    };

    // Open output file for write
    let mut output_file: Box<dyn Write> = match args.output_file {
        // if no file name was provided use stdout instead
        None => Box::new(BufWriter::new(io::stdout())),
        Some(ref filename) => match File::create(filename) {
            Ok(file) => Box::new(BufWriter::new(file)),
            Err(err) => {
                eprintln!("Could not create file \"{}\" : {}", filename, err);
                std::process::exit(1)
            }
        },
    };

    match if args.encode {
        encode(&mut input_file, &mut output_file)
    } else {
        decode(&mut input_file, &mut output_file)
    } {
        Err(err) => {
            eprintln!("encode/decode operation failed : {}", err);
            std::process::exit(1)
        }
        Ok((input_size, output_size)) => {
            // Output log if requested
            if args.verbose {
                let file_name = match args.input_file {
                    None => "-".to_string(),
                    Some(ref filename) => filename.to_string(),
                };
                report(
                    args.output_file.is_none(),
                    &file_name,
                    input_size,
                    output_size,
                );
            }
        }
    }
}
