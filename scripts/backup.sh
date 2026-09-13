#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
server_ip="$(tofu -chdir=infra/terraform output -raw server_ipv4)"
data_path="$(tofu -chdir=infra/terraform output -raw data_path)"
mkdir -p infra/.local/backups
chmod 700 infra/.local/backups
ssh "root@$server_ip" /usr/local/sbin/community-backup
rsync -av "root@$server_ip:$data_path/backups/" infra/.local/backups/
