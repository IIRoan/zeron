#!/usr/bin/env python3
"""Check the fork plugin's offline notes snapshot; write it only with --apply."""
import argparse
import hashlib
import json
from pathlib import Path


def expected(root):
    catalog = json.loads((root / 'docs/releases/changelog.json').read_text())
    if (catalog.get('schema_version'), catalog.get('upstream_repository'),
            catalog.get('fork_repository')) != (1, 'zeronsh/zeron', 'IIRoan/zeron'):
        raise SystemExit('Unrecognized fork release catalog')
    for release in catalog['releases']:
        content = [release[key] for key in
                   ['version', 'upstream_commit', 'upstream_notes', 'fork_changes']]
        release['digest'] = hashlib.sha256(json.dumps(content, separators=(',', ':')).encode()).hexdigest()
    text = json.dumps(catalog, separators=(',', ':'))
    delimiter = '===='
    while ']' + delimiter + ']' in text:
        delimiter += '='
    return ('-- Generated from docs/releases/changelog.json; bundled for offline reading.\n'
            f'return ui.json.decode([{delimiter}[{text}]{delimiter}])\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--apply', action='store_true')
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[3]
    destination = root / 'apps/zgx/plugins-fork/linux-workbench/screens/release_data.luau'
    text = expected(root)
    if args.apply:
        destination.write_text(text)
    elif not destination.is_file() or destination.read_text() != text:
        raise SystemExit('Snapshot differs; run apps/zgx/scripts/snapshot-fork-release-notes.py --apply')
    print('Offline fork notes snapshot matches the catalog')


if __name__ == '__main__':
    main()
