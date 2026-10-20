#!/usr/bin/env python3
"""Prepare and verify an ad hoc IPA for the registered devices in both profiles."""
import base64
import datetime
import hashlib
import os
from pathlib import Path
import plistlib
import re
import subprocess
import sys
import tempfile
import zipfile


def decode_profile(content):
    result = subprocess.run(
        ['security', 'cms', '-D'], input=content, capture_output=True)
    if result.returncode:
        raise ValueError('Apple provisioning profile could not be decoded')
    return plistlib.loads(result.stdout)


def validate_profile(profile, bundle, team, now=None):
    now = now or datetime.datetime.now(datetime.timezone.utc)
    expiry = profile['ExpirationDate'].replace(tzinfo=datetime.timezone.utc)
    if expiry <= now:
        raise ValueError('Provisioning profile has expired')
    if team not in profile.get('TeamIdentifier', []):
        raise ValueError('Provisioning profile belongs to another Apple team')
    entitlements = profile['Entitlements']
    prefixes = profile.get('ApplicationIdentifierPrefix', [])
    if entitlements.get('application-identifier') not in [f'{p}.{bundle}' for p in prefixes]:
        raise ValueError('Provisioning profile does not match the exact app bundle ID')
    if (entitlements.get('get-task-allow', True)
            or profile.get('ProvisionsAllDevices')
            or not profile.get('ProvisionedDevices')):
        raise ValueError('An ad hoc distribution profile with registered devices is required')
    certificates = {hashlib.sha1(c).hexdigest().upper() for c in profile['DeveloperCertificates']}
    return set(profile['ProvisionedDevices']), certificates


def private_write(path, content):
    path.parent.mkdir(parents=True, exist_ok=True)
    with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'wb') as output:
        output.write(content)


def prepare():
    required = ['IOS_BUNDLE_ID', 'IOS_TEAM_ID', 'IOS_APP_PROFILE', 'IOS_ACTIVITY_PROFILE']
    for name in required:
        if not os.environ.get(name):
            raise ValueError(f'Configure {name} before requesting a preview build')
    temp = Path(os.environ['RUNNER_TEMP'])
    bundle, team = os.environ['IOS_BUNDLE_ID'], os.environ['IOS_TEAM_ID']
    profiles, devices, certificates = {}, None, None
    env = {}
    installed = []
    manifest = temp / 'direct-installed-profiles.txt'
    private_write(manifest, b'')
    for secret, identifier, variable in [
        ('IOS_APP_PROFILE', bundle, 'ZERON_APP_PROFILE'),
        ('IOS_ACTIVITY_PROFILE', bundle + '.LiveActivity', 'ZERON_ACTIVITY_PROFILE'),
    ]:
        content = base64.b64decode(os.environ[secret], validate=True)
        profile = decode_profile(content)
        current_devices, current_certificates = validate_profile(profile, identifier, team)
        if devices is not None and devices != current_devices:
            raise ValueError('The app and extension profiles must include the same devices')
        devices = current_devices
        certificates = current_certificates if certificates is None else certificates & current_certificates
        uuid = profile['UUID']
        if not re.fullmatch(r'[A-Fa-f0-9-]{36}', uuid):
            raise ValueError('Invalid provisioning profile UUID')
        # Mask device identifiers before any Xcode output reaches Actions logs.
        for udid in sorted(current_devices):
            print(f'::add-mask::{udid}', flush=True)
        destination = Path.home() / 'Library/MobileDevice/Provisioning Profiles' / f'{uuid}.mobileprovision'
        if not destination.exists():
            private_write(destination, content)
            installed.append(str(destination))
            manifest.write_text('\n'.join(installed) + '\n')
        elif destination.read_bytes() != content:
            raise ValueError('An installed profile UUID has different contents')
        profiles[identifier] = uuid
        env[variable] = uuid
    identities = subprocess.check_output(
        ['security', 'find-identity', '-v', '-p', 'codesigning',
         str(temp / 'direct-signing.keychain-db')], text=True)
    available = set(re.findall(r'\b[A-F0-9]{40}\b', identities))
    matches = sorted(available & certificates)
    if not matches:
        raise ValueError('The imported signing certificate does not match both profiles')
    env['IOS_SIGNING_IDENTITY'] = matches[0]
    with open(os.environ['GITHUB_ENV'], 'a') as output:
        for key, value in env.items():
            output.write(f'{key}={value}\n')
    private_write(temp / 'direct-preview-export.plist', plistlib.dumps({
        'method': 'release-testing', 'destination': 'export', 'signingStyle': 'manual',
        'teamID': team, 'signingCertificate': matches[0],
        'provisioningProfiles': profiles, 'manageAppVersionAndBuildNumber': False,
        'thinning': '<none>',
    }))
    print(f'Prepared ad hoc signing for {len(devices)} registered devices.')


def verify():
    temp = Path(os.environ['RUNNER_TEMP'])
    files = list((temp / 'PreviewExport').glob('*.ipa'))
    if len(files) != 1:
        raise ValueError('Expected one exported IPA')
    bundle, team = os.environ['IOS_BUNDLE_ID'], os.environ['IOS_TEAM_ID']
    expected = {bundle, bundle + '.LiveActivity'}
    seen, devices = set(), None
    with zipfile.ZipFile(files[0]) as archive:
        for entry in archive.namelist():
            if not entry.endswith('/embedded.mobileprovision'):
                continue
            parent = entry.rsplit('/', 1)[0]
            info = plistlib.loads(archive.read(parent + '/Info.plist'))
            identifier = info['CFBundleIdentifier']
            if identifier not in expected or identifier in seen:
                raise ValueError('Unexpected signed bundle in the IPA')
            profile = decode_profile(archive.read(entry))
            current, certificates = validate_profile(profile, identifier, team)
            if os.environ['IOS_SIGNING_IDENTITY'] not in certificates:
                raise ValueError('Exported profile uses another signing certificate')
            if devices is not None and devices != current:
                raise ValueError('Exported app and extension support different devices')
            devices = current
            seen.add(identifier)
    if seen != expected:
        raise ValueError('The exported app or Live Activity profile is missing')
    with tempfile.TemporaryDirectory(dir=temp, prefix='verify-preview-') as directory:
        subprocess.run(['ditto', '-x', '-k', str(files[0]), directory], check=True)
        app = Path(directory) / 'Payload/Zeron.app'
        subprocess.run(['codesign', '--verify', '--deep', '--strict', str(app)], check=True)
    files[0].rename(temp / 'Zeron-preview.ipa')
    print(f'Verified the preview IPA profiles for {len(devices)} registered devices.')


def cleanup():
    temp = Path(os.environ['RUNNER_TEMP'])
    manifest = temp / 'direct-installed-profiles.txt'
    if manifest.exists():
        directory = Path.home() / 'Library/MobileDevice/Provisioning Profiles'
        for value in manifest.read_text().splitlines():
            path = Path(value)
            if path.parent == directory and path.suffix == '.mobileprovision':
                path.unlink(missing_ok=True)
        manifest.unlink()
    (temp / 'direct-preview-export.plist').unlink(missing_ok=True)


if __name__ == '__main__':
    try:
        {'prepare': prepare, 'verify': verify, 'cleanup': cleanup}[sys.argv[1]]()
    except (ValueError, KeyError) as error:
        raise SystemExit(str(error)) from None
