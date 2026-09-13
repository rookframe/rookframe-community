#!/usr/bin/env python3
"""Check public Git objects without printing credential values."""
from pathlib import Path
import os
import subprocess

root = Path(__file__).resolve().parents[1]
secrets = [v.encode() for k, v in os.environ.items()
           if any(word in k.upper() for word in ('TOKEN', 'PASSWORD', 'SECRET')) and len(v) >= 12]
config = root / 'infra/.local/config.yml'
if config.exists():
    for line in config.read_text().splitlines():
        key, _, value = line.partition(':')
        if any(word in key.lower() for word in ('password', 'token', 'secret')):
            value = value.strip().strip('"').strip("'")
            if len(value) >= 12:
                secrets.append(value.encode())

def git(*args):
    return subprocess.check_output(['git', *args], cwd=root)

for revision in git('rev-list', '--all').decode().splitlines():
    names = git('ls-tree', '-r', '--name-only', revision).decode().splitlines()
    for name in names:
        if ('/.local/' in name or '.tfstate' in name or name.endswith(('.tfplan', '.tfvars'))
                or Path(name).name in ('.env', 'id_rsa', 'id_ed25519')):
            raise SystemExit(f'Private file committed: {name}')
    archive = git('archive', revision)
    if any(value in archive for value in secrets):
        raise SystemExit('Credential value detected in committed source; publication stopped.')
print('PASS: committed history contains no known credential values or private configuration/state files.')
