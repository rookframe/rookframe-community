#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ -n "$(git status --porcelain --untracked-files=normal)" ]]; then
  echo 'Commit the complete source before deployment.' >&2
  exit 1
fi
mkdir -p dist infra/.local
chmod 700 infra/.local
revision="$(git rev-parse HEAD)"
git archive --format=tar.gz HEAD > dist/source.tar.gz
server_ip="$(tofu -chdir=infra/terraform output -raw server_ipv4)"
data_path="$(tofu -chdir=infra/terraform output -raw data_path)"
printf '[community]\n%s ansible_user=root ansible_ssh_private_key_file=%s/.ssh/id_ed25519\n' "$server_ip" "$HOME" > infra/.local/inventory.ini
ANSIBLE_HOST_KEY_CHECKING=True ansible-playbook -i infra/.local/inventory.ini infra/ansible/deploy.yml --extra-vars "data_path=$data_path release_revision=$revision"
printf 'Deployed %s to %s\n' "$revision" "$server_ip"
