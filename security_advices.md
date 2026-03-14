# Security Advices

This document covers security guidance for **dab-rtl** — a local CLI/TUI DAB radio receiver. It focuses on threats that apply to this project's current scope, and provides forward-looking advice if the application scope grows.

---

## Current Scope

dab-rtl is a local desktop application with no network services, no authentication, no database, and no cloud infrastructure. It receives radio signals via RTL-SDR hardware, decodes DAB audio, and outputs it locally.

---

## 1. Dependency Security

This is the highest-priority security concern for this project. Third-party crates can introduce known vulnerabilities.

**Practices in place:**
- `cargo audit` checks for CVEs in CI (see `.github/workflows/security.yml`)
- `cargo deny check` enforces license, ban, and source policy
- `DEPENDENCIES.md` documents each dependency with an evaluation checklist

**When adding or upgrading a dependency:**
- Work through the checklist in `DEPENDENCIES.md`
- Run `cargo audit` and `cargo deny check` locally before opening a PR
- Prefer well-maintained crates with recent activity and a clear security policy
- Avoid crates that pull in large transitive dependency trees unless necessary

---

## 2. Secrets and Credentials

Even in a local tool, secrets must not be hardcoded.

- Never commit API keys, tokens, or credentials to source code or config files
- Add any secret-containing files to `.gitignore`
- If RTL-SDR vendor or device authentication is ever added, store credentials outside the repository

---

## 3. Safe Rust Practices

Rust's safety guarantees are a major security asset. Maintain them.

- Avoid `unsafe` blocks; if one is necessary, add a comment explaining why it is sound
- Prefer bounded operations over unchecked indexing when processing external data (IQ samples, FIB/FIC bytes received from the device)
- Treat data arriving from the RTL-SDR device as untrusted: validate lengths and ranges before use
- Handle errors explicitly; avoid `unwrap()`/`expect()` on paths that process device input

---

## 4. CLI Input Handling

- Validate and sanitize all command-line arguments before use (e.g., frequency values, channel names)
- Do not pass user-supplied strings directly to shell commands or file paths without sanitization
- Reject out-of-range values early with a clear error message

---

## 5. Error Handling and Logging

- Error messages should be descriptive for debugging but must not expose sensitive system details (e.g., full file paths with home directory contents, internal memory addresses)
- Log at appropriate levels; avoid leaving verbose debug output enabled in release builds
- Use `RUST_LOG` for runtime log control rather than compile-time verbosity

---

## Scope Increase Advice

If the application grows beyond a local CLI tool, additional security concerns become relevant.

### If a network interface or HTTP API is added
- Authenticate every endpoint; default-deny unauthenticated access
- Validate all input server-side against a strict schema
- Rate-limit per client/IP
- Set security response headers (`Content-Security-Policy`, `Strict-Transport-Security`, etc.)
- Enforce TLS 1.2+ (prefer TLS 1.3); never expose plaintext control interfaces

### If persistent storage or a database is added
- Use parameterized queries for all database operations — never string concatenation
- Encrypt sensitive data at rest

### If user accounts or authentication are added
- Hash passwords with Argon2id (or bcrypt with cost ≥ 12); never MD5/SHA-1/plain SHA-256
- Enforce generic error messages for auth failures (never reveal which field was wrong)
- Regenerate session IDs on login; invalidate on logout

### If cloud deployment is added
- Apply least-privilege IAM roles; no wildcard (`*`) permissions
- Run containers as non-root
- Scan container images and IaC templates in CI

For each of these scenarios, consult the [OWASP Top 10](https://owasp.org/Top10/) and the [OWASP Application Security Verification Standard (ASVS)](https://owasp.org/www-project-application-security-verification-standard/).

---

## Remediation Priority

| Risk | Fix within |
|------|-----------|
| Critical CVE in dependency | 24–72 hours |
| High CVE in dependency | 30 days |
| Medium | 90 days |
| Low | Next release cycle |

Escalate immediately if the vulnerability is in the [CISA KEV catalog](https://www.cisa.gov/known-exploited-vulnerabilities-catalog).
