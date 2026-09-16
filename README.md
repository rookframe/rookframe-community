# Rookframe Community Server

An accountless, replaceable World Directory and temporary WebRTC setup exchange. Rust / axum, PostgreSQL, versioned
HTTP API. AGPL-3.0-only; the running deployment serves its exact corresponding
source at `/source.tar.gz`. The service publishes/discovers Worlds, exchanges
WebRTC setup, issues temporary UDP TURN credentials and projects Invitation
capacity. Invitation identity and gameplay admission belong to the World Authority.

## Clean clone

Install Rust 1.97.1 or newer, PostgreSQL 17 and Git. Create an empty local
`community` database owned by your local development role, then:

```sh
git clone https://github.com/rookframe/rookframe-community.git
cd rookframe-community
export DATABASE_URL=postgres://localhost/community
cargo run --locked -- migrate
cargo run --locked
curl --fail http://127.0.0.1:8080/api/v1/live
```

The default listener is loopback. Set `LISTEN_ADDR` explicitly inside a trusted
private reverse-proxy network. Do not expose PostgreSQL or plaintext app traffic.
The lockfile pins Rust dependencies; OpenTofu has its own committed provider lock.
TURN is disabled until configured; `/api/v1/health` returns 503 in that state. Choose
Cloudflare or independent coturn using the private configuration described in
[operations](docs/operations.md#turn-provider-configuration).

## Focused validation

Use a local disposable PostgreSQL role with CREATEDB permission. SQLx makes one
isolated database per test and applies real migrations. No World/Godot journeys
are hidden in this suite.

```sh
DATABASE_URL=postgres://localhost/postgres cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
tofu -chdir=infra/terraform init
tofu -chdir=infra/terraform validate
```

[API contract](docs/api.md) · [Local deployment and recovery](docs/operations.md)

Player administration includes installation request blocks, idempotent removal
cleanup and bounded abuse reports. The optional operator moderation capability
is separate from World administration; see [API](docs/api.md#player-removal-installation-blocks-and-abuse-reports)
and [operations](docs/operations.md#recruitment-moderation).
