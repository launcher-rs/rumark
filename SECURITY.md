# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.2.x   | :white_check_mark: |
| < 0.2   | :x:                |

## Reporting a Vulnerability

If you discover a security vulnerability in Rumark, please report it responsibly:

1. **Preferred**: Open a [GitHub Security Advisory](https://github.com/launcher-rs/rumark/security/advisories/new) to privately disclose the issue.
2. **Alternative**: Open a regular issue with `[Security]` in the title — but please avoid including exploit details in public.

Please include:
- A description of the vulnerability
- Steps to reproduce
- The affected version(s)
- Any potential impact

We aim to acknowledge reports within 48 hours and will keep you informed of the fix progress.

## Scope

Rumark is a local-first desktop Markdown editor. Security-relevant areas include:

- **HTML sanitization**: The built-in HTML sanitizer (`sanitize_html_fragment`) must not allow XSS via pasted or exported content.
- **File operations**: Atomic file writes must not be bypassable to corrupt user documents.
- **Network requests**: Remote image fetching (`reqwest` client) must enforce response size limits.
- **Windows registry**: File association and context menu entries must only write to HKCU.