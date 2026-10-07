#!/usr/bin/env bash
# On immutable Fedora, extract missing headers without installing host packages.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/linux-env.sh
if pkg-config --exists webkit2gtk-4.1 json-glib-1.0; then exit 0; fi
if ! command -v dnf >/dev/null; then
  echo "Install the Linux development libraries listed in CONTRIBUTORS.md." >&2
  exit 1
fi
mkdir -p target/linux-rpms target/linux-sysroot target/dnf-cache target/dnf-log
env -u LD_LIBRARY_PATH dnf --setopt="cachedir=$PWD/target/dnf-cache" --setopt="logdir=$PWD/target/dnf-log" \
  --disable-repo='*' --enable-repo=fedora --enable-repo=updates \
  download --resolve --arch="$(uname -m)" --destdir="$PWD/target/linux-rpms" \
  webkit2gtk4.1-devel json-glib-devel
env -u LD_LIBRARY_PATH python3 - <<'PY'
from pathlib import Path
import os, subprocess
root = Path('target/linux-sysroot').resolve()
for rpm in Path('target/linux-rpms').glob('*.rpm'):
    if not (rpm.name.endswith('.x86_64.rpm') or rpm.name.endswith('.aarch64.rpm') or rpm.name.endswith('.noarch.rpm')):
        continue
    if '-devel-' not in rpm.name and not rpm.name.startswith('kernel-headers-'):
        continue
    archive = subprocess.run(['rpm2cpio', str(rpm.resolve())], capture_output=True, check=True)
    subprocess.run(['cpio', '-idm', '--quiet'], input=archive.stdout, cwd=root, check=True)
for pc in root.rglob('*.pc'):
    text = pc.read_text()
    if str(root) not in text:
        pc.write_text(text.replace('/usr', str(root / 'usr')))
# Devel RPMs contain linker symlinks; the runtime libraries are on the host.
for link in (root / 'usr/lib64').iterdir():
    if link.is_symlink() and not link.exists():
        runtime = Path('/usr/lib64') / Path(os.readlink(link)).name
        if runtime.exists():
            link.unlink()
            link.symlink_to(runtime)
PY
source scripts/linux-env.sh
pkg-config --modversion webkit2gtk-4.1 json-glib-1.0
