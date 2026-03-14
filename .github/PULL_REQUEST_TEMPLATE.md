## Description

<!-- A clear and concise description of what this PR does and why. -->

## Related Issues

<!-- Link to the issue(s) this PR addresses. Use "Closes #<number>" to auto-close on merge. -->
Closes #

## Implementation Notes

<!-- Any notable design decisions, trade-offs, or context reviewers should be aware of. -->

## Testing

<!-- How was this tested? What test cases were added or updated? -->
- [ ] Unit tests added/updated
- [ ] Ran `cargo test --all`
- [ ] Ran `cargo clippy --all-targets --all-features -- -D warnings`
- [ ] Ran `cargo fmt --all -- --check`

## 🔒 Security

- [ ] No secrets or credentials hardcoded in code or config
- [ ] No new `unsafe` blocks introduced without a documented justification
- [ ] CLI input and device data (IQ samples, FIB/FIC bytes) are validated before use
- [ ] Error messages don't expose sensitive system details
- [ ] New dependencies follow the checklist in `DEPENDENCIES.md` (`cargo audit` and `cargo deny check` pass)
