# Security Policy

## Supported Versions

AletheiaDB is currently in pre-1.0 development. We provide security updates for the latest released version.

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |
| < 0.1   | :x:                |

## Reporting a Vulnerability

We take the security of AletheiaDB seriously. If you discover a security vulnerability, please follow responsible disclosure practices.

### How to Report

**DO NOT** create a public GitHub issue for security vulnerabilities.

Instead, please report security issues privately:

1. **Email**: Send details to [security contact - TBD]
2. **GitHub Security Advisory**: Use GitHub's [private vulnerability reporting](https://github.com/madmax983/AletheiaDB/security/advisories/new)

### What to Include

Please include as much of the following information as possible:

- Type of vulnerability (e.g., buffer overflow, SQL injection, XSS)
- Affected version(s)
- Step-by-step instructions to reproduce the issue
- Proof-of-concept or exploit code (if available)
- Potential impact of the vulnerability
- Suggested fix (if you have one)

### Response Timeline

- **Initial Response**: Within 48 hours
- **Triage**: Within 5 business days
- **Status Updates**: Every 7 days until resolved
- **Public Disclosure**: Coordinated with reporter (typically 90 days after fix)

### Security Update Process

1. We will confirm receipt of your vulnerability report
2. We will investigate and validate the vulnerability
3. We will develop a fix and prepare a security patch
4. We will coordinate public disclosure timing with you
5. We will release the security update and credit you (unless you prefer to remain anonymous)

## Security Best Practices for Users

### For Production Deployments

1. **Keep Updated**: Always run the latest version with security patches
2. **Access Control**: Implement proper authentication and authorization
3. **Network Security**: Use encrypted connections for remote access
4. **Backup**: Maintain regular backups of your data
5. **Monitoring**: Monitor logs for suspicious activity

### For Developers

1. **Dependency Audits**: Run `cargo audit` regularly (automated via Dependabot)
2. **Code Review**: All code changes undergo review before merge
3. **Static Analysis**: Use `cargo clippy` with strict lints
4. **Testing**: Comprehensive test coverage including edge cases
5. **Fuzzing**: Property-based testing for critical paths

### Known Limitations (Pre-1.0)

AletheiaDB is under active development. Be aware of these limitations:

- **No Encryption at Rest**: Data is not encrypted on disk (planned for 1.0)
- **Limited Access Control**: Basic authentication only (enhanced ACLs planned)
- **No Audit Logging**: User actions are not logged (planned for 1.0)
- **Development Focus**: Security hardening ongoing, not production-ready

## Security Features

### Current

- **Dependency Scanning**: Automated with Dependabot and cargo-audit
- **Memory Safety**: Rust's memory safety guarantees
- **Input Validation**: Comprehensive validation of all inputs
- **Error Handling**: No panic-based DoS vulnerabilities
- **CI Security Checks**: Automated security audits on every PR

### Planned (1.0 and Beyond)

- Encryption at rest
- TLS/SSL for network connections
- Role-based access control (RBAC)
- Audit logging
- Rate limiting
- Security hardening guide

## Bug Bounty Program

We do not currently have a bug bounty program. Security researchers who report valid vulnerabilities will be credited in our security advisories and release notes (unless they prefer anonymity).

## Security Hall of Fame

We recognize and thank security researchers who help keep AletheiaDB secure:

_No security reports received yet._

## Contact

For security concerns: [TBD - set up security@aletheiadb.io or similar]

For general questions: Create a public GitHub issue

---

Last updated: 2026-01-03
