# Reporting security vulnerabilities

Report vulnerabilities privately through [GitHub private vulnerability reporting](https://github.com/hanthor/roost-desktop/security/advisories/new). It is enabled for this repository. Include the affected commit or package version, reproduction steps, expected and observed behavior, and whether the issue exposes application data or bypasses a lock or capture permission.

Keep passwords, access tokens, private window contents and personal data out of attachments. Provide a minimal reproduction where possible. Ordinary bugs belong in public issues; undisclosed vulnerabilities belong in the private reporting channel.

This project is a developer preview. Security fixes target the current main branch; historical preview builds have no separate maintenance commitment. Review the [security boundaries](docs/architecture.md#cross-cutting-release-gates) and [control socket authentication decision](docs/adr/0005-control-socket-peer-authentication.md) for the intended trust model. Hardware validation and a complete lock and portal security review remain release gates.

Reports are handled on a best-effort basis with no guaranteed response time. If you have not received an acknowledgement after seven days, follow up in the private report. Coordinate disclosure with the maintainer while a fix and regression test are prepared. Tell us whether you want public credit and which name to use; identifying details stay private unless you agree to publish them.
