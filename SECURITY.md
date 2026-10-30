# Security

Run trusted projects only. Native commands use the server user's permissions.
Docker must be enabled explicitly and does not make arbitrary public jobs safe.

- Bind to loopback; use authenticated tunnels for remote access.
- Use scoped API credentials and a private administrator token file.
- Keep credentials out of argv, URLs, pipeline definitions and logs.
- Check snapshots, artifacts and Git history before sharing them.

A public reporting repository is not currently published. Contact the maintainer
privately for vulnerabilities; do not post secrets or exploit details publicly.
[Server setup](docs/server-setup.md) · [Access configuration](docs/server-settings.md).
