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
values are never logged. `/api/v1/health` checks the database/schema;
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
