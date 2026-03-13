# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Development

**Prerequisites (Debian/Ubuntu):**
```bash
sudo apt-get install librtlsdr-dev libasound2-dev pkg-config
```

**Build:**
```bash
cargo build --release          # full workspace
cargo build --release -p dab-rtl  # just the binary
```

**Test:**
```bash
cargo test --all               # all crates
cargo test -p <crate>          # single crate (sdr, ofdm, fec, protocol, audio, dab-rtl)
cargo test -p <crate> <name>   # single test by name, e.g.: cargo test -p dab-rtl known_channels_resolve
cargo test -p <crate> -- --nocapture  # with stdout
```

**Lint & Format:**
```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
```

Debug logging: `RUST_LOG=debug dab-rtl ...`

## Architecture

Cargo workspace with 6 crates. Signal pipeline (left to right):

```
RTL-SDR IQ → [sdr] → Complex32 samples
           → [ofdm] → OfdmFrame (soft bits, 75 symbols)
                    ├─ symbols 0–2 (FIC) → [fec] Viterbi → [protocol] FibParser → ensemble/service metadata
                    └─ symbols 3–74 (MSC) → [fec] depuncture+Viterbi → [protocol] MscHandler → MP2 bytes
                                                                      → [audio] symphonia Mp2Decoder → cpal output
```

**Crate responsibilities:**
- `crates/sdr` — RTL-SDR acquisition via `rtlsdr_mt`, IQ→Complex32 conversion, mpsc channel streaming
- `crates/ofdm` — DAB Mode I OFDM: null symbol frame sync, FFT (2048 pt), π/4-DQPSK differential product, frequency deinterleaver. Key type: `OfdmProcessor` (returns `OfdmFrame`)
- `crates/fec` — Soft-decision Viterbi (K=7, rate 1/4, 64 states) + 24 EEP/UEP depuncturing vectors
- `crates/protocol` — FIC/FIB/FIG parsing (`FicHandler`), ensemble/service/subchannel types, MSC scheduling (`MscHandler`)
- `crates/audio` — cpal `AudioOutput` (ALSA default), symphonia `Mp2Decoder` for DAB audio
- `crates/app` — Binary entry point. `pipeline.rs` wires the full threaded pipeline; `tui.rs` is the ratatui TUI; `countries.rs` maps country codes to Band III channels; `main.rs` has the Band III channel→frequency table (5A–13F)

**Threading model:** `pipeline.rs` runs SDR→OFDM→FIC→MSC→audio in a background thread. `PipelineHandle` exposes `update_rx` (events from pipeline) and `cmd_tx` (Play/Stop commands) to the TUI/CLI.

## Known TODOs

1. **EEP two-region puncturing** — currently single-vector approximation; needs ETSI EN 300 401 Table 8a/8b
2. **DAB+ HE-AAC** — requires `fdk-aac` (FFI C dep, unavoidable for DAB+)
3. **DLS** (Programme Associated Data / scrolling text)
4. **Scan caching** — persist results to `~/.config/dab-rtl/`

## DAB Mode I Constants (ofdm/src/params.rs)

- FFT size: 2048, guard interval: 504, symbol size: 2552 samples
- Null symbol: 2656 samples (frame boundary marker)
- 75 data symbols per frame: 0–2 → FIC, 3–74 → MSC (4 CIFs × 18 symbols)
- 1536 active carriers per symbol
