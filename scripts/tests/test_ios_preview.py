"""Reject credentials that cannot produce the requested registered-device preview."""
import copy
import datetime
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location(
    'preview', Path(__file__).resolve().parents[1] / 'ios/direct-preview.py')
preview = importlib.util.module_from_spec(spec)
spec.loader.exec_module(preview)


class PreviewProfilesTests(unittest.TestCase):
    def setUp(self):
        self.now = datetime.datetime(2026, 10, 20, tzinfo=datetime.timezone.utc)
        self.profile = {
            'ExpirationDate': datetime.datetime(2027, 1, 1),
            'TeamIdentifier': ['MYTEAM'],
            'ApplicationIdentifierPrefix': ['MYTEAM'],
            'Entitlements': {
                'application-identifier': 'MYTEAM.com.example.zeron',
                'get-task-allow': False,
            },
            'ProvisionedDevices': ['registered-iphone', 'registered-ipad'],
            'DeveloperCertificates': [b'distribution-certificate'],
        }

    def validate(self, profile):
        return preview.validate_profile(profile, 'com.example.zeron', 'MYTEAM', self.now)

    def test_accepts_distribution_profile_for_registered_devices(self):
        devices, certificates = self.validate(self.profile)
        self.assertEqual(devices, {'registered-iphone', 'registered-ipad'})
        self.assertEqual(len(certificates), 1)

    def test_rejects_rocal_profile_and_wildcard_for_zeron(self):
        for identifier in ['MYTEAM.onl.solace.mail', 'MYTEAM.com.example.zeron.LiveActivity', 'MYTEAM.*']:
            with self.subTest(identifier=identifier):
                self.profile['Entitlements']['application-identifier'] = identifier
                with self.assertRaisesRegex(ValueError, 'exact app bundle'):
                    self.validate(self.profile)

    def test_rejects_other_team_even_with_matching_bundle(self):
        self.profile['TeamIdentifier'] = ['ANOTHERTEAM']
        with self.assertRaisesRegex(ValueError, 'another Apple team'):
            self.validate(self.profile)

    def test_rejects_expired_profile(self):
        self.profile['ExpirationDate'] = self.now
        with self.assertRaisesRegex(ValueError, 'expired'):
            self.validate(self.profile)

    def test_rejects_store_enterprise_and_debug_profiles(self):
        for change in ['no_devices', 'all_devices', 'debug']:
            profile = copy.deepcopy(self.profile)
            if change == 'no_devices':
                del profile['ProvisionedDevices']
            elif change == 'all_devices':
                profile['ProvisionsAllDevices'] = True
            else:
                profile['Entitlements']['get-task-allow'] = True
            with self.subTest(change=change), self.assertRaisesRegex(ValueError, 'ad hoc'):
                self.validate(profile)


if __name__ == '__main__':
    unittest.main()
