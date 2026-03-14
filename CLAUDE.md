# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Development

**Prerequisites (Debian/Ubuntu):**
```bash
# Remove any apt-installed Rust packages first
sudo apt-get remove rustc cargo rust-all

# Install Rust via rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# Native libraries
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
cargo test --ignored # Run ignored tests - run this when there have been fundamental changes to OFDM, fec and protocol
```

**Lint & Format:**
```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
```

Debug logging: `RUST_LOG=debug dab-rtl ...`

## When changing code

Always supply:
- unit tests if relevant

Always run:
- lint and format (update code if needed)
- tests (all tests must be successful )


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

## Dependency management and security

See [DEPENDENCIES.md](./DEPENDENCIES.md) for the full evaluation checklist and
a table of all current dependencies.

**Quick security checks (run before adding or upgrading any dependency):**
```bash
cargo audit                  # known CVEs via RustSec
cargo deny check             # license, ban, and source policy (deny.toml)
cargo outdated --workspace   # show stale versions
cargo machete                # detect unused dependencies
```

When adding a new dependency, work through every item in the checklist in
`DEPENDENCIES.md` before opening a PR. The CI `security` workflow enforces
`cargo audit` and `cargo deny check` on every push and weekly on a schedule.

## Known TODOs

1. **EEP two-region puncturing** — currently single-vector approximation; needs ETSI EN 300 401 Table 8a/8b
2. **DAB+ HE-AAC** — requires `fdk-aac` (FFI C dep, unavoidable for DAB+)
3. **DLS** (Programme Associated Data / scrolling text)
4. **Scan caching** — persist results to `~/.config/dab-rtl/`

## Soft bit layout (split, not interleaved)

The OFDM demodulator produces soft bits in **split layout**: `[Re(0)..Re(1535), Im(0)..Im(1535)]` per symbol (3072 values). The Re and Im halves are frequency-deinterleaved separately and then concatenated.

This differs from the ETSI EN 300 401 §14.4 interleaved mapping `[Im(0), Re(0), Im(1), Re(1), ...]`. The split layout is empirically verified to produce valid FIB CRCs (tested against real IQ captures). Interleaved layout produces 0% CRC pass rate. Do **not** change to interleaved without re-running `cargo test -p ofdm --test iq_pipeline -- --nocapture` to confirm FIB CRCs still pass.

The FIC accumulator collects soft bits from 3 symbols (9216 total) and cuts 2304-bit blocks for depuncturing → Viterbi. The MSC similarly flattens 18 symbols per CIF (55296 bits) and extracts subchannel ranges.

## DAB Mode I Constants (ofdm/src/params.rs)

- FFT size: 2048, guard interval: 504, symbol size: 2552 samples
- Null symbol: 2656 samples (frame boundary marker)
- 75 data symbols per frame: 0–2 → FIC, 3–74 → MSC (4 CIFs × 18 symbols)
- 1536 active carriers per symbol
