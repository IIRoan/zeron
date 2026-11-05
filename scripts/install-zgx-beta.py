#!/usr/bin/env python3
"""Package and install the separate Linux beta. Check-only without --apply.

Build apps/zgx with --features beta and ZERON_BETA_VERSION first. Supply the
native source manifest taken at build time, and an existing engine release
with build.json provenance. No regular Zeron/demo files or services change.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def atomic(path, data, mode=0o600):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, name = tempfile.mkstemp(prefix='.' + path.name, dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as handle:
            handle.write(data)
            os.fchmod(handle.fileno(), mode)
        os.replace(name, path)
    finally:
        if os.path.exists(name):
            os.unlink(name)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--frontend', type=Path, required=True)
    parser.add_argument('--native-source', type=Path, required=True)
    parser.add_argument('--engine-release', type=Path, required=True)
    parser.add_argument('--apply', action='store_true')
    args = parser.parse_args()
    if os.uname().sysname != 'Linux':
        raise SystemExit('This installer is for Linux.')
    frontend = args.frontend.resolve(strict=True)
    engine_release = args.engine_release.resolve(strict=True)
    engine = engine_release / 'bin/zeron-engine'
    provenance = json.loads((engine_release / 'build.json').read_text())
    native = json.loads(args.native_source.read_text())
    actual = {str(p.relative_to(ROOT)): sha(p) for p in (ROOT / 'apps/zgx/src').rglob('*.rs')}
    if native != actual:
        raise SystemExit('Native source changed since the supplied build manifest. Rebuild first.')
    for name in ['crates/engine/src/plugins/mod.rs', 'crates/engine/src/plugins/tests.rs']:
        if provenance['source_files'].get(name) != sha(ROOT / name):
            raise SystemExit(f'Engine provenance differs: {name}. Rebuild the engine first.')
    if sha(engine) != provenance['binaries']['zeron-engine']:
        raise SystemExit('Engine checksum differs from its build receipt.')
    # --version is read-only, with an explicit beta data/port boundary even
    # when this installer is invoked by the regular Zeron app.
    env = dict(os.environ, ZERON_DATA_DIR=str(Path.home() / '.local/share/zeron-beta/profile/engine'), ZERON_IPC_PORT='27946')
    version = subprocess.check_output([str(frontend), '--version'], env=env, text=True).strip()
    if not version.startswith('zeron ') or '-beta.' not in version:
        raise SystemExit(f'Expected a beta frontend: {version}')
    version = version.removeprefix('zeron ')
    for binary in [frontend, engine]:
        data = binary.read_bytes()
        if b'staging.zeron.sh' in data or b'client_01KWD0E9SYPV2WE4QN9TP9DP0W' in data:
            raise SystemExit(f'{binary.name} contains staging account configuration.')
    paths = subprocess.check_output(['git', 'ls-files', '-m', '-o', '--exclude-standard', '-z'], cwd=ROOT).decode().split('\0')
    sources = {p: sha(ROOT / p) for p in sorted(set(paths)) if p and (ROOT / p).is_file()}
    binaries = {'zeron-beta': sha(frontend), 'zeron-engine': sha(engine)}
    fingerprint = hashlib.sha256(json.dumps({'sources': sources, 'binaries': binaries}, sort_keys=True).encode()).hexdigest()
    base = Path.home() / '.local/share/zeron-beta'
    release = base / 'releases' / f'{version}-{fingerprint[:10]}'
    plugins = ['linux-workbench', 'project-services', 'zeron-git']
    plan = {'app': 'Zeron Beta', 'version': version, 'release': str(release),
            'profile': str(base / 'profile'), 'port': 27946,
            'plugins': plugins, 'stock_plugins_off': ['git', 'github'],
            'shared_agent_logins': True, 'automatic_updates': False,
            'binaries': binaries, 'source_fingerprint': fingerprint, 'applied': args.apply}
    print(json.dumps(plan, indent=2))
    if not args.apply:
        return
    base.mkdir(parents=True, exist_ok=True, mode=0o700)
    (base / 'releases').mkdir(exist_ok=True, mode=0o700)
    if not release.exists():
        temporary = Path(tempfile.mkdtemp(prefix='.beta-', dir=base / 'releases'))
        try:
            (temporary / 'bin').mkdir()
            shutil.copy2(frontend, temporary / 'bin/zeron-beta')
            shutil.copy2(engine, temporary / 'bin/zeron-engine')
            resources = temporary / 'Resources'
            shutil.copytree(ROOT / 'apps/zgx/ui', resources / 'ui', ignore=shutil.ignore_patterns('.zgx', 'tests', '.lock', '.DS_Store'))
            shutil.copytree(ROOT / 'apps/zgx/plugins-builtin', resources / 'plugins-builtin', ignore=shutil.ignore_patterns('.zgx', '.DS_Store'))
            subprocess.run(['python3', str(ROOT / 'apps/zgx/scripts/bundle-fork-plugins.py'), '--resources', str(resources), '--apply'], check=True, stdout=subprocess.DEVNULL)
            shutil.copy2(ROOT / 'apps/zgx/plugins-fork/PORTING.md', resources / 'FORK-PORTING.md')
            for name in ['LICENSE', 'THIRD_PARTY_NOTICES.md', 'UPSTREAM-README.md']:
                shutil.copy2(engine_release / name, temporary / name)
            shutil.copytree(engine_release / 'licenses', temporary / 'licenses')
            shutil.copy2(engine_release / 'zeron-zgx-demo.png', temporary / 'zeron-beta.png')
            patch = subprocess.check_output(['git', 'diff', '--binary', 'HEAD'], cwd=ROOT)
            (temporary / 'source-changes.patch').write_bytes(patch)
            # Untracked plugin source is included in Resources, and its
            # complete hashes are retained alongside tracked source changes.
            (temporary / 'source-files.json').write_text(json.dumps(sources, indent=2) + '\n')
            metadata = dict(plan, built_at=datetime.now(timezone.utc).isoformat(),
                            engine_reused_from=str(engine_release), source_files=sources,
                            source_revision=subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
                            scripted_agent=False, native_features=['beta'])
            (temporary / 'build.json').write_text(json.dumps(metadata, indent=2) + '\n')
            os.rename(temporary, release)
        finally:
            if temporary.exists():
                shutil.rmtree(temporary)
    elif json.loads((release / 'build.json').read_text())['binaries'] != binaries:
        raise SystemExit('Existing immutable release differs; left untouched.')
    profile = base / 'profile'
    for folder in ['engine/plugins', 'config', 'data', 'cache', 'logs', 'projects']:
        (profile / folder).mkdir(parents=True, exist_ok=True, mode=0o700)
    state = profile / 'engine/plugins/.state.json'
    # Only seed a new profile. Subsequent installs preserve plugin switches.
    if not state.exists():
        decisions = {p: {'allowed': True, 'off': True} for p in ['git', 'github']}
        atomic(state, (json.dumps({'own': [], 'decisions': decisions}, indent=2) + '\n').encode())
    current = base / 'current'
    next_link = base / '.current-next'
    if next_link.is_symlink():
        next_link.unlink()
    next_link.symlink_to(release)
    os.replace(next_link, current)
    atomic(Path.home() / '.local/bin/zeron-beta', (ROOT / 'scripts/launch-zgx-beta.py').read_bytes(), 0o755)
    launcher = Path.home() / '.local/bin/zeron-beta'
    desktop = f'''[Desktop Entry]
Type=Application
Name=Zeron Beta
Comment=Separate Zeron beta with the Linux workbench, Git and Services plugins
Exec={launcher}
TryExec={launcher}
Icon={current}/zeron-beta.png
Terminal=false
Categories=Development;
StartupNotify=true
StartupWMClass=sh.zeron.beta
Keywords=Zeron;Beta;zgx;Plugins;
'''
    atomic(Path.home() / '.local/share/applications/sh.zeron.beta.desktop', desktop.encode(), 0o644)
    print('Installed Zeron Beta. Launch: zeron-beta')


if __name__ == '__main__':
    main()
