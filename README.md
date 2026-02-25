# irssi-v5

Web IRC client. Rust backend, Cloudflare Access auth, ttyd terminal, soju bouncer.

## Stack

| | |
|---|---|
| **Rust + Axum** | HTTP server, CF JWT validation, session management |
| **ttyd** | Per-user browser terminal (spawned on demand) |
| **irssi** | IRC client (launched by ttyd) |
| **soju** | IRC bouncer — persistent connections + message backlog |
| **Cloudflare Access** | SSO — GitHub, Apple, Google, etc. |
| **Caddy + cloudflared** | TLS + tunnel (caddy-proxy stack) |

## How it works

```
Browser → Cloudflare Access (OAuth) → CF signs JWT
       → irssi-v5 (validates JWT, email → username)
       → ensure soju account exists (sojuctl)
       → spawn ttyd running irssi (connects to soju)
       → proxy browser ↔ ttyd
       → soju keeps IRC alive when browser closes
```

## Setup

### 1. Cloudflare

- Tunnel via caddy-proxy stack
- Access Application for `irc.yourdomain.com` — copy the AUD tag
- Identity providers: GitHub, Apple, Google (Zero Trust → Settings → Authentication)

### 2. Configure

```bash
cp env.example.txt .env && chmod 600 .env
$EDITOR .env
```

### 3. Deploy

```bash
podman-compose build
podman-compose up -d
```

## Development

```bash
# .env: DEV_MODE=true, DEV_USER=yourname
# Requires irssi, ttyd, soju installed locally

cargo run
```

## Project structure

```
src/
├── main.rs          # Axum server, all HTTP handlers
├── config.rs        # Config from environment
├── auth/mod.rs      # CF JWT validation + JWKS caching
├── session/mod.rs   # ttyd process management (tokio::process)
├── soju/mod.rs      # soju user provisioning via sojuctl
└── store/mod.rs     # SQLite via sqlx
```

## Notes

- First build is slow (~2–3 min) due to Rust compilation — subsequent builds use Docker layer cache for dependencies
- `RUST_LOG=irssi_v5=debug` for verbose logging
- `Cargo.lock` is committed — use `cargo update` to bump dependencies

## License

MIT
