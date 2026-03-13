
## 🔒 Security Checklist
- [ ] No secrets or credentials hardcoded in code or config
- [ ] All DB queries use parameterized statements (no string concatenation)
- [ ] Authorization is enforced server-side on every endpoint touched
- [ ] All user input is validated server-side before use
- [ ] Output is encoded for the correct context (HTML / JS / URL)
- [ ] Error messages don't leak internal details or stack traces
- [ ] New dependencies have been scanned for known vulnerabilities
