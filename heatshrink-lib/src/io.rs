//! [`embedded_io`] adapters for the Heatshrink encoder and decoder.
//!
//! Enabled by the `embedded-io` feature flag.  All types are `no_std`
//! compatible; no allocator is required.
//!
//! # Available adapters
//!
//! ## Push (Write) adapters
//!
//! These implement [`embedded_io::Write`]: the caller pushes bytes in, and the
//! adapter forwards the transformed bytes to an inner `Write` sink.
//!
//! | Type | Input | Output |
//! |------|-------|--------|
//! | [`EncoderWriter`] | raw bytes | compressed bytes → inner writer |
//! | [`DecoderWriter`] | compressed bytes | decompressed bytes → inner writer |
//!
//! Call [`EncoderWriter::finish`] / [`DecoderWriter::finish`] when done to
//! flush the encoder's bit-buffer and write the final bytes.
//!
//! ## Pull (Read) adapters
//!
//! These implement [`embedded_io::Read`]: the caller pulls transformed bytes
//! out; the adapter consumes raw bytes from an inner `Read` source.
//!
//! | Type | Source | Output |
//! |------|--------|--------|
//! | [`DecoderReader`] | compressed bytes (inner reader) | decompressed bytes |
//! | [`EncoderReader`] | raw bytes (inner reader) | compressed bytes |
//!
//! # Buffer sizing (`OUT`)
//!
//! The `Read` adapters need an internal output buffer to hold bytes produced
//! by `poll()` that did not fit in the caller's buffer on the previous call.
//! Its size is the const generic `OUT` (default: `256`).  A larger value
//! reduces the number of `poll()` calls; a smaller value saves stack space.
//! `OUT` must be ≥ 1.
//!
//! # Example — compress to a `Vec`-like sink
//!
//! ```rust,no_run
//! # #[cfg(feature = "embedded-io")] {
//! use heatshrink::io::EncoderWriter;
//! use heatshrink::DefaultEncoder;
//! use embedded_io::Write as _;
//!
//! // Any type that implements embedded_io::Write can be used as the sink.
//! // Here we use a fixed-size array wrapped in a cursor-like struct.
//! let mut compressed = [0u8; 1024];
//! let mut cursor = heatshrink::io::SliceSink::new(&mut compressed);
//!
//! let mut enc: EncoderWriter<_, DefaultEncoder> = EncoderWriter::new(&mut cursor);
//! enc.write_all(b"hello heatshrink").unwrap();
//! let n = enc.finish().unwrap();
//! // `n` bytes of compressed data are now in `compressed[..n]`.
//! # }
//! ```

use crate::{Finish, Poll, SinkError};
use embedded_io::{Error, ErrorKind, ErrorType, Read, Write};

// ─── EncoderWriter ────────────────────────────────────────────────────────────

/// A [`Write`] adapter that compresses bytes with a Heatshrink encoder and
/// forwards the compressed output to an inner [`Write`] sink.
///
/// # Type parameters
///
/// - `W` — the inner sink (must implement [`embedded_io::Write`]).
/// - `ENC` — the encoder type (e.g. [`crate::DefaultEncoder`]).
///
/// # Usage
///
/// 1. Create with [`EncoderWriter::new`].
/// 2. Write raw data with [`embedded_io::Write::write`] /
///    [`embedded_io::Write::write_all`].
/// 3. **Call [`finish`](EncoderWriter::finish)** to flush the encoder's
///    internal bit-buffer and write the final compressed bytes.  Dropping
///    without calling `finish` will silently lose the tail of the stream.
pub struct EncoderWriter<W, ENC> {
    inner: W,
    enc: ENC,
    /// Scratch buffer used for a single `poll()` call.
    poll_buf: [u8; 256],
}

impl<W, ENC> EncoderWriter<W, ENC>
where
    ENC: Default,
{
    /// Create a new `EncoderWriter` that compresses into `inner`.
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            enc: ENC::default(),
            poll_buf: [0u8; 256],
        }
    }
}

impl<W, ENC> ErrorType for EncoderWriter<W, ENC>
where
    W: ErrorType,
{
    type Error = W::Error;
}

impl<W, ENC> EncoderWriter<W, ENC>
where
    W: Write,
    ENC: EncoderOps,
{
    /// Drain all pending compressed output from the encoder into `inner`.
    fn drain_poll(&mut self) -> Result<(), W::Error> {
        loop {
            match self.enc.poll(&mut self.poll_buf) {
                Ok(Poll::More(n)) => self.inner.write_all(&self.poll_buf[..n])?,
                Ok(Poll::Empty(n)) => {
                    if n > 0 {
                        self.inner.write_all(&self.poll_buf[..n])?;
                    }
                    return Ok(());
                }
                Err(_) => return Ok(()), // empty poll_buf: cannot happen (len=256)
            }
        }
    }

    /// Signal end-of-stream: flush the encoder's bit-buffer and write all
    /// remaining compressed bytes to the inner sink.
    ///
    /// Returns `Ok(())` on success.  Must be called exactly once after all
    /// input has been written.
    pub fn finish(&mut self) -> Result<(), W::Error> {
        loop {
            match self.enc.finish() {
                Finish::Done => return self.inner.flush(),
                Finish::More => self.drain_poll()?,
            }
        }
    }
}

impl<W, ENC> Write for EncoderWriter<W, ENC>
where
    W: Write,
    ENC: EncoderOps,
{
    fn write(&mut self, buf: &[u8]) -> Result<usize, W::Error> {
        let mut consumed = 0;
        while consumed < buf.len() {
            match self.enc.sink(&buf[consumed..]) {
                Ok(n) => consumed += n,
                Err(SinkError::Full) => self.drain_poll()?,
                Err(SinkError::Misuse) => {
                    // finish() already called — treat as a no-op write of 0.
                    break;
                }
            }
        }
        // Opportunistically drain so the inner buffer doesn't fill up.
        self.drain_poll()?;
        Ok(consumed)
    }

    fn flush(&mut self) -> Result<(), W::Error> {
        self.inner.flush()
    }
}

// ─── DecoderWriter ────────────────────────────────────────────────────────────

/// A [`Write`] adapter that decompresses Heatshrink-compressed bytes and
/// forwards the decompressed output to an inner [`Write`] sink.
///
/// # Usage
///
/// 1. Create with [`DecoderWriter::new`].
/// 2. Write compressed data with [`embedded_io::Write::write`] /
///    [`embedded_io::Write::write_all`].
/// 3. **Call [`finish`](DecoderWriter::finish)** to assert that the decoder
///    has no pending input and flush the inner sink.
pub struct DecoderWriter<W, DEC> {
    inner: W,
    dec: DEC,
    poll_buf: [u8; 256],
}

impl<W, DEC> DecoderWriter<W, DEC>
where
    DEC: Default,
{
    /// Create a new `DecoderWriter` that decompresses into `inner`.
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            dec: DEC::default(),
            poll_buf: [0u8; 256],
        }
    }
}

impl<W, DEC> ErrorType for DecoderWriter<W, DEC>
where
    W: ErrorType,
{
    type Error = W::Error;
}

impl<W, DEC> DecoderWriter<W, DEC>
where
    W: Write,
    DEC: DecoderOps,
{
    fn drain_poll(&mut self) -> Result<(), W::Error> {
        loop {
            match self.dec.poll(&mut self.poll_buf) {
                Ok(Poll::More(n)) => self.inner.write_all(&self.poll_buf[..n])?,
                Ok(Poll::Empty(n)) => {
                    if n > 0 {
                        self.inner.write_all(&self.poll_buf[..n])?;
                    }
                    return Ok(());
                }
                Err(_) => return Ok(()),
            }
        }
    }

    /// Assert end-of-stream and flush the inner sink.
    ///
    /// Decompression is stateless with respect to trailing bytes — calling
    /// `finish` simply ensures all decoded output has been forwarded and the
    /// inner sink is flushed.
    pub fn finish(&mut self) -> Result<(), W::Error> {
        self.drain_poll()?;
        self.inner.flush()
    }
}

impl<W, DEC> Write for DecoderWriter<W, DEC>
where
    W: Write,
    DEC: DecoderOps,
{
    fn write(&mut self, buf: &[u8]) -> Result<usize, W::Error> {
        let mut consumed = 0;
        while consumed < buf.len() {
            match self.dec.sink(&buf[consumed..]) {
                Ok(n) => consumed += n,
                Err(SinkError::Full) => self.drain_poll()?,
                Err(SinkError::Misuse) => break,
            }
        }
        self.drain_poll()?;
        Ok(consumed)
    }

    fn flush(&mut self) -> Result<(), W::Error> {
        self.inner.flush()
    }
}

// ─── DecoderReader ────────────────────────────────────────────────────────────

/// A [`Read`] adapter that decompresses Heatshrink-compressed bytes on the
/// fly.  The caller reads decompressed bytes; the adapter fetches compressed
/// bytes from an inner [`Read`] source as needed.
///
/// # Type parameters
///
/// - `R`   — the inner source (must implement [`embedded_io::Read`]).
/// - `DEC` — the decoder type (e.g. [`crate::DefaultDecoder`]).
/// - `OUT` — size of the internal output buffer (default `256`).  Must be ≥ 1.
pub struct DecoderReader<R, DEC, const OUT: usize = 256> {
    inner: R,
    dec: DEC,
    /// Decompressed bytes waiting to be handed to the caller.
    out_buf: [u8; OUT],
    /// Slice of `out_buf` that is filled but not yet consumed: `[out_start..out_end]`.
    out_start: usize,
    out_end: usize,
    /// Compressed bytes read from `inner` but not yet sunk into the decoder.
    in_buf: [u8; 64],
    in_start: usize,
    in_end: usize,
    /// Set when the inner reader returned 0 bytes (EOF).
    eof: bool,
}

impl<R, DEC, const OUT: usize> DecoderReader<R, DEC, OUT>
where
    DEC: Default,
{
    /// Create a new `DecoderReader` wrapping a compressed `inner` source.
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            dec: DEC::default(),
            out_buf: [0u8; OUT],
            out_start: 0,
            out_end: 0,
            in_buf: [0u8; 64],
            in_start: 0,
            in_end: 0,
            eof: false,
        }
    }
}

impl<R, DEC, const OUT: usize> ErrorType for DecoderReader<R, DEC, OUT>
where
    R: ErrorType,
{
    type Error = R::Error;
}

impl<R, DEC, const OUT: usize> Read for DecoderReader<R, DEC, OUT>
where
    R: Read,
    DEC: DecoderOps,
{
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, R::Error> {
        // 1. Serve leftover decoded output from a previous oversized poll.
        if self.out_start < self.out_end {
            let n = (self.out_end - self.out_start).min(buf.len());
            buf[..n].copy_from_slice(&self.out_buf[self.out_start..self.out_start + n]);
            self.out_start += n;
            if self.out_start == self.out_end {
                self.out_start = 0;
                self.out_end = 0;
            }
            return Ok(n);
        }

        loop {
            // 2. Sink any leftover compressed bytes from a previous read that
            //    was interrupted by SinkError::Full.
            while self.in_start < self.in_end {
                match self.dec.sink(&self.in_buf[self.in_start..self.in_end]) {
                    Ok(n) => self.in_start += n,
                    Err(SinkError::Full) => break, // will poll first, then retry
                    Err(SinkError::Misuse) => return Ok(0),
                }
            }
            if self.in_start == self.in_end {
                self.in_start = 0;
                self.in_end = 0;
            }

            // 3. Poll decoded output.
            match self.dec.poll(&mut self.out_buf) {
                Ok(Poll::More(n)) | Ok(Poll::Empty(n)) if n > 0 => {
                    let give = n.min(buf.len());
                    buf[..give].copy_from_slice(&self.out_buf[..give]);
                    if n > give {
                        self.out_start = give;
                        self.out_end = n;
                    }
                    return Ok(give);
                }
                Ok(Poll::More(_)) => {
                    // Decoder needs more input — fall through to fetch from inner.
                }
                Ok(Poll::Empty(_)) => {
                    // Decoder drained.
                    if self.eof && self.in_start == self.in_end {
                        return Ok(0); // truly done
                    }
                    // Still have buffered input or inner not exhausted yet.
                }
                Err(_) => return Ok(0), // out_buf.len() > 0, cannot happen
            }

            // 4. If we still have un-sunk input buffered, loop back to sink it.
            if self.in_start < self.in_end {
                continue;
            }

            // 5. Read more compressed bytes from inner into in_buf.
            if self.eof {
                continue; // drain decoder until it signals Empty with 0
            }
            let n_read = self.inner.read(&mut self.in_buf)?;
            if n_read == 0 {
                self.eof = true;
                // Don't return yet — the decoder may still have output to give.
                continue;
            }
            self.in_start = 0;
            self.in_end = n_read;
        }
    }
}

// ─── EncoderReader ────────────────────────────────────────────────────────────

/// A [`Read`] adapter that compresses bytes on the fly.  The caller reads
/// compressed bytes; the adapter fetches raw bytes from an inner [`Read`]
/// source and compresses them.
///
/// # Type parameters
///
/// - `R`   — the inner source (must implement [`embedded_io::Read`]).
/// - `ENC` — the encoder type (e.g. [`crate::DefaultEncoder`]).
/// - `OUT` — size of the internal output buffer (default `256`).  Must be ≥ 1.
///
/// # End of stream
///
/// When the inner reader returns 0 bytes (EOF), the encoder's `finish()` is
/// called automatically.  The adapter will return 0 only after all compressed
/// output has been drained.
pub struct EncoderReader<R, ENC, const OUT: usize = 256> {
    inner: R,
    enc: ENC,
    out_buf: [u8; OUT],
    out_start: usize,
    out_end: usize,
    /// Raw bytes read from `inner` but not yet sunk into the encoder.
    in_buf: [u8; 64],
    in_start: usize,
    in_end: usize,
    /// Inner reader reached EOF.
    eof: bool,
    /// `finish()` has been called on the encoder.
    finishing: bool,
}

impl<R, ENC, const OUT: usize> EncoderReader<R, ENC, OUT>
where
    ENC: Default,
{
    /// Create a new `EncoderReader` wrapping a raw `inner` source.
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            enc: ENC::default(),
            out_buf: [0u8; OUT],
            out_start: 0,
            out_end: 0,
            in_buf: [0u8; 64],
            in_start: 0,
            in_end: 0,
            eof: false,
            finishing: false,
        }
    }
}

impl<R, ENC, const OUT: usize> ErrorType for EncoderReader<R, ENC, OUT>
where
    R: ErrorType,
{
    type Error = R::Error;
}

impl<R, ENC, const OUT: usize> Read for EncoderReader<R, ENC, OUT>
where
    R: Read,
    ENC: EncoderOps,
{
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, R::Error> {
        // 1. Serve leftover compressed output from a previous oversized poll.
        if self.out_start < self.out_end {
            let n = (self.out_end - self.out_start).min(buf.len());
            buf[..n].copy_from_slice(&self.out_buf[self.out_start..self.out_start + n]);
            self.out_start += n;
            if self.out_start == self.out_end {
                self.out_start = 0;
                self.out_end = 0;
            }
            return Ok(n);
        }

        loop {
            // 2. Sink any leftover raw bytes from a previous read interrupted
            //    by SinkError::Full.
            while self.in_start < self.in_end {
                match self.enc.sink(&self.in_buf[self.in_start..self.in_end]) {
                    Ok(n) => self.in_start += n,
                    Err(SinkError::Full) => break,
                    Err(SinkError::Misuse) => return Ok(0),
                }
            }
            if self.in_start == self.in_end {
                self.in_start = 0;
                self.in_end = 0;
            }

            // 3. Poll the encoder for compressed output.
            match self.enc.poll(&mut self.out_buf) {
                Ok(Poll::More(n)) | Ok(Poll::Empty(n)) if n > 0 => {
                    let give = n.min(buf.len());
                    buf[..give].copy_from_slice(&self.out_buf[..give]);
                    if n > give {
                        self.out_start = give;
                        self.out_end = n;
                    }
                    return Ok(give);
                }
                Ok(Poll::More(_)) => {
                    // Encoder needs more input — fall through to fetch from inner.
                }
                Ok(Poll::Empty(_)) => {
                    if self.finishing {
                        match self.enc.finish() {
                            Finish::Done => return Ok(0),
                            Finish::More => continue,
                        }
                    }
                    if self.eof && self.in_start == self.in_end {
                        // All input consumed; begin the finish sequence.
                        self.finishing = true;
                        continue;
                    }
                }
                Err(_) => return Ok(0),
            }

            // 4. Loop back to sink buffered input if any remains.
            if self.in_start < self.in_end {
                continue;
            }

            // 5. Fetch more raw bytes from inner.
            if self.eof || self.finishing {
                continue;
            }
            let n_read = self.inner.read(&mut self.in_buf)?;
            if n_read == 0 {
                self.eof = true;
                continue;
            }
            self.in_start = 0;
            self.in_end = n_read;
        }
    }
}

// ─── Sealed trait helpers ────────────────────────────────────────────────────
//
// `EncoderOps` and `DecoderOps` are `pub` so they can appear in the bounds of
// the `pub` adapter structs without triggering `private_bounds`.  They are
// *sealed* via a private supertrait (`private::Sealed`) so that no external
// crate can implement them — only `HeatshrinkEncoder` / `HeatshrinkDecoder`
// have implementations, which live in this module.

mod private {
    pub trait Sealed {}
}

/// Abstraction over [`crate::encoder::HeatshrinkEncoder`] used by the io
/// adapters.  This trait is sealed: it cannot be implemented outside this
/// crate.
pub trait EncoderOps: private::Sealed {
    #[doc(hidden)]
    fn sink(&mut self, input: &[u8]) -> Result<usize, SinkError>;
    #[doc(hidden)]
    fn poll(&mut self, output: &mut [u8]) -> Result<Poll, crate::PollError>;
    #[doc(hidden)]
    fn finish(&mut self) -> Finish;
}

/// Abstraction over [`crate::decoder::HeatshrinkDecoder`] used by the io
/// adapters.  This trait is sealed: it cannot be implemented outside this
/// crate.
pub trait DecoderOps: private::Sealed {
    #[doc(hidden)]
    fn sink(&mut self, input: &[u8]) -> Result<usize, SinkError>;
    #[doc(hidden)]
    fn poll(&mut self, output: &mut [u8]) -> Result<Poll, crate::PollError>;
    #[doc(hidden)]
    #[allow(dead_code)]
    fn finish(&self) -> Finish;
}

impl<const W: usize, const L: usize, const BUF: usize> private::Sealed
    for crate::encoder::HeatshrinkEncoder<W, L, BUF>
{
}

impl<const W: usize, const L: usize, const BUF: usize> EncoderOps
    for crate::encoder::HeatshrinkEncoder<W, L, BUF>
{
    #[inline]
    fn sink(&mut self, input: &[u8]) -> Result<usize, SinkError> {
        crate::encoder::HeatshrinkEncoder::sink(self, input)
    }
    #[inline]
    fn poll(&mut self, output: &mut [u8]) -> Result<Poll, crate::PollError> {
        crate::encoder::HeatshrinkEncoder::poll(self, output)
    }
    #[inline]
    fn finish(&mut self) -> Finish {
        crate::encoder::HeatshrinkEncoder::finish(self)
    }
}

impl<const W: usize, const L: usize, const I: usize, const WIN: usize> private::Sealed
    for crate::decoder::HeatshrinkDecoder<W, L, I, WIN>
{
}

impl<const W: usize, const L: usize, const I: usize, const WIN: usize> DecoderOps
    for crate::decoder::HeatshrinkDecoder<W, L, I, WIN>
{
    #[inline]
    fn sink(&mut self, input: &[u8]) -> Result<usize, SinkError> {
        crate::decoder::HeatshrinkDecoder::sink(self, input)
    }
    #[inline]
    fn poll(&mut self, output: &mut [u8]) -> Result<Poll, crate::PollError> {
        crate::decoder::HeatshrinkDecoder::poll(self, output)
    }
    #[inline]
    fn finish(&self) -> Finish {
        crate::decoder::HeatshrinkDecoder::finish(self)
    }
}

// ─── SliceSink — minimal Write impl for tests / doc-examples ─────────────────

/// A simple [`Write`] sink backed by a mutable byte slice.
///
/// Useful for tests and documentation examples where no external I/O is
/// available.  `write` returns [`ErrorKind::OutOfMemory`] when the slice is
/// full.
pub struct SliceSink<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> SliceSink<'a> {
    /// Create a new `SliceSink` backed by `buf`.
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Number of bytes written so far.
    pub fn len(&self) -> usize {
        self.pos
    }

    /// Returns `true` if no bytes have been written yet.
    pub fn is_empty(&self) -> bool {
        self.pos == 0
    }

    /// The written portion of the backing slice.
    pub fn written(&self) -> &[u8] {
        &self.buf[..self.pos]
    }
}

/// Error type for [`SliceSink`].
#[derive(Debug)]
pub struct SliceSinkError(ErrorKind);

impl Error for SliceSinkError {
    fn kind(&self) -> ErrorKind {
        self.0
    }
}

impl ErrorType for SliceSink<'_> {
    type Error = SliceSinkError;
}

impl Write for SliceSink<'_> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, SliceSinkError> {
        let space = self.buf.len() - self.pos;
        if space == 0 {
            return Err(SliceSinkError(ErrorKind::OutOfMemory));
        }
        let n = buf.len().min(space);
        self.buf[self.pos..self.pos + n].copy_from_slice(&buf[..n]);
        self.pos += n;
        Ok(n)
    }

    fn flush(&mut self) -> Result<(), SliceSinkError> {
        Ok(())
    }
}

/// A simple [`Read`] source backed by a byte slice.
///
/// Useful for tests: wraps a `&[u8]` so it can be passed to
/// [`DecoderReader`] / [`EncoderReader`].
pub struct SliceSource<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> SliceSource<'a> {
    /// Create a new `SliceSource` backed by `buf`.
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
}

/// Error type for [`SliceSource`].
#[derive(Debug)]
pub struct SliceSourceError(ErrorKind);

impl Error for SliceSourceError {
    fn kind(&self) -> ErrorKind {
        self.0
    }
}

impl ErrorType for SliceSource<'_> {
    type Error = SliceSourceError;
}

impl Read for SliceSource<'_> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, SliceSourceError> {
        let remaining = self.buf.len() - self.pos;
        let n = remaining.min(buf.len());
        buf[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}
