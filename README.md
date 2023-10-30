# heatshrink_embedded
Minimal no_std implementation of Heatshrink compression &amp; decompression
for embedded systems

This library is a port to RUST of the original heatshrink C library available
at https://github.com/atomicobject/heatshrink.

The port is limited to the "static" version of the C library which means
heatshrink parameters are hardcoded to window_sz2 = 8 and lookahead_sz2 = 4.

## Key Features:

- **Low memory usage:**
    It is useful for many general cases with small memory.
- **Incremental, bounded CPU use:**
    You can chew on input data in arbitrarily tiny bites.
    This is a useful property in hard real-time environments.
- **For now you are limited to the static version because of no_std:**
    No dynamic allocation is used.
- **ISC license:**
    You can use it freely, even for commercial purposes.

## Getting Started:

### Basic Usage

1. Allocate a heatshrink encoder or heatshrink decoder state machine using
either `HeatshrinkEncoder::new` or `HeatshrinkDecoder::new`. You can also
reset an existing state machine by calling the `reset` function on the state
machine.

2. Use `sink` to sink an input buffer into the state machine. In the
returned result you get a CR code and the amount of bytes that were actually
consumed (If 0 bytes were conusmed, the buffer is full.).

3. Use `poll` to move output from the state machine into an output
buffer. In the returned result you get a CR code and the amount bytes
that were writen to the provided buffer.

Repeat steps 2 and 3 to stream data through the state machine. Since
it's doing data compression, the input and output sizes can vary
significantly. Looping will be necessary to buffer the input and output
as the data is processed.

4. When the end of the input stream is reached, call `finish` to notify
the state machine that no more input is available. The return value from
`finish` will indicate whether any output remains. if so, call `poll` to
get more.

Continue calling `finish` and `poll`ing to flush remaining output until
`finish` indicates that the output has been exhausted.

Sinking more data after `finish` has been called will not work without
calling `reset` on the state machine.

## Configuration

No configuration is needed (for now) on this RUST implementation as
parameters are not user defined (they are hardcoded).

On the cargo build command you can choose to enable the lookup table to
speed up the compression phase by selecting --features "heatshrink-use-index"
on the cargo command line.

## More Information and Benchmarks:

heatshrink is based on [LZSS], since it's particularly suitable for
compression in small amounts of memory. It can use an optional, small
[index] to make compression significantly faster, but otherwise can run
in under 100 bytes of memory. The index currently adds 2^(window size+1)
bytes to memory usage for compression, and temporarily allocates 512
bytes on the stack during index construction (if the index is enabled).

For more information, see the [blog post] for an overview.

[blog post]: http://spin.atomicobject.com/2013/03/14/heatshrink-embedded-data-compression/
[index]: http://spin.atomicobject.com/2014/01/13/lightweight-indexing-for-embedded-systems/
[LZSS]: http://en.wikipedia.org/wiki/Lempel-Ziv-Storer-Szymanski
