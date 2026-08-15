# Security Policy

## Supported Versions

Image Service is currently in early development and does not yet have a stable release.

Until the first supported release is published, security fixes are applied to the latest code on the `main` branch.

| Version | Supported |
| --- | --- |
| `main` | Yes |
| Unreleased snapshots or older commits | No guarantee |

This policy will be updated when versioned releases are published.

## Reporting a Vulnerability

Please **do not report security vulnerabilities through public GitHub issues, pull requests, discussions, or other public project channels**.

The preferred reporting method is GitHub's private vulnerability reporting feature for this repository.

If the repository shows a **Report a vulnerability** option under its Security section, use that option to submit the report privately to the maintainers.

If private vulnerability reporting is not available, do not publish exploit details, credentials, private URLs, sensitive images, or other confidential information in a public issue. A dedicated external security-reporting address has not yet been established.

## What to Include

When a private reporting channel is available, a useful report should include:

- A clear description of the vulnerability.
- The affected endpoint, component, or configuration.
- Reproduction steps or a minimal proof of concept.
- The expected security impact.
- The version, commit, or deployment configuration tested.
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

Please allow maintainers a reasonable opportunity to investigate and prepare a fix before publicly disclosing vulnerability details.

The project will aim to communicate clearly about confirmed vulnerabilities and remediation when a supported release process is established.

## Third-Party Dependencies

Image Service depends on third-party Rust crates and native image-processing components.

If a vulnerability originates entirely in an upstream dependency, reports are still useful when the issue is exploitable through Image Service. The project may coordinate remediation through a dependency update or upstream fix.
