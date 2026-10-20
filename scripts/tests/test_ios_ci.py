import importlib.util
from pathlib import Path
import tempfile
import unittest
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('ios_ci', ROOT / 'scripts/ios/configure-ci.py')
ci = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ci)


def phone(udid, available=True, name='iPhone 17 Pro'):
    return {'udid': udid, 'isAvailable': available, 'name': name}


class IosCiTests(unittest.TestCase):
    def test_latest_available_runtime_wins_over_first_device(self):
        devices = {
            'com.apple.CoreSimulator.SimRuntime.iOS-26-0-1': [phone('old')],
            'com.apple.CoreSimulator.SimRuntime.iOS-26-2': [phone('new')],
            'com.apple.CoreSimulator.SimRuntime.iOS-26-10': [phone('newest')],
            'com.apple.CoreSimulator.SimRuntime.iOS-27-0': [phone('unsupported')],
        }
        self.assertEqual(ci.select_simulator(devices)['udid'], 'newest')

    def test_unavailable_devices_and_other_platforms_are_ignored(self):
        devices = {
            'com.apple.CoreSimulator.SimRuntime.iOS-26-3': [phone('unavailable', False)],
            'com.apple.CoreSimulator.SimRuntime.iOS-26-2': [phone('ipad', name='iPad Pro'), phone('usable')],
            'com.apple.CoreSimulator.SimRuntime.tvOS-26-3': [phone('wrong-platform')],
        }
        self.assertEqual(ci.select_simulator(devices)['udid'], 'usable')
        with self.assertRaisesRegex(RuntimeError, 'available iOS 26'):
            ci.select_simulator({'com.apple.CoreSimulator.SimRuntime.iOS-18-6': [phone('old')]})

    def test_ci_scheme_preserves_tests_and_local_debugging(self):
        source = ROOT / 'apps/ios/Zeron.xcodeproj/xcshareddata/xcschemes/Zeron.xcscheme'
        original = source.read_bytes()
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / 'Zeron-CI.xcscheme'
            ci.create_ci_scheme(source, destination)
            local = ET.fromstring(original)
            generated = ET.parse(destination).getroot()
            test = generated.find('TestAction')
            self.assertEqual(test.get('selectedDebuggerIdentifier'), '')
            self.assertEqual(test.get('selectedLauncherIdentifier'), 'Xcode.IDEFoundation.Launcher.PosixSpawn')
            self.assertEqual(ET.tostring(test.find('Testables')),
                             ET.tostring(local.find('TestAction/Testables')))
            self.assertEqual(ET.tostring(generated.find('LaunchAction')),
                             ET.tostring(local.find('LaunchAction')))
            self.assertEqual(source.read_bytes(), original)
            self.assertEqual(local.find('TestAction').get('selectedDebuggerIdentifier'),
                             'Xcode.DebuggerFoundation.Debugger.LLDB')
        with self.assertRaises(ValueError):
            ci.create_ci_scheme(source, source)


if __name__ == '__main__':
    unittest.main()
