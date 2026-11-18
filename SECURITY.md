# Security Policy

## Supported Versions

Image Service has a published production-baseline release. Security fixes are
provided for the latest stable release line and the current `main` branch.

| Version | Supported |
| --- | --- |
| Latest stable release line (`v0.1.x`, currently `v0.1.0`) | Yes |
| `main` (unreleased development) | Best effort |
| Older release lines, snapshots, or commits | No |

Users should upgrade to the latest stable release to receive security fixes.
The project does not currently promise backports to older release lines. This
table will be updated when a new stable release line is published or support
changes.

## Reporting a Vulnerability

Please **do not report security vulnerabilities through public GitHub issues, pull requests, discussions, or other public project channels**.

If the repository's **Security** tab offers **Report a vulnerability**, use it
to submit a private report visible to the maintainers.

If private reporting is unavailable, contact a repository maintainer through
a private channel before sharing exploit details. Do not put exploit details,
credentials, private URLs, sensitive images, or other confidential information
in a public issue. The project does not publish a separate security email
address.

Maintainers will acknowledge and triage reports as promptly as practical,
coordinate remediation with the reporter, and disclose confirmed issues after
a fix or mitigation is available. Response and remediation times are not
guaranteed; please do not publicly disclose an unpatched vulnerability.

## What to Include

When a private reporting channel is available, a useful report should include:

- A clear description of the vulnerability.
- The affected endpoint, component, or configuration.
- Reproduction steps or a minimal proof of concept.
- The expected security impact.
- The version, commit, or deployment configuration tested.
- For container deployments, the image tag and digest, target architecture,
  and whether the image came from GHCR or a local build.
- Any relevant logs or error messages with secrets and personal data removed.
- Suggested mitigations, if known.

Please use the minimum amount of sensitive data necessary to demonstrate the issue.

## Security Scope

Examples of issues that should be reported privately include:

- Authentication or authorization bypasses.
- Path traversal or unintended filesystem access.
- Arbitrary file read or write behavior.
- Denial-of-service issues caused by malformed or adversarial uploads.
- Image-decoder vulnerabilities that can be triggered through the service.
- Exposure of API keys, credentials, or sensitive configuration.
- Request-size or decoder-limit bypasses.
- Unsafe behavior involving storage backends.
- Vulnerabilities in dependencies that are exploitable through Image Service.

Normal bugs that do not have a security impact can be reported using the public bug-report template.

## Disclosure

Please allow maintainers a reasonable opportunity to investigate and prepare a
fix before publicly disclosing vulnerability details. For confirmed issues,
the project will communicate the affected versions, impact, mitigation, and
fixed release through a GitHub security advisory or release notes as
appropriate.

## Third-Party Dependencies

Image Service depends on third-party Rust crates and native image-processing components.

If a vulnerability originates entirely in an upstream dependency, reports are still useful when the issue is exploitable through Image Service. The project may coordinate remediation through a dependency update or upstream fix.
