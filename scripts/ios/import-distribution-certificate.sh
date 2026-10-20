#!/usr/bin/env bash
# Import the existing Apple distribution certificate for an ad hoc preview.
set -euo pipefail

if [ -z "${IOS_DISTRIBUTION_P12:-}" ] || [ -z "${IOS_DISTRIBUTION_PASSWORD:-}" ]; then
  echo "Configure both IOS_DISTRIBUTION_P12 and IOS_DISTRIBUTION_PASSWORD." >&2
  exit 1
fi

umask 077
certificate="$RUNNER_TEMP/direct-distribution.p12"
keychain="$RUNNER_TEMP/direct-signing.keychain-db"
trap 'rm -f "$certificate"' EXIT
python3 - <<'PY'
import base64
import os
from pathlib import Path

Path(os.environ['RUNNER_TEMP'], 'direct-distribution.p12').write_bytes(
    base64.b64decode(os.environ['IOS_DISTRIBUTION_P12'], validate=True))
PY

keychain_password=$(openssl rand -hex 32)
echo "::add-mask::$keychain_password"
security create-keychain -p "$keychain_password" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$keychain_password" "$keychain"
security import "$certificate" -k "$keychain" -P "$IOS_DISTRIBUTION_PASSWORD" \
  -T /usr/bin/codesign -T /usr/bin/security
security set-key-partition-list -S apple-tool:,apple:,codesign: -s \
  -k "$keychain_password" "$keychain" >/dev/null
python3 - <<'PY'
import os
from pathlib import Path
import re
import shlex
import subprocess

keychain = str(Path(os.environ['RUNNER_TEMP'], 'direct-signing.keychain-db'))
existing = shlex.split(subprocess.check_output(
    ['security', 'list-keychains', '-d', 'user'], text=True))
subprocess.run(['security', 'list-keychains', '-d', 'user', '-s',
                keychain, *[path for path in existing if path != keychain]], check=True)
identities = subprocess.check_output(
    ['security', 'find-identity', '-v', '-p', 'codesigning', keychain], text=True)
if not re.search(r'\b[A-F0-9]{40}\b', identities):
    raise SystemExit('The imported certificate has no valid code-signing identity.')
PY
echo "Imported the existing Apple distribution certificate."
