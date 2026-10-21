#!/usr/bin/env python3
"""Prepare a private Linux x86_64 WPE runtime and its build headers.

Checks only by default. --apply downloads checksum-pinned distribution packages
and atomically creates --output. Nothing is installed into the operating system.
Put the result beside the Zeron binary as browser-runtime, or set
ZERON_BROWSER_WPE_RUNTIME to its absolute path. See docs/reference/linux-browser.md.
"""
import argparse
import hashlib
import io
import json
from pathlib import Path
import platform
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request

BASE = 'https://deb.debian.org/debian/pool/main/'
PACKAGES = [
    ('w/wpewebkit/libwpewebkit-2.0-1_2.54.1-1_amd64.deb', 'c1c8e5590d3dcf1f480952890122aa5e39768ebb279017013b915631d1e291bd'),
    ('w/wpewebkit/libwpewebkit-2.0-dev_2.54.1-1_amd64.deb', 'fe142768553f71a36f279db6012c8d525de9b4c370d583e6239349c2b10cfac7'),
    ('i/icu/libicu78_78.3-2_amd64.deb', 'dc918ea6cbe7bb6feea43aac3920a86a29dcc5faf4c56d8f0948408f9076d866'),
    ('libx/libxml2/libxml2-16_2.15.4+dfsg-1_amd64.deb', '36f043b5014adf26ff722f693361d1d677088d1eb384fe4522ad514b52053c88'),
    ('libw/libwpe/libwpe-1.0-1_1.16.3-2_amd64.deb', '2fc89dd4a3ed2bcd970917d2884ff039669a397effe0169b57c9e53cf1bbe354'),
    ('libw/libwpe/libwpe-1.0-dev_1.16.3-2_amd64.deb', '1da5bbf3f2b865ece6a30567dfd20f097ba46da8b330dfd9bda0ec8de621a51e'),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, default=Path('target/browser-runtime'))
    parser.add_argument('--cache', type=Path, default=Path('target/browser-runtime-downloads'))
    parser.add_argument('--apply', action='store_true')
    args = parser.parse_args()
    if platform.system() != 'Linux' or platform.machine() != 'x86_64':
        parser.error('this private runtime currently supports Linux x86_64')
    output = args.output.resolve()
    if any(c.isspace() for c in str(output)):
        parser.error('pkg-config build paths must not contain whitespace')
    if output.exists():
        manifest = output / 'manifest.json'
        if (manifest.is_file() and json.loads(manifest.read_text()).get('webkitVersion') == '2.54.1'
                and json.loads(manifest.read_text()).get('packages') ==
                [{'url': BASE + path, 'sha256': digest} for path, digest in PACKAGES]):
            print(f'WPE runtime already prepared: {output}')
            return
        parser.error(f'output exists and is not this runtime: {output}')
    if not args.apply:
        print(f'Check only: would prepare WPE WebKit 2.54.1 in {output}')
        print('Use --apply to download and extract the pinned packages. System libraries remain unchanged.')
        return
    args.cache.mkdir(parents=True, exist_ok=True)
    output.parent.mkdir(parents=True, exist_ok=True)
    pending = Path(tempfile.mkdtemp(prefix=output.name + '.pending-', dir=output.parent))
    try:
        for relative, digest in PACKAGES:
            package = args.cache / Path(relative).name
            if not package.is_file():
                urllib.request.urlretrieve(BASE + relative, package)
            if hashlib.sha256(package.read_bytes()).hexdigest() != digest:
                raise ValueError(f'Package checksum mismatch: {package}; remove it before retrying')
            members = subprocess.check_output(['ar', 't', str(package)], text=True).splitlines()
            data = next(member for member in members if member.startswith('data.tar'))
            archive = subprocess.check_output(['ar', 'p', str(package), data])
            with tarfile.open(fileobj=io.BytesIO(archive)) as contents:
                contents.extractall(pending, filter='data')
        config = pending / 'pkgconfig'
        config.mkdir()
        (config / 'wpe-webkit-2.0.pc').write_text('''prefix=${pcfiledir}/..
Name: Private WPE WebKit
Description: Isolated WPE runtime build headers
Version: 2.54.1
Requires: glib-2.0 gobject-2.0 libsoup-3.0
Cflags: -I${prefix}/usr/include/wpe-webkit-2.0 -I${prefix}/usr/include/wpe-webkit-2.0/wpe-platform -I${prefix}/usr/include/wpe-1.0
Libs: -L${prefix}/usr/lib/x86_64-linux-gnu -lWPEWebKit-2.0 -Wl,-rpath-link,${prefix}/usr/lib/x86_64-linux-gnu
''')
        (pending / 'manifest.json').write_text(json.dumps({
            'format': 1, 'architecture': 'x86_64', 'webkitVersion': '2.54.1',
            'packages': [{'url': BASE + path, 'sha256': digest} for path, digest in PACKAGES],
        }, indent=2) + '\n')
        pending.rename(output)
        print(f'Prepared WPE runtime: {output}')
        print(f'Build with PKG_CONFIG_PATH={output}/pkgconfig:$PKG_CONFIG_PATH')
    finally:
        if pending.exists():
            shutil.rmtree(pending)


if __name__ == '__main__':
    main()
