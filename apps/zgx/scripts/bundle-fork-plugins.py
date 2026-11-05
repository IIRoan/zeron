#!/usr/bin/env python3
"""Bundle independent fork plugins into a new demo's Resources directory.

Check-only unless --apply is given. Existing, different plugin folders are
left untouched; build a new release directory to retain a rollback.
"""
import argparse
import hashlib
import json
import shutil
from pathlib import Path


def files(directory):
    return {
        str(path.relative_to(directory)): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in directory.rglob('*') if path.is_file()
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--resources', type=Path, required=True)
    parser.add_argument('--apply', action='store_true')
    args = parser.parse_args()
    source = Path(__file__).resolve().parents[1] / 'plugins-fork'
    planned = []
    for folder in sorted(source.iterdir()):
        manifest = folder / 'zeron-plugin.json'
        if not manifest.is_file():
            continue
        plugin = json.loads(manifest.read_text())
        if plugin['id'] != folder.name:
            raise SystemExit(f'Plugin id differs from its folder: {folder.name}')
        destination = args.resources / 'plugins-builtin' / plugin['id']
        changed = not destination.exists() or files(folder) != files(destination)
        if changed and destination.exists():
            raise SystemExit(f'Use a new release directory; preserve {destination}')
        planned.append({'id': plugin['id'], 'name': plugin['name'], 'copy': changed})
        if args.apply and changed:
            shutil.copytree(folder, destination)
    print(json.dumps({'applied': args.apply, 'plugins': planned,
                      'disable_stock_plugins': ['git', 'github']}, indent=2))


if __name__ == '__main__':
    main()
