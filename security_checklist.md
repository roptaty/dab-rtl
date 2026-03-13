# Security checklist

## 1. Authentication

| # | Check | Risk | Scope |
|---|-------|------|-------|
| 1.1 | Hash passwords with **Argon2id** or bcrypt (cost ≥ 12). Never use MD5, SHA-1, or plain SHA-256. | 🔴 | All |
| 1.2 | Return the same generic error for all auth failures (`"Invalid credentials"`). Never reveal whether email or password was wrong. | 🟠 | All |
| 1.3 | Regenerate session ID after login. Invalidate it fully on logout — server-side. | 🟠 | Web |
| 1.4 | Set cookies with `Secure`, `HttpOnly`, and `SameSite=Strict` (or `Lax`). | 🟠 | Web |
| 1.5 | Validate JWT claims: `iss`, `aud`, `exp`, `alg`. Never accept `alg: none`. Keep access token lifetime ≤ 15 min. | 🟠 | API |
| 1.6 | Lock or throttle accounts after 5–10 failed login attempts. | 🟡 | All |
| 1.7 | Support MFA (TOTP or WebAuthn). Avoid SMS where possible. | 🟡 | All |

*Tools: Semgrep (auth rules), CodeQL, OWASP ZAP (session analysis)*

---

## 2. Authorization

| # | Check | Risk | Scope |
|---|-------|------|-------|
| 2.1 | Enforce authorization server-side on **every** endpoint. Never rely on UI hiding. | 🔴 | All |
| 2.2 | Check object ownership before returning any resource (`user_id == resource.owner_id`). | 🔴 | API |
| 2.3 | Default deny: if no explicit permission exists, block access. | 🔴 | All |
| 2.4 | Use UUIDs instead of sequential IDs. Always verify ownership on direct object access. | 🟠 | Web/API |
| 2.5 | Admin endpoints must verify admin role server-side, not just client-side. | 🟠 | All |
| 2.6 | Apply least privilege to all IAM roles and service accounts. No wildcard (`*`) permissions. | 🔴 | Cloud |

*Tools: Burp Authorize plugin, OWASP ZAP, IAM Access Analyzer (cloud)*

---

## 3. Input Validation & Output Encoding

| # | Check | Risk | Scope |
|---|-------|------|-------|
| 3.1 | Use **parameterized queries** for all DB operations. Zero string concatenation with user data. | 🔴 | All |
| 3.2 | Validate all input server-side: type, length, range, format. Allowlists over denylists. | 🔴 | All |
| 3.3 | Use framework auto-escaping (React JSX, Django templates). Never disable or bypass it. | 🔴 | Web |
| 3.4 | Apply context-specific encoding: HTML entities in HTML, JS encoding in script context, URL encoding in URLs. | 🟠 | Web |
| 3.5 | Sanitize user-supplied HTML with a library (DOMPurify, Bleach). Never use custom regex. | 🟠 | Web |
| 3.6 | Validate file uploads by magic bytes (not just extension). Enforce size limits. Store outside webroot. | 🟠 | Web |
| 3.7 | Disable XML external entity (XXE) resolution in your XML parser. | 🟠 | API |

*Tools: Semgrep (injection rules), CodeQL, SonarQube, OWASP ZAP*

---

## 4. Cryptography & Secrets

| # | Check | Risk | Scope |
|---|-------|------|-------|
| 4.1 | **Never hardcode secrets**, API keys, or credentials in source code, config files, or Docker images. | 🔴 | All |
| 4.2 | Add `.env` to `.gitignore`. Commit only `.env.example` with placeholder values. | 🔴 | All |
| 4.3 | Enforce TLS 1.2+ everywhere. Prefer TLS 1.3. Enable HSTS with `includeSubDomains`. | 🔴 | All |
| 4.4 | Use AES-256-GCM or ChaCha20-Poly1305 for encryption. Never use DES, 3DES, or RC4. | 🔴 | All |
| 4.5 | Generate tokens and random values with a CSPRNG only (`crypto.randomBytes`, `secrets`, `SecureRandom`). Never `Math.random()`. | 🟠 | All |
| 4.6 | Store secrets in a secrets manager (Vault, AWS Secrets Manager, Azure Key Vault). Not in env vars baked into images. | 🟠 | Cloud |
| 4.7 | Run secret scanning as a pre-commit hook and in CI. Block commits that contain detected secrets. | 🟠 | All |

*Tools: TruffleHog, Gitleaks, GitHub Secret Protection, detect-secrets*

---

## 5. Dependencies & Supply Chain

| # | Check | Risk | Scope |
|---|-------|------|-------|
| 5.1 | Scan dependencies for known vulnerabilities in CI. Block on Critical/High findings. | 🔴 | All |
| 5.2 | Commit lock files (`package-lock.json`, `Pipfile.lock`, `Gemfile.lock`). Never ignore them. | 🟠 | All |
| 5.3 | Pin dependency versions. Avoid floating major version ranges. | 🟠 | All |
| 5.4 | Patch CISA KEV-listed vulnerabilities within 48 hours. | 🔴 | All |
| 5.5 | Review new dependencies before adding: maintenance status, license, known issues. | 🟡 | All |
| 5.6 | Generate an SBOM (CycloneDX or SPDX) for release artifacts. | 🟡 | All |

*Tools: Trivy, Snyk, Dependabot, Grype, Syft (SBOM), Socket.dev*

---

## 6. API Security

| # | Check | Risk | Scope |
|---|-------|------|-------|
| 6.1 | Authenticate every API endpoint. No anonymous access to data endpoints. | 🔴 | API |
| 6.2 | Rate-limit per client/IP, tuned to endpoint sensitivity. | 🟠 | API |
| 6.3 | Validate all request parameters server-side against a defined schema. | 🟠 | API |
| 6.4 | Configure CORS with specific allowed origins. Never use `*` in production. | 🟠 | API |
| 6.5 | Return only required fields in responses. Never expose full database objects. | 🟠 | API |
| 6.6 | Block mass assignment: define explicit input schemas, reject unexpected fields. | 🟠 | API |
| 6.7 | Treat third-party API responses as untrusted — validate and schema-check them. | 🟡 | API |

*Tools: OWASP ZAP (API mode), Spectral (OpenAPI linting), Nuclei, Postman + Pynt*

---

## 7. Logging, Monitoring & Error Handling

| # | Check | Risk | Scope |
|---|-------|------|-------|
| 7.1 | **Fail securely**: deny access when an error occurs. Never fail-open. | 🔴 | All |
| 7.2 | Return generic error messages to users. Log full details server-side only. | 🟠 | All |
| 7.3 | Disable debug mode and verbose error output in production. | 🟠 | All |
| 7.4 | Log all auth events (success, failure, lockout) and all authorization failures. | 🟠 | All |
| 7.5 | **Never log** passwords, tokens, session IDs, credit card numbers, or SSNs. | 🔴 | All |
| 7.6 | Use structured logging (JSON) with consistent fields: `timestamp`, `level`, `event`, `user`, `ip`, `correlation_id`. | 🟡 | All |
| 7.7 | Implement global exception handlers. Unhandled errors must never propagate raw to the client. | 🟠 | All |
| 7.8 | Add timeouts and circuit breakers on outbound calls to prevent cascade failures. | 🟡 | All |

*Tools: Semgrep (error handling rules), OWASP ZAP (error page detection)*

---

## 8. Infrastructure & Cloud

| # | Check | Risk | Scope |
|---|-------|------|-------|
| 8.1 | Scan IaC templates (Terraform, CloudFormation, K8s YAML, Dockerfiles) in CI. Block on Critical findings. | 🔴 | Cloud |
| 8.2 | Run containers as non-root. Use read-only filesystem where possible. | 🟠 | Cloud |
| 8.3 | Scan container images for vulnerabilities before deployment. | 🟠 | Cloud |
| 8.4 | Enable encryption at rest for all data stores (S3, RDS, EBS, DynamoDB). | 🟠 | Cloud |
| 8.5 | Set security response headers: `Content-Security-Policy`, `Strict-Transport-Security`, `X-Content-Type-Options`, `X-Frame-Options`. | 🟠 | Web |
| 8.6 | Restrict network access with security groups and K8s network policies. Limit egress. | 🟠 | Cloud |

*Tools: Checkov, Trivy (IaC), KICS, Docker Bench, kube-bench, IAM Access Analyzer*

---

## 9. Data Protection

| # | Check | Risk | Scope |
|---|-------|------|-------|
| 9.1 | Encrypt all data in transit (TLS 1.2+) and at rest (AES-256). | 🔴 | All |
| 9.2 | Collect and return only the fields you actually need (data minimization). | 🟠 | All |
| 9.3 | Mask or redact PII in non-production environments (dev, staging, test). | 🟠 | All |
| 9.4 | Set `Cache-Control: no-store` on responses containing sensitive data. | 🟡 | Web/API |
| 9.5 | Clear sensitive data from memory after use. Zero-out buffers holding secrets. | 🟡 | All |

*Tools: SSL Labs, testssl.sh, Checkov (encryption-at-rest rules)*

---

## Remediation SLA

| Risk | Fix within |
|------|-----------|
| 🔴 Critical | 24–72 hours |
| 🟠 High | 30 days |
| 🟡 Medium | 90 days |
| 🟢 Low | Next release cycle |

Escalate immediately if the vulnerability is in the [CISA KEV catalog](https://www.cisa.gov/known-exploited-vulnerabilities-catalog) or affects auth, authorization, or data access paths.

---

---

# Appendix

## A. Framework Reference Matrix

Maps checklist sections to the underlying frameworks for compliance traceability.

| Section | OWASP Top 10 (2025) | ASVS 5.0 | NIST 800-53 | NIST CSF 2.0 | OWASP API Top 10 (2023) |
|---------|---------------------|----------|-------------|-------------|------------------------|
| Authentication | A07 | V6, V9, V10 | IA-5 | PR.AA-03 | API2 |
| Authorization | A01 | V8 | AC-3, AC-6 | PR.AA-05 | API1, API5 |
| Input Validation | A05 | V1, V2 | SI-10 | PR.DS-01 | API3 |
| Cryptography | A04 | V11, V12 | SC-8, SC-12, SC-13 | PR.DS-01/02 | — |
| Secrets | A04 | V11 | IA-5(7), SC-12 | PR.DS-01 | — |
| Dependencies | A03 | V15 | SA-15 | ID.RA-01 | API10 |
| API Security | A01, A05 | V4 | AC-3, SI-10 | PR.AA | API1–10 |
| Logging & Errors | A09, A10 | V16 | SI-11, AU-2 | DE.CM-01 | — |
| Infrastructure | A02 | V13 | AC-6, SA-15 | PR.PS | API8 |
| Data Protection | A04 | V14 | SC-28 | PR.DS-01/02/10 | API3 |

---

## B. Approved Cryptographic Algorithms (2025)

| Purpose | Use | Do NOT use |
|---------|-----|-----------|
| Symmetric encryption | AES-256-GCM, ChaCha20-Poly1305 | DES, 3DES, RC4, Blowfish |
| Password hashing | Argon2id · scrypt · bcrypt (≥12) · PBKDF2 (≥600K iterations, SHA-256) | MD5, SHA-1, SHA-256 (plain) |
| Asymmetric / signing | RSA ≥ 2048, ECDSA P-256+, Ed25519 | RSA < 2048, DSA |
| General hashing | SHA-256, SHA-384, SHA-512 | MD5, SHA-1 |
| Key exchange | ECDH P-256/P-384, X25519 | DH < 2048 |
| Random generation | CSPRNG: `crypto.randomBytes`, `secrets.token_bytes`, `SecureRandom` | `Math.random()`, `rand()` |

---

## C. Recommended Toolchain

### Free / Open-Source Baseline

| Layer | Tool | What it covers |
|-------|------|----------------|
| SAST | [Semgrep OSS](https://semgrep.dev) | Injection, auth patterns, crypto misuse |
| SAST | [CodeQL](https://codeql.github.com) | Deep semantic analysis, data flow |
| SCA + Container + IaC | [Trivy](https://trivy.dev) | Dependencies, container CVEs, IaC, secrets |
| IaC | [Checkov](https://checkov.io) | Terraform, CloudFormation, K8s (1,000+ policies) |
| Secrets (pre-commit) | [Gitleaks](https://gitleaks.io) | Secrets in git history and staged files |
| Secrets (CI) | [TruffleHog](https://trufflesecurity.com/trufflehog) | 700+ detectors with live API verification |
| DAST | [OWASP ZAP](https://zaproxy.org) | Web and API scanning, active/passive |
| SBOM | [Syft](https://github.com/anchore/syft) + [Grype](https://github.com/anchore/grype) | Software inventory + vulnerability matching |

### GitHub-Native (included with GitHub Advanced Security)

| Layer | Tool |
|-------|------|
| SAST | CodeQL |
| SCA | Dependabot (auto-fix PRs) |
| Secrets | GitHub Secret Protection (push protection, 200+ patterns) |

### Commercial (higher signal, lower noise)

| Layer | Tool | Differentiator |
|-------|------|----------------|
| SAST | Semgrep Enterprise / Checkmarx One | Cross-file taint analysis |
| SCA | Snyk | Proprietary DB (~47 days ahead of NVD); reachability analysis |
| DAST | Burp Suite DAST | Highest accuracy; blind/out-of-band detection |
| Secrets | TruffleHog Enterprise | Active verification eliminates false positives |
| Platform (ASPM) | Aikido / OX Security | Unified finding correlation across all tools |

---

## D. What Changed in 2025

**OWASP ASVS 5.0** (May 2025): completely restructured from 14 to 17 chapters. All requirement IDs changed. New dedicated chapters for OAuth/OIDC (V10), Self-Contained Tokens (V9), and Web Frontend Security (V3). Target **L2** as your baseline.

**OWASP Top 10 2025** (November 2025): based on 175,000+ applications. Two new categories:
- **A03 — Software Supply Chain Failures** (dependency and build pipeline attacks)
- **A10 — Mishandling of Exceptional Conditions** (fail-open error handling)

SSRF merged into A01. Broken Access Control remains #1.

**NIST CSF 2.0** (February 2024): added a sixth function (Govern). Developer-relevant subcategories: PR.AA (access control), PR.DS (data security), DE.CM (monitoring), ID.RA (risk/threat modeling).

**NIST SP 800-53 rev 5**: most developer-relevant control families remain SA (secure SDLC), SI (input validation, error handling), AC (access control), IA (authentication), and SC (cryptography, TLS).

---

## E. Logging — What to Log and What Not to Log

**Always log:**
- Auth events: success, failure, lockout — with timestamp, user ID, IP, user agent
- Authorization failures and access denials
- Input validation failures on sensitive endpoints
- Admin and security configuration changes
- Security control failures (TLS errors, crypto failures)

**Never log:**
- Passwords or authentication tokens
- Session IDs or refresh tokens
- Credit card numbers, SSNs, health data
- Raw request bodies that may contain any of the above

Use structured JSON logging with consistent fields: `timestamp`, `level`, `event`, `user_id`, `ip`, `correlation_id`.
