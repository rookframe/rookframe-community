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

Keep the repository checkout, OpenTofu state and private configuration together
on the operator's machine. The deployment script reads the existing OpenTofu
outputs; it does not provision infrastructure or alter DNS. A new operator also
needs OpenTofu, Ansible, SSH, rsync, the selected provider credentials, and the
verified SSH host key. `infra/.local/`, `dist/`, Terraform state, plans and real
tfvars are ignored. Run `python3 scripts/check-publication.py` before publishing:
it checks all reachable committed history for private file paths and known
credential values from the environment and private configuration. This check
cannot identify an unknown credential merely from its contents.

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

Also verify `join_requests`, `installation_blocks`, `player_removals`,
`abuse_reports` and `_sqlx_migrations`; a successful SQL import alone is not a
restore acceptance. Compare row counts and complete business data with the
backup-time snapshot, using UTC for timestamp serialization. Running authorities
refresh `world_addresses.checked_in_at`, so compare that lease timestamp
separately from durable listing/ownership/capacity data. Use the restored service
on loopback to verify a public listing and authenticated request recovery with
the original privately retained proof. Never print accepted receipts, proofs or
private messages. Do not point the public proxy at the restore-check database.
After acceptance, stop its local process and drop only that disposable database;
retain the protected backup under the operator's retention policy.

Update by deploying another clean commit with the same private state/config.
The source is unpacked into `releases/<commit>` so deleted files cannot survive
updates. Roll back source only if compatible with the current schema, otherwise
restore a verified backup into a separate database. `prevent_destroy` protects
the data volume from routine plans; no destructive teardown is automated.

### Update and source rollback

Use a maintenance window for setup interruption. Before updating, record the
running source revision from the deployment record, download `/source.tar.gz`
and hash it, and record the application image ID and binary hash without dumping
its environment:

```sh
# On the Community host:
docker inspect rookframe-community-app-1 --format '{{.Image}}'
docker exec rookframe-community-app-1 sha256sum /app/community
```

On the operator's machine, from this repository:

```sh
git status --short
git rev-parse HEAD
python3 scripts/check-publication.py
tofu -chdir=infra/terraform validate
./scripts/backup.sh
# Select the reviewed release in a clean checkout, then:
./scripts/deploy.sh
```

Record the actual deployed commit, source archive hash, image ID and binary hash
after each successful deployment. Check public HTTPS `/api/v1/health`, public
Directory reads and a retained authenticated Join Request; compare durable
records with the pre-update snapshot. Check the separately operated Package
Catalogue's health too. Do not publish Compose configuration, `docker inspect`
without a field filter, database dumps, or private API bodies as diagnostics.

For a source rollback, first compare both releases' `migrations/` and the applied
`_sqlx_migrations` versions/checksums. Confirm the previous application supports
the current schema and any data written since the update. With identical schema
and compatible data, use the same operator checkout/state/configuration:

```sh
git switch --detach <previous-full-commit>
./scripts/deploy.sh
```

Repeat the health, source, binary and durable-state checks. To return to the
accepted release, `git switch --detach <accepted-full-commit>` and deploy again;
return the checkout to its original branch afterward. Keep the private state and
configuration in place throughout. Never copy another deployment's state.

This is a source rebuild, not an immutable-image rollback: the Docker base tags
can change even though Cargo.lock and source are fixed. Compare the resulting
artifact identity and retain prior images if byte-identical recovery is required.
An incompatible schema must not be rolled back this way. Stop writes and use the
fresh-database restore procedure above, with an explicit operator decision about
post-backup writes; SQLx has no automatic down-migration here. Do not restore over
the live database or run `docker compose down -v` as a recovery shortcut.

### Restart without deployment

```sh
# On the Community host:
cd /opt/rookframe-community
docker compose -f compose.production.yml restart app
docker compose -f compose.production.yml exec -T app /app/community healthcheck
```

The health command can fail while the process starts; retry after startup and
then check the public HTTPS health endpoint. Restart PostgreSQL separately only
when necessary; its data stays on the mounted volume. A stopped or unmounted
volume is a deployment failure, never a reason to initialize a replacement.
Directory listings, address ownership, Join Requests and decision/removal
receipts survive app restart. Setup locators, pending negotiation and TURN
issuance metrics are process-local and reset. Hosts publish fresh locators and
interrupted applicants explicitly retry; established gameplay does not depend
on the service process. A green readiness response confirms database/schema and
configured/last-observed TURN issuance, not a fresh TURN connectivity test.

## Runtime capability storage

A World administrator creates and retains a random per-address token locally.
Losing it does not transfer ownership; recovering the token from a private backup
is required. There is no password, Participant account, or public reset endpoint.
Public GET never returns tokens, digests or operation history. Removal retains
ownership and retry receipts so stale publication requests cannot resurrect data.
Directory publication operation receipts remain until operator-managed
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

## Recruitment moderation

Migration `0004_recruitment_controls.sql` adds bounded private installation blocks,
removed-request state, removal receipts and expiring abuse reports. The ordinary
deploy backup runs before migrations; no local World data is stored or changed.

Generate a separate random 32-byte hex operator token and store it in an
operator-only secret file (mode 0600). Put its SHA-256 hex digest into the private
Ansible configuration as `moderation_token_sha256`. Only the digest enters the
service environment. Missing/empty configuration disables operator access.
Never reuse a World administrator token, host lease, applicant proof or TURN key.

Use this bearer over HTTPS to read `/api/v1/operator/reports` (oldest 100
unresolved, within 30 days), then PUT `{ "hidden": true }` to
`/api/v1/operator/worlds/{world}/{address}`. Restoring uses `false`. Review and
actions are explicitly operator initiated; reports do not impose automatic
sanctions. The route changes Directory visibility and new Join Request intake
only; World access and data remain under the GM's World Authority.

The operator receives submitted reasons and target IDs, not applicant proofs,
private recruitment messages, response text or accepted World credentials. Do
not paste report contents into public logs or tickets. Inspect redacted aggregate
service health only. Minute maintenance removes reports and removal receipts
after 30 days and keeps the pre-existing request retention rules.
