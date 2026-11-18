# Security Policy

## Supported Versions

| Version | Supported          |
| :---    | :---               |
| 0.1.x   | :white_check_mark: |

## Reporting a Vulnerability

TALOS is designed with local-first security boundaries:
- Task state and logs are kept strictly in local SQLite WAL (`.talos/state.sqlite3`).
- No credentials or API keys are stored, logged, or sent to third-party endpoints.
- AI processes run with explicit permission sandboxes and role isolation.

If you discover a security vulnerability in TALOS:
1. Please do not open a public issue.
2. Report the vulnerability privately via [GitHub Security Advisories](https://github.com/AVP-Dev/talos/security/advisories) or contact the maintainer directly at security@avpdev.com.
3. Include details of the vulnerability, reproduction steps, and potential impact.

We will acknowledge receipt within 48 hours and work on a resolution promptly.
