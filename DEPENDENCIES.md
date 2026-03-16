# Dependency Evaluation Guidelines

This document describes the process for evaluating, adding, and maintaining
third-party dependencies in `dab-rtl`. Follow these steps whenever you want to
introduce a new crate or upgrade an existing one.

---

## Current dependencies

| Crate | Version | Used in | Purpose | License |
|-------|---------|---------|---------|---------|
| `rtlsdr_mt` | 2.1.0-rc1 | `sdr` | RTL-SDR hardware interface | MIT |
| `num-complex` | 0.4 | workspace | Complex number arithmetic (IQ samples) | MIT / Apache-2.0 |
| `rustfft` | 6 | `ofdm` | 2048-point FFT for OFDM demodulation | MIT / Apache-2.0 |
| `log` | 0.4 | workspace | Structured logging facade | MIT / Apache-2.0 |
| `thiserror` | 1 | workspace | Ergonomic `std::error::Error` derive | MIT / Apache-2.0 |
| `env_logger` | 0.11 | `app` | `RUST_LOG` log filtering at runtime | MIT / Apache-2.0 |
| `clap` | 4 | `app` | CLI argument parsing | MIT / Apache-2.0 |
| `ratatui` | 0.26 | `app` | Terminal UI framework | MIT |
| `crossterm` | 0.27 | `app` | Cross-platform terminal control | MIT |
| `cpal` | 0.15 | `audio` | Cross-platform audio output (ALSA) | Apache-2.0 |
| `symphonia` | 0.5 | `audio` | MP2 / AAC audio decoding | MPL-2.0 |
| `serde` | 1 | `app` | Serialization framework (derive macros for cache structs) | MIT / Apache-2.0 |
| `serde_json` | 1 | `app` | JSON encoding/decoding for the channel cache file | MIT / Apache-2.0 |
| `tempfile` | 3 | `app` (dev) | Temporary directories for cache unit tests | MIT / Apache-2.0 |

---

## Checklist for adding a new dependency

Before opening a PR that adds or upgrades a dependency, work through each item:

### 1. Necessity
- [ ] There is no suitable crate already in the workspace.
- [ ] The functionality cannot be implemented in a reasonable amount of
      maintainable code without the dependency.

### 2. Maintenance health
Check on [crates.io](https://crates.io) and the crate's repository:
- [ ] Last publish date is within the past 18 months **or** the crate is
      explicitly declared stable/complete.
- [ ] Open issues and PRs do not indicate abandonment or critical unfixed bugs.
- [ ] The crate has at least one active maintainer (check `Cargo.toml` authors
      and commit history).

### 3. Popularity and community trust
- [ ] Download count or GitHub stars give evidence of widespread use.
- [ ] The crate is used by reputable projects in the Rust ecosystem.
- [ ] No credible community concerns (CVEs, supply-chain incidents, etc.) are
      visible in recent discussion.

### 4. License compatibility
- [ ] The license is listed in the `allow` list in `deny.toml`.
- [ ] If the license is copyleft (LGPL, GPL, AGPL), it is explicitly discussed
      and approved by the project owner before merging.
- [ ] `cargo deny check licenses` passes locally.

### 5. Security
- [ ] `cargo audit` reports no known vulnerabilities for the chosen version.
- [ ] The crate does not use `unsafe` excessively. Check with:
      ```bash
      cargo geiger --all-features 2>/dev/null | grep <crate-name>
      ```
- [ ] If the crate uses `unsafe`, the unsafe sections are documented with
      safety comments and audited before acceptance.
- [ ] The crate does not request OS capabilities it does not need (e.g., file
      system access from a pure-math crate).

### 6. Supply-chain integrity
- [ ] The published crate on crates.io matches what is in the public repository
      (check that the repo URL in `Cargo.toml` corresponds to the published
      source).
- [ ] Prefer crates with verified crates.io owners and no history of ownership
      transfers to unknown parties.
- [ ] Consider pinning to a specific version in `Cargo.lock` and reviewing the
      diff when upgrading.

### 7. Transitive dependencies
- [ ] Run `cargo tree -p <crate>` and review what transitive dependencies are
      pulled in.
- [ ] No transitive dependency introduces a license conflict or known
      vulnerability.
- [ ] `cargo deny check` passes after adding the crate.

### 8. Size and compilation impact
- [ ] The dependency does not significantly increase compile times or binary
      size for the functionality it provides.
- [ ] Use `default-features = false` and enable only the features you actually
      need (see `symphonia` for an example).

---

## Running the security tools locally

```bash
# Check for known vulnerabilities
cargo audit

# Enforce license, ban, and source policies
cargo deny check

# Show outdated dependencies
cargo outdated --workspace

# Detect unused dependencies
cargo machete

# Static analysis (requires pip install opengrep)
opengrep scan --config "p/rust" .
```

---

## Upgrading an existing dependency

1. Run `cargo outdated --workspace` to see what is out of date.
2. Update the version in the relevant `Cargo.toml`.
3. Run `cargo update -p <crate>` to resolve the new version into `Cargo.lock`.
4. Run `cargo audit` and `cargo deny check` to verify no new issues.
5. Run `cargo test --all` to confirm nothing is broken.
6. Summarise the upgrade in the PR description, referencing the changelog or
   release notes of the upgraded crate.

---

## Removing a dependency

1. Delete all usages from source code.
2. Remove the entry from `Cargo.toml`.
3. Run `cargo machete` to confirm no other references remain.
4. Run `cargo build --all` to confirm the workspace compiles cleanly.
