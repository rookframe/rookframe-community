# Local deployment and recovery

Provision and deploy from an operator's machine using OpenTofu and Ansible.
No CI/CD integration is required. This repository owns only its Community
server, firewall and attached 10 GiB persistent volume. It must never import or
modify the Package Catalogue or other existing servers.

1. Export `HCLOUD_TOKEN` for the intended Hetzner project in the environment.
   Keep it out of arguments, logs and tracked files. Copy the tfvars example to
   ignored `infra/terraform/terraform.tfvars`. Use restricted administrative
   IPv4 CIDRs and an existing SSH key name when available.
2. Run `tofu -chdir=infra/terraform init`, then `plan -out=community.tfplan`.
   Inspect the plan; apply that saved plan. Retain the private state securely.
3. Add a public DNS A record pointing at the `server_ipv4` output. Preserve
   other records. Verify the server SSH identity and retain its host key.
4. Copy `infra/config.yml.example` to `infra/.local/config.yml`, mode 0600 in a
   mode-0700 directory. Set the public domain and a random database password
   generated using `openssl rand -hex 32`. Never publish the configuration.
5. Commit the complete source, then run `./scripts/deploy.sh` from a clean
   checkout. It archives the exact commit, bootstraps Docker, checks the mounted
   volume, starts PostgreSQL, dumps a pre-migration backup, runs explicit
   migrations, and starts the app and Caddy with valid public HTTPS.

Production directory: `/opt/rookframe-community`. The app is unprivileged,
read-only, capability-free, and memory/process limited. Database and app ports
are private to Compose. Public bodies are limited to 32 KiB, responses/pages
bounded, requests limited to 64 concurrent with a 15-second deadline, and SQL
statements to 10 seconds. Large public operations should add operator-specific
edge rate limits and monitoring. No proxy header can authenticate a caller.
Administrator tokens, Authorization headers, request bodies and database error
values are never logged. `/api/v1/health` checks the database/schema and observed TURN issuance state;
`/api/v1/live` checks the process. No service failure changes a local World.

## Backup and restore

The daily 02:17 UTC job writes atomic gzip pg_dump files on the attached volume
and retains 14 days. `./scripts/backup.sh` copies a fresh backup to private local
storage. Keep an encrypted off-machine copy; volume-local dumps are insufficient.
Use a new empty database for restore checks:

```sh
createdb community_restore_check
gzip -dc path/to/private/backup.sql.gz | psql -v ON_ERROR_STOP=1 community_restore_check
psql community_restore_check -c 'SELECT count(*) FROM world_addresses; SELECT count(*) FROM directory_operations;'
```

Restore only during deliberate maintenance: stop writes, restore into a fresh
volume/database, verify authenticated retry and public readback, and retain the
old volume until accepted. Both address ownership and operation records are
required for correct retries. They contain capability digests and private
historical metadata; protect backups accordingly.

Update by deploying another clean commit with the same private state/config.
The source is unpacked into `releases/<commit>` so deleted files cannot survive
updates. Roll back source only if compatible with the current schema, otherwise
restore a verified backup into a separate database. `prevent_destroy` protects
the data volume from routine plans; no destructive teardown is automated.

## Runtime capability storage

A World administrator creates and retains a random per-address token locally.
Losing it does not transfer ownership; recovering the token from a private backup
is required. There is no password, Participant account, or public reset endpoint.
Public GET never returns tokens, digests or operation history. Removal retains
ownership and retry receipts so stale publication requests cannot resurrect data.
Service moderation, retention cleanup, edits/check-in and lifecycle synchronization
are later slices; operation receipts currently remain until operator-managed
retention can be introduced with an explicit retry horizon.

## Setup operations

WebRTC setup uses the same deployment and public Community Server Address.
No new database migration or inbound port is required for Cloudflare. The service
holds availability leases, setup attempts and temporary credential grants in
bounded memory. Signaling expires after 60 seconds; grants remain until credential
expiry to permit separate revocation. Neither is backed up. A service restart
clears pending setup; established gameplay continues using its native transport.
The Community Server never carries gameplay traffic.

### TURN provider configuration

Keep provider credentials in mode-0600 `infra/.local/config.yml`, never in source,
client exports, command arguments or logs. The Compose template passes only the
selected provider's secrets. The example file documents the deployment keys.

| Runtime variable | Cloudflare | Independent coturn |
| --- | --- | --- |
| `TURN_PROVIDER` | `cloudflare` | `coturn` |
| `TURN_TTL_SECONDS` | Default 21600; range 300–86400 | Same |
| `CLOUDFLARE_TURN_KEY_ID` | TURN key identifier | Unset |
| `CLOUDFLARE_TURN_API_TOKEN` | TURN key's backend API token | Unset |
| `COTURN_URLS` | Unset | Comma-separated `stun:relay.example:3478,turn:relay.example:3478?transport=udp` |
| `COTURN_SHARED_SECRET` | Unset | 32–256 byte random secret shared only by this backend and coturn |

Cloudflare uses its documented credential issuance/revocation REST endpoints,
with an opaque Connectivity Principal as `customIdentifier`. The application
receives only short-lived ICE credentials and filters unsupported TCP/TLS URLs.
The API token never crosses the backend boundary. Analytics credentials are
operator-only and are not required on the deployed Community Server.

For independent operation, deploy this same public source at your own HTTPS
Community Server Address with `TURN_PROVIDER=coturn`. Run stock coturn on a public
UDP address using `use-auth-secret`, the matching `static-auth-secret`, a stable
`realm`, `fingerprint`, `no-tcp`, `no-tls`, `no-dtls`, `no-tcp-relay`, and `no-cli`.
Set finite `user-quota`, `total-quota`, `max-bps`, `bps-capacity` and a bounded
`min-port`/`max-port` relay range for your capacity. Allow UDP 3478 and that relay
range through the firewall. Configure `external-ip` when behind NAT. Deny relay
peers in loopback, private, link-local, multicast and other internal ranges,
including IPv6; do not permit TURN access to cloud metadata or private services.
Use separate infrastructure from private databases. Restrict administrative
access and protect the shared secret and any provider allocation logs.

Select that HTTPS Community Server Address in Manager before hosting a new World
or rendering its Invitation. Both Directory and setup use it; the client has no
separate relay setting, central credential, Rookframe account or hardcoded STUN
fallback. This deployment requires neither Cloudflare nor a Rookframe service.
Changing an existing published World's server remains governed by its existing
address-administration contract; it does not silently migrate Directory ownership.

### Expiry, revocation and abuse control

Choose credential TTL longer than the longest expected World Session, within the
24-hour provider bound. Stock Godot does not expose in-place credential refresh
for an established native peer. This implementation does not renew credentials
behind a running session; if the relay later cannot refresh an allocation, the
ordinary disconnect/reconnect boundary applies. A fresh join obtains fresh
credentials. Six hours is the default, not a promise of unlimited session length.

Each side of an attempt has one retained grant. Identical retries reuse its
result; a failed issuance needs a new bounded attempt. Issuance is capped at four
provider calls concurrently, 64 grants/minute globally, 128 retained grants per
World and 1024 globally, all retained until expiry. Use provider bandwidth and
allocation quotas plus monitoring to bound spend; issuance limits alone do not
cap bytes. Review aggregate `/metrics` counters and opaque-principal
issuance outcomes. Never enable HTTP body/header tracing. Principals are technical
attribution, not verified human identity; no account system is introduced.

`DELETE .../connections/{attempt}/relay` first denies further issuance. Cloudflare
then revokes the issued username using its API. Retry a 503 if provider revocation
failed. Coturn's HMAC REST scheme has no per-username revocation API: the response
honestly reports `provider_revoked: false`, and issued credentials expire at their
original deadline. Emergency provider-wide denial requires rotating the shared
secret on backend and coturn and terminating existing allocations. Stop/restart
the affected coturn instance for that operation; it disconnects active relayed
sessions. Cloudflare operators can revoke their TURN key for equivalent emergency
containment. Do not confuse signaling deletion with provider revocation.

Readiness reflects configured issuance and the last observed issuance result; it
is not an active provider probe and cannot prove relay reachability. Metrics are
process-local and reset on restart. Cloudflare telemetry or protected coturn
allocation counters must corroborate actual relay selection and egress. Retain
only redacted aggregates in evidence. Document time windows, provider units and
price basis when computing table-hour cost.

Provider references: [Cloudflare credentials](https://developers.cloudflare.com/realtime/turn/generate-credentials/),
[Cloudflare analytics](https://developers.cloudflare.com/realtime/turn/analytics/),
[coturn configuration](https://github.com/coturn/coturn/blob/master/README.turnserver).

## Capacity migration

Migration `0002_capacity.sql` adds bounded reservation/claim counts and an
admission revision, defaulting existing addresses to zero. Deployment backs up
PostgreSQL before applying it. The health probe references these columns, so an
unmigrated deployment cannot report readiness. Automatic Private cleanup keeps
address ownership and operation receipts. It never republishes a World.
