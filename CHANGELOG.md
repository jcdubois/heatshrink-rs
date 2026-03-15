# Changelog

## [1.0.0] - 2026-03-15

This release is a significant rewrite of the 0.4.x codebase.

### Breaking Changes

- Parameters `W` and `L` are now const generics instead of being hardcoded
  to W=8, L=4. Use `DefaultEncoder` / `DefaultDecoder` to keep the previous
  behaviour.
- `sink()` now returns `Result<usize, SinkError>` instead of `HSsinkRes`.
- `poll()` now returns `Result<Poll, PollError>` instead of `HSpollRes`.
- `finish()` now returns `Finish` instead of `HSfinishRes`.
- `encode()` / `decode()` now return `Result<&[u8], CodecError>` instead of
  `Result<&[u8], HSError>`.

### New Features

- Full W/L parameter range: W ∈ [4..=15], L ∈ [3..W).
- Optional `embedded-io` adapters: `EncoderWriter`, `DecoderWriter`,
  `EncoderReader`, `DecoderReader`.
- `reset()` method on encoder and decoder for reuse without reallocation.

### Performance

- Encoder speedup (~32%) with optional search index (`heatshrink-use-index`).
- Decoder bulk-copy optimisation for non-self-referential back-references.

### Internal

- Split into a library crate (`heatshrink-lib`) and a binary crate
  (`heatshrink-bin`).
- Fuzzing targets added.

## [0.4.2] - see git history
