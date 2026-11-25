#!/usr/bin/env python3
"""Package and install the Linux fork's Zeron. Check-only without --prepare
(build the release folder only) or --apply (install it).

Build these from this commit first:
  apps/zgx:  cargo build --release --features beta          (the app)
  root:      cargo build --release -p zeron --bin zeron-engine
             cargo build --release -p zeron-rpc --bin zeron-remote
             scripts/build-sandbox-helper.sh                (zeron-sandbox)
and take the app's native source manifest at build time (apps/zgx/src's
hashes). The release is laid out as upstream's Linux tarball is
(scripts/package-linux.sh): the app as `zeron`, `ui/` and `plugins-builtin/`
beside it, and `zeron-engine`, which runs its engine commands (`zeron
headless` for zeron-host.service, `zeron mcp`).

It goes into ~/.local/share/zeron-linux-fork/releases/, `current` switches
to it, ~/.local/bin/zeron becomes scripts/launch-zeron.py, and the desktop
entry is Zeron's. Running processes are left alone: restart zeron-host.service
(the engine), zeron-remote.service and the window to run the new release.
From Zeron Beta: its window state (~/.local/share/zeron-beta) is copied to
the app's (~/.local/share/zeron) where that has none, and its launcher and
desktop entry go (kept in the backup).
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
HOME = Path.home()
BASE = HOME / '.local/share/zeron-linux-fork'
BETA = HOME / '.local/share/zeron-beta'
APP_DATA = HOME / '.local/share/zeron'
# What Zeron Beta kept for its window (zgx's app data, keyed by app id).
WINDOW_STATE = ['script-state', 'storage', 'localstorage', 'hsts-storage.sqlite',
                'databases', 'mediakeys', 'project-icons', 'wallpaper-restore']
# The previous release's local files the services run (not in the repository).
CARRIED = ['configure-muute-service.py', 'muute-database-service.sh', 'muute-redis.compose.yaml']
PLUGINS = ['linux-workbench', 'project-services', 'project-sandbox', 'windows-vm', 'zeron-git']


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


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


def clean_env():
    return {k: v for k, v in os.environ.items() if not k.startswith(('ZERON_', 'CLAUDE', 'ANTHROPIC_'))}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--app', type=Path, required=True, help='apps/zgx release build (zeron-zgx)')
    parser.add_argument('--native-source', type=Path, required=True)
    parser.add_argument('--engine', type=Path, required=True, help='zeron-engine built from this commit')
    parser.add_argument('--remote', type=Path, required=True, help='zeron-remote built from this commit')
    parser.add_argument('--sandbox-helper', type=Path, required=True, help='zeron-sandbox from scripts/build-sandbox-helper.sh')
    parser.add_argument('--label', default='fork', help='a word in the release folder name')
    parser.add_argument('--prepare', action='store_true', help='only build the release folder (to try it first)')
    parser.add_argument('--apply', action='store_true')
    args = parser.parse_args()
    if os.uname().sysname != 'Linux':
        raise SystemExit('This installer is for Linux.')
    app, engine, remote, helper = (p.resolve(strict=True) for p in (args.app, args.engine, args.remote, args.sandbox_helper))
    if not re.search(r'static(-pie|ally) linked', subprocess.check_output(['file', str(helper)], text=True)):
        raise SystemExit(f'{helper} is not the static sandbox helper.')
    native = json.loads(args.native_source.read_text())
    actual = {str(p.relative_to(ROOT)): sha(p) for p in (ROOT / 'apps/zgx/src').rglob('*.rs')}
    if native != actual:
        raise SystemExit('The app\'s source changed since the supplied build manifest. Rebuild first.')
    head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
    build_id = subprocess.check_output([str(engine), '--build-id'], env=clean_env(), text=True).strip()
    if not build_id.startswith(head[:12]):
        raise SystemExit(f'The engine was built from {build_id}, not this checkout ({head[:12]}). Rebuild it.')
    version = subprocess.check_output([str(app), '--version'], env=clean_env(), text=True).strip()
    if not re.fullmatch(r'zeron \d+\.\d+\.\d+', version):
        raise SystemExit(f'Expected the app\'s release version (zeron X.Y.Z): {version}')
    version = version.removeprefix('zeron ')
    engine_version = subprocess.check_output([str(engine), '--version'], env=clean_env(), text=True).strip()
    if engine_version != f'zeron-engine {version}':
        raise SystemExit(f'The engine is {engine_version}, the app {version}.')
    subprocess.run([str(remote), '--help'], env=clean_env(), check=True, stdout=subprocess.DEVNULL)
    for binary in [app, engine]:
        data = binary.read_bytes()
        if b'staging.zeron.sh' in data or b'client_01KWD0E9SYPV2WE4QN9TP9DP0W' in data:
            raise SystemExit(f'{binary.name} contains staging account configuration.')
    paths = subprocess.check_output(['git', 'ls-files', '-m', '-o', '--exclude-standard', '-z'], cwd=ROOT).decode().split('\0')
    sources = {p: sha(ROOT / p) for p in sorted(set(paths)) if p and (ROOT / p).is_file()}
    binaries = {'zeron': sha(app), 'zeron-engine': sha(engine), 'zeron-remote': sha(remote), 'zeron-sandbox': sha(helper)}
    fingerprint = hashlib.sha256(json.dumps({'sources': sources, 'binaries': binaries}, sort_keys=True).encode()).hexdigest()
    release = BASE / 'releases' / f'{version}-{args.label}-{head[:8]}-{fingerprint[:8]}'
    previous = (BASE / 'current').resolve() if (BASE / 'current').exists() else None
    plan = {'app': 'Zeron', 'version': version, 'release': str(release), 'previous': str(previous),
            'profile': str(HOME / '.zeron'), 'port': 27654, 'engine': 'zeron-host.service',
            'plugins': PLUGINS, 'stock_plugins_off': ['git', 'github'], 'automatic_updates': False,
            'binaries': binaries, 'engine_build_id': build_id, 'source_revision': head,
            'source_fingerprint': fingerprint, 'applied': args.apply}
    print(json.dumps(plan, indent=2))
    if not args.apply and not args.prepare:
        return
    (BASE / 'releases').mkdir(parents=True, exist_ok=True, mode=0o700)
    if not release.exists():
        temporary = Path(tempfile.mkdtemp(prefix='.release-', dir=BASE / 'releases'))
        try:
            for name, source in [('zeron', app), ('zeron-engine', engine), ('zeron-remote', remote), ('zeron-sandbox', helper)]:
                shutil.copy2(source, temporary / name)
                (temporary / name).chmod(0o755)
            shutil.copytree(ROOT / 'apps/zgx/ui', temporary / 'ui', ignore=shutil.ignore_patterns('.zgx', 'tests', '.lock', '.DS_Store'))
            shutil.copytree(ROOT / 'apps/zgx/plugins-builtin', temporary / 'plugins-builtin', ignore=shutil.ignore_patterns('.zgx', '.DS_Store'))
            subprocess.run(['python3', str(ROOT / 'apps/zgx/scripts/bundle-fork-plugins.py'), '--resources', str(temporary), '--apply'],
                           check=True, stdout=subprocess.DEVNULL)
            shutil.copy2(ROOT / 'apps/zgx/plugins-fork/PORTING.md', temporary / 'FORK-PORTING.md')
            for name in ['LICENSE', 'THIRD_PARTY_NOTICES.md']:
                shutil.copy2(ROOT / name, temporary / name)
            (temporary / 'licenses/fonts').mkdir(parents=True)
            for licence in [*(ROOT / 'assets/fonts/licenses').iterdir(), *(ROOT / 'apps/zgx/fonts').glob('*-OFL.txt')]:
                shutil.copy2(licence, temporary / 'licenses/fonts' / licence.name)
            shutil.copy2(ROOT / 'dist/zeron.png', temporary / 'zeron.png')
            for name in CARRIED:
                if previous and (previous / name).is_file():
                    shutil.copy2(previous / name, temporary / name)
            (temporary / 'source-changes.patch').write_bytes(subprocess.check_output(['git', 'diff', '--binary', 'HEAD'], cwd=ROOT))
            (temporary / 'source-files.json').write_text(json.dumps(sources, indent=2) + '\n')
            (temporary / 'rollback.txt').write_text(f'Previous release: {previous}\n'
                                                    'Point `current` back at it, then restart zeron-host.service.\n')
            metadata = dict(plan, built_at=datetime.now(timezone.utc).isoformat(), source_files=sources,
                            carried_from_previous=[n for n in CARRIED if (temporary / n).exists()],
                            native_features=['beta'], restart_required=['zeron-host.service', 'zeron-remote.service', 'the window'])
            (temporary / 'build.json').write_text(json.dumps(metadata, indent=2) + '\n')
            os.rename(temporary, release)
        finally:
            if temporary.exists():
                shutil.rmtree(temporary)
    elif json.loads((release / 'build.json').read_text())['binaries'] != binaries:
        raise SystemExit('Existing immutable release differs; left untouched.')
    if not args.apply:
        print(f'Prepared {release}; nothing else changed. Install it with --apply.')
        return

    # What changes outside the release, kept first.
    stamp = datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')
    backup = BASE / 'backups' / f'{stamp}-{args.label}'
    backup.mkdir(parents=True, mode=0o700)
    launcher = HOME / '.local/bin/zeron'
    entries = HOME / '.local/share/applications'
    for path in [launcher, HOME / '.local/bin/zeron-beta', entries / 'zeron.desktop', entries / 'sh.zeron.beta.desktop']:
        if path.is_file():
            shutil.copy2(path, backup / path.name)
    (backup / 'previous-current.txt').write_text(f'{previous}\n')

    # The profile: Zeron Git and the stock Git and GitHub plugins aren't
    # both on (a profile without decisions only).
    state = HOME / '.zeron/plugins/.state.json'
    if (HOME / '.zeron').is_dir() and not state.exists():
        decisions = {p: {'allowed': True, 'off': True} for p in ['git', 'github']}
        atomic(state, (json.dumps({'own': [], 'decisions': decisions}, indent=2) + '\n').encode())
    # Zeron Beta's window state, where the app has none of its own yet.
    copied = []
    for name in WINDOW_STATE:
        source, target = BETA / name, APP_DATA / name
        if source.exists() and not target.exists():
            APP_DATA.mkdir(parents=True, exist_ok=True, mode=0o700)
            if source.is_dir():
                shutil.copytree(source, target, symlinks=True)
            else:
                shutil.copy2(source, target)
            copied.append(name)

    current = BASE / 'current'
    following = BASE / '.current-next'
    if following.is_symlink():
        following.unlink()
    following.symlink_to(release)
    os.replace(following, current)
    script = (ROOT / 'scripts/launch-zeron.py').read_text()
    assert script.count('\nINSTALLED = None\n') == 1
    script = script.replace('\nINSTALLED = None\n', f'\nINSTALLED = {str(BASE)!r}\n')
    atomic(launcher, script.encode(), 0o755)
    desktop = f'''[Desktop Entry]
Type=Application
Name=Zeron
GenericName=Coding Agent Controller
Comment=Multi-device controller for coding agents
Exec={launcher} %u
TryExec={launcher}
Icon={current}/zeron.png
Terminal=false
StartupNotify=true
Categories=Development;Utility;
Keywords=agent;claude;codex;ai;coding;
StartupWMClass=sh.zeron.app
MimeType=x-scheme-handler/zeron;
'''
    atomic(entries / 'zeron.desktop', desktop.encode(), 0o644)
    for path in [HOME / '.local/bin/zeron-beta', entries / 'sh.zeron.beta.desktop']:
        path.unlink(missing_ok=True)
    subprocess.run(['update-desktop-database', str(entries)], check=False,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    print(json.dumps({'installed': str(release), 'previous': str(previous), 'backup': str(backup),
                      'window_state_copied': copied}, indent=2))
    print('Installed Zeron. Restart zeron-host.service and zeron-remote.service, then open Zeron.')


if __name__ == '__main__':
    main()
