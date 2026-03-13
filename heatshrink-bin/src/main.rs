use clap::Parser;
use std::fs::File;
use std::io;
use std::io::{BufReader, BufWriter};
use std::io::{Read, Write};

use heatshrink::decoder::HeatshrinkDecoder;
use heatshrink::encoder::HeatshrinkEncoder;

const HEATSHRINK_APP_BUFFER_SIZE: usize = 64 * 1024;

#[derive(Parser)]
#[clap(author, version, about, long_about = None)]
struct Cli {
    #[clap(
        short = 'e',
        long = "encode",
        help = "Compress data (default if neither -e nor -d is given)"
    )]
    encode: bool,

    #[clap(short = 'd', long = "decode", help = "Decompress data")]
    decode: bool,

    #[clap(
        short = 'v',
        long = "verbose",
        help = "Print input & output sizes, compression ratio, etc"
    )]
    verbose: bool,

    #[clap(short = 'w', long = "window",
           help = "Base-2 log of LZSS sliding window size (4–15)",
           default_value_t = heatshrink::DEFAULT_WINDOW_BITS)]
    size: usize,

    #[clap(short = 'l', long = "length",
           help = "Number of bits used for back-reference lengths (3–14, must be < window)",
           default_value_t = heatshrink::DEFAULT_LOOKAHEAD_BITS)]
    bits: usize,

    /// Input file (defaults to stdin if unspecified)
    #[clap(group = "input")]
    input_file: Option<String>,

    /// Output file (defaults to stdout if unspecified)
    #[clap(group = "output")]
    output_file: Option<String>,
}

fn report(
    use_stderr: bool,
    file_name: &str,
    w: usize,
    l: usize,
    input_len: usize,
    output_len: usize,
) {
    let ratio = if input_len > 0 {
        100.0 - (100.0 * output_len as f32) / input_len as f32
    } else {
        0.0
    };
    let msg = format!(
        "{} {:.2}% \t{} -> {} (-w {} -l {})",
        file_name, ratio, input_len, output_len, w, l
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

fn encode_with<const W: usize, const L: usize, const BUF: usize>(
    input_file: &mut Box<dyn Read>,
    output_file: &mut Box<dyn Write>,
) -> Result<(usize, usize), io::Error> {
    let mut input_buffer = [0u8; HEATSHRINK_APP_BUFFER_SIZE];
    let mut output_buffer = [0u8; HEATSHRINK_APP_BUFFER_SIZE];
    let mut total_input_byte_size = 0usize;
    let mut total_output_byte_size = 0usize;

    let mut enc = HeatshrinkEncoder::<W, L, BUF>::new();

    loop {
        let input_bytes_read = input_file.read(&mut input_buffer)?;
        total_input_byte_size += input_bytes_read;

        let mut input_bytes_processed = 0;
        while input_bytes_processed < input_bytes_read {
            match enc.sink(&input_buffer[input_bytes_processed..input_bytes_read]) {
                Ok(n) => input_bytes_processed += n,
                Err(heatshrink::SinkError::Full) => {}
                Err(heatshrink::SinkError::Misuse) => {
                    eprintln!("Error in HeatshrinkEncoder::sink()");
                    return Err(io::ErrorKind::Other.into());
                }
            }
            loop {
                match enc.poll(&mut output_buffer) {
                    Ok(heatshrink::Poll::More(n)) => {
                        flush_output(output_file, &output_buffer[..n])?;
                        total_output_byte_size += n;
                    }
                    Ok(heatshrink::Poll::Empty(n)) => {
                        flush_output(output_file, &output_buffer[..n])?;
                        total_output_byte_size += n;
                        break;
                    }
                    Err(_) => {
                        eprintln!("Error in HeatshrinkEncoder::poll()");
                        return Err(io::ErrorKind::Other.into());
                    }
                }
            }
        }

        if input_bytes_read == 0 {
            loop {
                match enc.finish() {
                    heatshrink::Finish::Done => break,
                    heatshrink::Finish::More => loop {
                        match enc.poll(&mut output_buffer) {
                            Ok(heatshrink::Poll::More(n)) => {
                                flush_output(output_file, &output_buffer[..n])?;
                                total_output_byte_size += n;
                            }
                            Ok(heatshrink::Poll::Empty(n)) => {
                                flush_output(output_file, &output_buffer[..n])?;
                                total_output_byte_size += n;
                                break;
                            }
                            Err(_) => {
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

fn decode_with<const W: usize, const L: usize, const I: usize, const WIN: usize>(
    input_file: &mut Box<dyn Read>,
    output_file: &mut Box<dyn Write>,
) -> Result<(usize, usize), io::Error> {
    let mut input_buffer = [0u8; HEATSHRINK_APP_BUFFER_SIZE];
    let mut output_buffer = [0u8; HEATSHRINK_APP_BUFFER_SIZE];
    let mut total_input_byte_size = 0usize;
    let mut total_output_byte_size = 0usize;

    let mut dec = HeatshrinkDecoder::<W, L, I, WIN>::new();

    loop {
        let input_bytes_read = input_file.read(&mut input_buffer)?;
        total_input_byte_size += input_bytes_read;

        if input_bytes_read == 0 {
            match dec.finish() {
                heatshrink::Finish::Done => {}
                heatshrink::Finish::More => {
                    eprintln!("Decoder has uninput_bytes_processed data at end of input");
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
            }
            break;
        }

        let mut input_bytes_processed = 0;
        while input_bytes_processed < input_bytes_read {
            match dec.sink(&input_buffer[input_bytes_processed..input_bytes_read]) {
                Ok(n) => input_bytes_processed += n,
                Err(heatshrink::SinkError::Full) => {}
                Err(heatshrink::SinkError::Misuse) => {
                    eprintln!("Error in HeatshrinkDecoder::sink()");
                    return Err(io::ErrorKind::Other.into());
                }
            }
            loop {
                match dec.poll(&mut output_buffer) {
                    Ok(heatshrink::Poll::More(n)) => {
                        flush_output(output_file, &output_buffer[..n])?;
                        total_output_byte_size += n;
                    }
                    Ok(heatshrink::Poll::Empty(n)) => {
                        flush_output(output_file, &output_buffer[..n])?;
                        total_output_byte_size += n;
                        break;
                    }
                    Err(_) => {
                        eprintln!("Error in HeatshrinkDecoder::poll()");
                        return Err(io::ErrorKind::Other.into());
                    }
                }
            }
        }
    }
    Ok((total_input_byte_size, total_output_byte_size))
}

// Dispatch tables.
// BUF = 2 << W, WIN = 1 << W.
// I (decoder input buffer) is fixed at 32 bytes for all configurations;
// it can be changed independently if needed.

macro_rules! dispatch_encode {
    ($w:expr, $l:expr, $input:expr, $output:expr,
     $(($wv:literal, $lv:literal, $buf:literal)),+ $(,)?) => {
        match ($w, $l) {
            $(($wv, $lv) => encode_with::<$wv, $lv, $buf>($input, $output),)+
            _ => {
                eprintln!(
                    "Unsupported -w {} -l {} (valid range: 4<=W<=15, 1<=L<W, and the \
                     combination must be in the supported list)",
                    $w, $l
                );
                Err(io::ErrorKind::InvalidInput.into())
            }
        }
    }
}

macro_rules! dispatch_decode {
    ($w:expr, $l:expr, $input:expr, $output:expr,
     $(($wv:literal, $lv:literal, $win:literal)),+ $(,)?) => {
        match ($w, $l) {
            $(($wv, $lv) => decode_with::<$wv, $lv, 32, $win>($input, $output),)+
            _ => {
                eprintln!(
                    "Unsupported -w {} -l {} (valid range: 4<=W<=15, 1<=L<W, and the \
                     combination must be in the supported list)",
                    $w, $l
                );
                Err(io::ErrorKind::InvalidInput.into())
            }
        }
    }
}

fn dispatch_encode(
    w: usize,
    l: usize,
    input_file: &mut Box<dyn Read>,
    output_file: &mut Box<dyn Write>,
) -> Result<(usize, usize), io::Error> {
    // BUF = 2 << W — all valid combinations W [4..15], L [3..W-1)
    dispatch_encode!(
        w,
        l,
        input_file,
        output_file,
        (4, 3, 32),
        (5, 3, 64),
        (5, 4, 64),
        (6, 3, 128),
        (6, 4, 128),
        (6, 5, 128),
        (7, 3, 256),
        (7, 4, 256),
        (7, 5, 256),
        (7, 6, 256),
        (8, 3, 512),
        (8, 4, 512),
        (8, 5, 512),
        (8, 6, 512),
        (8, 7, 512),
        (9, 3, 1024),
        (9, 4, 1024),
        (9, 5, 1024),
        (9, 6, 1024),
        (9, 7, 1024),
        (9, 8, 1024),
        (10, 3, 2048),
        (10, 4, 2048),
        (10, 5, 2048),
        (10, 6, 2048),
        (10, 7, 2048),
        (10, 8, 2048),
        (10, 9, 2048),
        (11, 3, 4096),
        (11, 4, 4096),
        (11, 5, 4096),
        (11, 6, 4096),
        (11, 7, 4096),
        (11, 8, 4096),
        (11, 9, 4096),
        (11, 10, 4096),
        (12, 3, 8192),
        (12, 4, 8192),
        (12, 5, 8192),
        (12, 6, 8192),
        (12, 7, 8192),
        (12, 8, 8192),
        (12, 9, 8192),
        (12, 10, 8192),
        (12, 11, 8192),
        (13, 3, 16384),
        (13, 4, 16384),
        (13, 5, 16384),
        (13, 6, 16384),
        (13, 7, 16384),
        (13, 8, 16384),
        (13, 9, 16384),
        (13, 10, 16384),
        (13, 11, 16384),
        (13, 12, 16384),
        (14, 3, 32768),
        (14, 4, 32768),
        (14, 5, 32768),
        (14, 6, 32768),
        (14, 7, 32768),
        (14, 8, 32768),
        (14, 9, 32768),
        (14, 10, 32768),
        (14, 11, 32768),
        (14, 12, 32768),
        (14, 13, 32768),
        (15, 3, 65536),
        (15, 4, 65536),
        (15, 5, 65536),
        (15, 6, 65536),
        (15, 7, 65536),
        (15, 8, 65536),
        (15, 9, 65536),
        (15, 10, 65536),
        (15, 11, 65536),
        (15, 12, 65536),
        (15, 13, 65536),
        (15, 14, 65536),
    )
}

fn dispatch_decode(
    w: usize,
    l: usize,
    input_file: &mut Box<dyn Read>,
    output_file: &mut Box<dyn Write>,
) -> Result<(usize, usize), io::Error> {
    // WIN = 1 << W, I fixed at 32 bytes — all valid combinations W [4..15], L [3..W-1)
    dispatch_decode!(
        w,
        l,
        input_file,
        output_file,
        (4, 3, 16),
        (5, 3, 32),
        (5, 4, 32),
        (6, 3, 64),
        (6, 4, 64),
        (6, 5, 64),
        (7, 3, 128),
        (7, 4, 128),
        (7, 5, 128),
        (7, 6, 128),
        (8, 3, 256),
        (8, 4, 256),
        (8, 5, 256),
        (8, 6, 256),
        (8, 7, 256),
        (9, 3, 512),
        (9, 4, 512),
        (9, 5, 512),
        (9, 6, 512),
        (9, 7, 512),
        (9, 8, 512),
        (10, 3, 1024),
        (10, 4, 1024),
        (10, 5, 1024),
        (10, 6, 1024),
        (10, 7, 1024),
        (10, 8, 1024),
        (10, 9, 1024),
        (11, 3, 2048),
        (11, 4, 2048),
        (11, 5, 2048),
        (11, 6, 2048),
        (11, 7, 2048),
        (11, 8, 2048),
        (11, 9, 2048),
        (11, 10, 2048),
        (12, 3, 4096),
        (12, 4, 4096),
        (12, 5, 4096),
        (12, 6, 4096),
        (12, 7, 4096),
        (12, 8, 4096),
        (12, 9, 4096),
        (12, 10, 4096),
        (12, 11, 4096),
        (13, 3, 8192),
        (13, 4, 8192),
        (13, 5, 8192),
        (13, 6, 8192),
        (13, 7, 8192),
        (13, 8, 8192),
        (13, 9, 8192),
        (13, 10, 8192),
        (13, 11, 8192),
        (13, 12, 8192),
        (14, 3, 16384),
        (14, 4, 16384),
        (14, 5, 16384),
        (14, 6, 16384),
        (14, 7, 16384),
        (14, 8, 16384),
        (14, 9, 16384),
        (14, 10, 16384),
        (14, 11, 16384),
        (14, 12, 16384),
        (14, 13, 16384),
        (15, 3, 32768),
        (15, 4, 32768),
        (15, 5, 32768),
        (15, 6, 32768),
        (15, 7, 32768),
        (15, 8, 32768),
        (15, 9, 32768),
        (15, 10, 32768),
        (15, 11, 32768),
        (15, 12, 32768),
        (15, 13, 32768),
        (15, 14, 32768),
    )
}

fn main() {
    let args = Cli::parse();

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

    // Default to encode if neither -e nor -d is specified.
    // Explicit -d takes priority; -e and no flag both mean encode.
    let result = if args.decode && !args.encode {
        dispatch_decode(args.size, args.bits, &mut input_file, &mut output_file)
    } else {
        dispatch_encode(args.size, args.bits, &mut input_file, &mut output_file)
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
                    args.size,
                    args.bits,
                    input_size,
                    output_size,
                );
            }
        }
    }
}
