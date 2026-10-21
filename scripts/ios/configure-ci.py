#!/usr/bin/env python3
"""Select an iOS simulator and generate a CI-only scheme without LLDB."""
import json
import os
from pathlib import Path
import re
import subprocess
import xml.etree.ElementTree as ET


def select_simulator(devices):
    candidates = []
    for runtime, group in devices.items():
        match = re.search(r'\.iOS-(26(?:-\d+)*)$', runtime)
        if not match:
            continue
        version = tuple(int(part) for part in match[1].split('-'))
        for device in group:
            if device.get('isAvailable') and device['name'].startswith('iPhone'):
                candidates.append((version, device))
    if not candidates:
        raise RuntimeError('An available iOS 26 iPhone simulator is required')
    return max(candidates, key=lambda candidate: candidate[0])[1]


def create_ci_scheme(source, destination):
    if source.resolve() == destination.resolve():
        raise ValueError('The local development scheme must not be overwritten')
    tree = ET.parse(source)
    action = tree.getroot().find('TestAction')
    if action is None:
        raise ValueError('The scheme has no TestAction')
    # XCTest still runs every test, but does not suspend the host app waiting
    # for an LLDB attachment on the headless runner.
    action.set('selectedDebuggerIdentifier', '')
    action.set('selectedLauncherIdentifier', 'Xcode.IDEFoundation.Launcher.PosixSpawn')
    tree.write(destination, encoding='utf-8', xml_declaration=True)


def main():
    root = Path(__file__).resolve().parents[2]
    schemes = root / 'apps/ios/Zeron.xcodeproj/xcshareddata/xcschemes'
    create_ci_scheme(schemes / 'Zeron.xcscheme', schemes / 'Zeron-CI.xcscheme')
    devices = json.loads(subprocess.check_output(
        ['xcrun', 'simctl', 'list', 'devices', 'available', '--json']))['devices']
    device = select_simulator(devices)
    with open(os.environ['GITHUB_ENV'], 'a') as output:
        output.write('IOS_SIMULATOR_ID=' + device['udid'] + '\n')
    print('CI tests use Zeron-CI without debugger attachment.')
    print('Selected simulator:', device['name'], device['udid'])


if __name__ == '__main__':
    main()
