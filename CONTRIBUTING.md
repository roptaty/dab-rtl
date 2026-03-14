# Contributing to dab-rtl

Thank you for your interest in contributing! This document covers how to get started, submit changes, and follow the project's conventions.

---

## Getting started

### Prerequisites (Debian/Ubuntu)

```bash
# Remove any apt-installed Rust packages first
sudo apt-get remove rustc cargo rust-all

# Install Rust via rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# Native libraries
sudo apt-get install librtlsdr-dev libasound2-dev pkg-config
```

### Build

```bash
cargo build --release          # full workspace
cargo build --release -p dab-rtl  # just the binary
```

---

## Development workflow

### Running tests

```bash
cargo test --all               # all crates
cargo test -p <crate>          # single crate (sdr, ofdm, fec, protocol, audio, dab-rtl)
cargo test -p <crate> <name>   # single test by name
cargo test -p <crate> -- --nocapture  # with stdout
cargo test --ignored           # run ignored tests (use after fundamental OFDM/fec/protocol changes)
```

### Lint and format

All PRs must pass these checks before merging:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
```

Auto-format your code before committing:

```bash
cargo fmt --all
```

### Debug logging

```bash
RUST_LOG=debug dab-rtl ...
```

---

## Submitting changes

1. Fork the repository and create a branch from `main`.
2. Make your changes. Add unit tests for any new logic.
3. Run lint and format checks — fix any issues.
4. Run the full test suite — all tests must pass.
5. Open a pull request against `main`. Describe what the change does and why.

### Commit messages

Use short, imperative-mood subject lines (e.g. `fix ofdm frame sync off-by-one`). Reference issues with `Fixes #N` or `Closes #N` in the body when applicable.

---

## Adding or upgrading dependencies

Follow the checklist in [DEPENDENCIES.md](./DEPENDENCIES.md) before opening any PR that introduces or upgrades a dependency. The CI `security` workflow enforces `cargo audit` and `cargo deny check` on every push.

**Quick security checks:**

```bash
cargo audit                  # known CVEs via RustSec
cargo deny check             # license, ban, and source policy
cargo outdated --workspace   # show stale versions
cargo machete                # detect unused dependencies
```

---

## Architecture overview

The signal pipeline flows left to right:

```
RTL-SDR IQ → [sdr] → [ofdm] → [fec] → [protocol] → [audio]
```

See [CLAUDE.md](./CLAUDE.md) for a detailed breakdown of each crate's responsibilities, threading model, and key implementation notes (soft-bit layout, DAB Mode I constants, etc.).

---

## Reporting issues

- **Bugs and feature requests:** Open a GitHub issue with as much detail as possible (OS, hardware, DAB channel, log output with `RUST_LOG=debug`).
- **Security vulnerabilities:** See [SECURITY.md](./SECURITY.md).

---

## License

By contributing, you agree that your contributions will be licensed under the same terms as the project (MIT OR Apache-2.0).
