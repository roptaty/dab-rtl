
## 🔒 Security

- [ ] No secrets or credentials hardcoded in code or config
- [ ] No new `unsafe` blocks introduced without a documented justification
- [ ] CLI input and device data (IQ samples, FIB/FIC bytes) are validated before use
- [ ] Error messages don't expose sensitive system details
- [ ] New dependencies follow the checklist in `DEPENDENCIES.md` (`cargo audit` and `cargo deny check` pass)
