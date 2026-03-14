# Security Policy

## Supported versions

| Version | Supported |
|---------|-----------|
| `main` branch | :white_check_mark: |
| Older releases | :x: |

Only the latest code on `main` receives security fixes.

---

## Reporting a vulnerability

**Please do not report security vulnerabilities through public GitHub issues.**

Instead, use [GitHub's private security advisory feature](https://github.com/roptaty/dab-rtl/security/advisories/new) to report the vulnerability privately. This allows us to assess and address the issue before any public disclosure.

### What to include

A good report helps us respond quickly. Please include:

- A clear description of the vulnerability and its potential impact.
- Steps to reproduce (commands, configuration, sample IQ data if relevant).
- The version/commit you tested against.
- Any suggested mitigations or fixes you have in mind.

### What to expect

- **Acknowledgement:** We will acknowledge receipt within 5 business days.
- **Assessment:** We will confirm whether the report is valid and what severity we assign.
- **Fix timeline:** We aim to release a fix within 30 days for critical issues; lower-severity issues may take longer.
- **Disclosure:** We will coordinate a public disclosure date with you once a fix is available. We follow a responsible disclosure model and will credit reporters unless they prefer to remain anonymous.

---

## Scope

This project processes RF signals from consumer hardware and plays audio output. Areas most likely to have security relevance:

- Parsing of untrusted IQ data or DAB bitstreams (buffer overflows, integer overflows in `ofdm`, `fec`, `protocol` crates).
- Audio decoding of untrusted MP2/AAC data via `symphonia` (report upstream to symphonia if applicable).
- CLI argument handling (`clap` in `app`).

Out of scope: denial-of-service via malformed radio signals in a hobby/research context, issues in hardware firmware.

---

## Security tooling

The CI pipeline runs the following on every push and weekly on a schedule:

- `cargo audit` — checks dependencies against the [RustSec Advisory Database](https://rustsec.org/).
- `cargo deny check` — enforces license, banned crate, and source policies.
- Opengrep SAST scan with the Rust ruleset.
