#!/usr/bin/env python3
"""Internal TestFlight signing for a fork. Secrets stay in the runner temp dir."""
import json
import os
from pathlib import Path
import plistlib
import subprocess
import sys
import time
import urllib.parse
import urllib.request

import jwt

TEMP = Path(os.environ['RUNNER_TEMP'])
KEY = TEMP / 'direct-asc-key.p8'


def api(path, data=None):
    now = int(time.time())
    token = jwt.encode({'iss': os.environ['AC_API_ISSUER_ID'], 'iat': now, 'exp': now + 600, 'aud': 'appstoreconnect-v1'},
                       KEY.read_bytes(), algorithm='ES256', headers={'kid': os.environ['AC_API_KEY_ID']})
    request = urllib.request.Request('https://api.appstoreconnect.apple.com/v1/' + path,
                                     data=None if data is None else json.dumps(data).encode(),
                                     headers={'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=60) as response:
        return json.load(response)


def environment(**values):
    with open(os.environ['GITHUB_ENV'], 'a') as output:
        for key, value in values.items():
            if '\n' in str(value) or '\r' in str(value):
                raise ValueError('Invalid environment value')
            output.write(f'{key}={value}\n')


def prepare():
    for name in ['AC_API_KEY_P8', 'AC_API_KEY_ID', 'AC_API_ISSUER_ID', 'IOS_TEAM_ID', 'IOS_BUNDLE_ID']:
        if not os.environ.get(name):
            raise SystemExit(f'Configure {name} before requesting a TestFlight upload.')
    with os.fdopen(os.open(KEY, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'w') as output:
        output.write(os.environ['AC_API_KEY_P8'])
    bundle = os.environ['IOS_BUNDLE_ID']
    apps = api('apps?' + urllib.parse.urlencode({'filter[bundleId]': bundle, 'limit': 10}))['data']
    if len(apps) != 1:
        raise SystemExit(f'Create the App Store Connect app for {bundle} first.')
    app = apps[0]['id']
    builds = api('builds?' + urllib.parse.urlencode({'filter[app]': app, 'sort': '-uploadedDate', 'limit': 200}))['data']
    number = max([int(b['attributes']['version']) for b in builds if b['attributes']['version'].isdigit()] + [0]) + 1
    # Preserve the existing app's push entitlement, even while direct-mode
    # push is unavailable. Xcode creates distribution provisioning profiles.
    ids = api('bundleIds?' + urllib.parse.urlencode({'filter[identifier]': bundle}))['data']
    if len(ids) != 1:
        raise SystemExit(f'Register the App ID {bundle} in your Apple Developer account first.')
    identifier = ids[0]['id']
    caps = api(f'bundleIds/{identifier}/bundleIdCapabilities')['data']
    if not any(c['attributes']['capabilityType'] == 'PUSH_NOTIFICATIONS' for c in caps):
        api('bundleIdCapabilities', {'data': {'type': 'bundleIdCapabilities', 'attributes': {'capabilityType': 'PUSH_NOTIFICATIONS'},
            'relationships': {'bundleId': {'data': {'type': 'bundleIds', 'id': identifier}}}}})
    environment(BUILD_NUMBER=number, ASC_APP_ID=app)
    print(f'Preparing internal TestFlight build {number} for {bundle}.')


def upload():
    archive = TEMP / 'Zeron.xcarchive'
    app = archive / 'Products/Applications/Zeron.app'
    subprocess.run(['codesign', '--force', '--sign', '-', '--entitlements', 'apps/ios/Zeron.entitlements', str(app)], check=True)
    options = TEMP / 'direct-export.plist'
    with options.open('wb') as output:
        plistlib.dump({'method': 'app-store-connect', 'destination': 'upload', 'signingStyle': 'automatic',
                      'teamID': os.environ['IOS_TEAM_ID'], 'manageAppVersionAndBuildNumber': False,
                      'testFlightInternalTestingOnly': True}, output)
    subprocess.run(['xcodebuild', '-exportArchive', '-archivePath', str(archive), '-exportPath', str(TEMP / 'TestFlightExport'),
                    '-exportOptionsPlist', str(options), '-allowProvisioningUpdates', '-authenticationKeyPath', str(KEY),
                    '-authenticationKeyID', os.environ['AC_API_KEY_ID'], '-authenticationKeyIssuerID', os.environ['AC_API_ISSUER_ID']], check=True)
    for _ in range(40):
        builds = api('builds?' + urllib.parse.urlencode({'filter[app]': os.environ['ASC_APP_ID'], 'sort': '-uploadedDate', 'limit': 30}))['data']
        build = next((b for b in builds if b['attributes']['version'] == os.environ['BUILD_NUMBER']), None)
        state = build['attributes']['processingState'] if build else 'NOT_FOUND'
        print('App Store Connect processing:', state, flush=True)
        if state == 'VALID':
            with open(os.environ['GITHUB_STEP_SUMMARY'], 'a') as output:
                output.write(f"Internal TestFlight build {os.environ['BUILD_NUMBER']} finished processing. Add it to your internal testing group in App Store Connect.\n")
            return
        if state in ('INVALID', 'FAILED'):
            raise SystemExit('Apple rejected the uploaded build')
        time.sleep(15)
    raise SystemExit('Build uploaded, but Apple processing did not finish within ten minutes')


if __name__ == '__main__':
    {'prepare': prepare, 'upload': upload}[sys.argv[1]]()
