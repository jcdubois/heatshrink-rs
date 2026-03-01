use clap::{ArgGroup, Parser};
use std::fs::File;
use std::io;
use std::io::{BufReader, BufWriter};
use std::io::{Read, Write};

const HEATSHRINK_APP_BUFFER_SIZE: usize = 64 * 1024;

#[derive(Parser)]
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

    /// Input file (defaults to stdin if unspecified)
    #[clap(group = "input")]
    input_file: Option<String>,

    /// Output file (defaults to stdout if unspecified)
    #[clap(group = "output")]
    output_file: Option<String>,
}

fn report(use_stderr: bool, file_name: &str, input_len: usize, output_len: usize) {
    let ratio = 100.0 - (100.0 * output_len as f32) / input_len as f32;
    let msg = format!(
        "{} {:.2}% \t{} -> {} (-w {} -l {})",
        file_name,
        ratio,
        input_len,
        output_len,
        heatshrink::HEATSHRINK_WINDOWS_BITS,
        heatshrink::HEATSHRINK_LOOKAHEAD_BITS
    );
    if use_stderr {
        eprintln!("{}", msg);
    } else {
        println!("{}", msg);
    }
}

#[inline]
fn flush_output(output_file: &mut Box<dyn Write>, buf: &[u8]) -> Result<(), io::Error> {
    output_file.write_all(buf)
}

fn encode(
    input_file: &mut Box<dyn Read>,
    output_file: &mut Box<dyn Write>,
) -> Result<(usize, usize), io::Error> {
    let mut input_buffer = [0u8; HEATSHRINK_APP_BUFFER_SIZE];
    let mut output_buffer = [0u8; HEATSHRINK_APP_BUFFER_SIZE];
    let mut total_input_byte_size = 0;
    let mut total_output_byte_size = 0;

    let mut enc: heatshrink::encoder::HeatshrinkEncoder = Default::default();

    loop {
        let input_bytes_read = input_file.read(&mut input_buffer)?;
        total_input_byte_size += input_bytes_read;

        let mut input_bytes_processed = 0;

        while input_bytes_processed < input_bytes_read {
            match enc.sink(&input_buffer[input_bytes_processed..input_bytes_read]) {
                heatshrink::HSsinkRes::SinkOK(n) => {
                    input_bytes_processed += n;
                }
                heatshrink::HSsinkRes::SinkFull => {}
                heatshrink::HSsinkRes::SinkErrorMisuse => {
                    eprintln!("Error in HeatshrinkEncoder::sink()");
                    return Err(io::ErrorKind::Other.into());
                }
            }

            loop {
                match enc.poll(&mut output_buffer) {
                    heatshrink::HSpollRes::PollMore(n) => {
                        flush_output(output_file, &output_buffer[..n])?;
                        total_output_byte_size += n;
                    }
                    heatshrink::HSpollRes::PollEmpty(n) => {
                        flush_output(output_file, &output_buffer[..n])?;
                        total_output_byte_size += n;
                        break;
                    }
                    heatshrink::HSpollRes::PollErrorMisuse => {
                        eprintln!("Error in HeatshrinkEncoder::poll()");
                        return Err(io::ErrorKind::Other.into());
                    }
                }
            }
        }

        // End of file => finich the compressed stream
        if input_bytes_read == 0 {
            loop {
                match enc.finish() {
                    heatshrink::HSfinishRes::FinishDone => break,
                    heatshrink::HSfinishRes::FinishMore => loop {
                        match enc.poll(&mut output_buffer) {
                            heatshrink::HSpollRes::PollMore(n) => {
                                flush_output(output_file, &output_buffer[..n])?;
                                total_output_byte_size += n;
                            }
                            heatshrink::HSpollRes::PollEmpty(n) => {
                                flush_output(output_file, &output_buffer[..n])?;
                                total_output_byte_size += n;
                                break;
                            }
                            heatshrink::HSpollRes::PollErrorMisuse => {
                                eprintln!("Error in HeatshrinkEncoder::poll()");
                                return Err(io::ErrorKind::Other.into());
                            }
                        }
                    },
                }
            }
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

    loop {
        let input_bytes_read = input_file.read(&mut input_buffer)?;
        total_input_byte_size += input_bytes_read;

        // End of file => Check everything has been processed
        if input_bytes_read == 0 {
            match dec.finish() {
                heatshrink::HSfinishRes::FinishDone => {}
                heatshrink::HSfinishRes::FinishMore => {
                    eprintln!("Decoder has unprocessed data at end of input");
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
            }
            break;
        }

        let mut input_bytes_processed = 0;

        while input_bytes_processed < input_bytes_read {
            match dec.sink(&input_buffer[input_bytes_processed..input_bytes_read]) {
                heatshrink::HSsinkRes::SinkOK(n) => {
                    input_bytes_processed += n;
                }
                heatshrink::HSsinkRes::SinkFull => {}
                heatshrink::HSsinkRes::SinkErrorMisuse => {
                    eprintln!("Error in HeatshrinkDecoder::sink()");
                    return Err(io::ErrorKind::Other.into());
                }
            }

            loop {
                match dec.poll(&mut output_buffer) {
                    heatshrink::HSpollRes::PollMore(n) => {
                        flush_output(output_file, &output_buffer[..n])?;
                        total_output_byte_size += n;
                    }
                    heatshrink::HSpollRes::PollEmpty(n) => {
                        flush_output(output_file, &output_buffer[..n])?;
                        total_output_byte_size += n;
                        break;
                    }
                    heatshrink::HSpollRes::PollErrorMisuse => {
                        eprintln!("Error in HeatshrinkDecoder::poll()");
                        return Err(io::ErrorKind::Other.into());
                    }
                }
            }
        }
    }

    Ok((total_input_byte_size, total_output_byte_size))
}

fn main() {
    let args = Cli::parse();

    if args.size != heatshrink::HEATSHRINK_WINDOWS_BITS {
        eprintln!(
            "For now only the default value [{}] is supported for window size",
            heatshrink::HEATSHRINK_WINDOWS_BITS
        );
        std::process::exit(1);
    }

    if args.bits != heatshrink::HEATSHRINK_LOOKAHEAD_BITS {
        eprintln!(
            "For now only the default value [{}] is supported for back-reference length",
            heatshrink::HEATSHRINK_LOOKAHEAD_BITS
        );
        std::process::exit(1);
    }

    let file_name = match &args.input_file {
        None => "-".to_string(),
        Some(f) => f.clone(),
    };

    let mut input_file: Box<dyn Read> = match args.input_file {
        None => Box::new(BufReader::new(io::stdin())),
        Some(ref filename) => match File::open(filename) {
            Ok(file) => Box::new(BufReader::new(file)),
            Err(err) => {
                eprintln!("Could not open file \"{}\" : {}", filename, err);
                std::process::exit(1)
            }
        },
    };

    let mut output_file: Box<dyn Write> = match args.output_file {
        None => Box::new(BufWriter::new(io::stdout())),
        Some(ref filename) => match File::create(filename) {
            Ok(file) => Box::new(BufWriter::new(file)),
            Err(err) => {
                eprintln!("Could not create file \"{}\" : {}", filename, err);
                std::process::exit(1)
            }
        },
    };

    let result = if args.encode {
        encode(&mut input_file, &mut output_file)
    } else {
        decode(&mut input_file, &mut output_file)
    };

    match result {
        Err(err) => {
            eprintln!("encode/decode operation failed : {}", err);
            std::process::exit(1)
        }
        Ok((input_size, output_size)) => {
            if args.verbose {
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
