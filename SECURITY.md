# Security Policy

PlaySparse is experimental software and should not be pointed at irreplaceable data.

## Principles

- source directories are read-only inputs;
- packed stores are written to separate paths;
- reconstruction must verify integrity before source deletion is ever considered;
- no DRM, anti-cheat or license-check bypass features are accepted;
- malformed manifests/chunks must be treated as untrusted input.

Please report security issues privately to the repository owner rather than publishing an exploit in an issue.
