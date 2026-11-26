"""Clipboard policy and stale-callback checks; no running VM or clipboard is used."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock, patch


HERE = Path(__file__).resolve().parent


class ClipboardTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.runtime = tempfile.TemporaryDirectory()
        root = Path(cls.runtime.name)
        os.environ['XDG_RUNTIME_DIR'] = str(root)
        os.environ['WINVM_HOME'] = str(root / 'guest')
        (root / 'winvm-guest').mkdir()
        spec = importlib.util.spec_from_file_location('winvm_viewer', HERE / 'viewer.py')
        cls.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.module)

    @classmethod
    def tearDownClass(cls):
        cls.module.LOG.close()
        cls.runtime.cleanup()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name)
        self.module.VM_HOME = self.home

    def command(self, *args):
        result = subprocess.run(
            [sys.executable, str(HERE / 'winvm'), 'clipboard', *args, '--json'],
            env=dict(os.environ, WINVM_HOME=str(self.home)),
            capture_output=True, text=True, check=True,
        )
        return json.loads(result.stdout)

    def viewer(self):
        return self.module.Viewer()

    def test_legacy_default_and_persistent_switch_preserve_other_settings(self):
        self.assertTrue(self.command()['clipboard'])
        path = self.home / 'config.json'
        self.assertFalse(path.exists())
        previous = {'vcpus': 12, 'pin': '0-11', 'clip_port': 8888, 'custom': 'keep'}
        path.write_text(json.dumps(previous))
        self.assertFalse(self.command('off')['clipboard'])
        self.assertEqual(json.loads(path.read_text()), dict(previous, clipboard=False))
        self.assertFalse(self.command()['clipboard'])
        self.assertTrue(self.command('on')['clipboard'])
        self.assertEqual(json.loads(path.read_text()), dict(previous, clipboard=True))
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_disabled_paths_neither_read_nor_write_the_host_clipboard(self):
        viewer = self.viewer()
        viewer.conn = Mock()
        with patch.object(viewer, 'host_text') as read, patch.object(self.module.subprocess, 'run') as write:
            viewer.host_clip_changed()
            viewer.guest_copied('copied in Windows')
            viewer.register_clipboard()
            read.assert_not_called()
            write.assert_not_called()
            viewer.conn.call.assert_not_called()
        invocation = Mock()
        viewer.clip_call(viewer.conn, None, None, None, 'Request', Mock(), invocation)
        invocation.return_dbus_error.assert_called_once()
        invocation = Mock()
        viewer.clip_call(viewer.conn, None, None, None, 'Grab', Mock(), invocation)
        invocation.return_value.assert_called_once_with(None)
        viewer.conn.call.assert_not_called()

    def test_disabling_stops_both_transports_and_unregisters_qemu(self):
        viewer = self.viewer()
        viewer.clipboard_enabled = True
        viewer.clip_ready = True
        viewer.conn = Mock()
        viewer.conn.is_closed.return_value = False
        watch, server = viewer.watch, viewer.clip_server = Mock(), Mock()
        generation = viewer.clip_generation
        viewer.set_clipboard_enabled(False)
        watch.force_exit.assert_called_once()
        server.shutdown.assert_called_once()
        server.server_close.assert_called_once()
        self.assertIsNone(viewer.watch)
        self.assertIsNone(viewer.clip_server)
        self.assertFalse(viewer.clip_ready)
        self.assertGreater(viewer.clip_generation, generation)
        self.assertEqual(viewer.conn.call.call_args.args[3], 'Unregister')

    def test_late_registration_cannot_restart_a_stopped_watcher(self):
        viewer = self.viewer()
        viewer.conn = Mock()
        generation = viewer.clip_generation
        viewer.clip_generation += 1
        # Even after a later re-enable, an old reply must not affect the new registration.
        viewer.clipboard_enabled = True
        viewer.clip_registering = True
        with patch.object(self.module.Gio.Subprocess, 'new') as start:
            viewer.clip_registered(viewer.conn, Mock(), generation)
            start.assert_not_called()
        self.assertTrue(viewer.clip_registering)
        self.assertFalse(viewer.clip_ready)

    def test_late_guest_data_cannot_overwrite_a_new_host_copy(self):
        viewer = self.viewer()
        viewer.clipboard_enabled = True
        viewer.conn = Mock()
        viewer.conn.call_finish.return_value.get_child_value.return_value.get_data_as_bytes.return_value.get_data.return_value = b'old Windows copy'
        params = Mock()
        params.unpack.return_value = (0, 1, [self.module.TEXT])
        viewer.clip_call(viewer.conn, None, None, None, 'Grab', params, Mock())
        callback = viewer.conn.call.call_args.args[9]
        viewer.clip_generation += 1
        with patch.object(viewer, 'guest_copied') as copied:
            callback(viewer.conn, Mock(), None)
            copied.assert_not_called()

    def test_atomic_config_changes_are_seen_without_a_viewer_restart(self):
        viewer = self.viewer()
        with patch.object(viewer, 'set_clipboard_enabled') as apply:
            viewer.monitor_clipboard_settings()
            self.addCleanup(viewer.clip_monitor.cancel)
            apply.assert_called_with(True)
            for value in ['off', 'on', 'off']:
                apply.reset_mock()
                self.command(value)
                deadline = time.monotonic() + 3
                context = self.module.GLib.MainContext.default()
                while not apply.called and time.monotonic() < deadline:
                    while context.pending():
                        context.iteration(False)
                    time.sleep(.01)
                apply.assert_called_with(value == 'on')

    def test_invalid_config_does_not_reenable_sharing(self):
        viewer = self.viewer()
        (self.home / 'config.json').write_text('{"clipboard":"false"}')
        with patch.object(viewer, 'set_clipboard_enabled') as apply:
            viewer.refresh_clipboard_settings()
            apply.assert_not_called()


if __name__ == '__main__':
    unittest.main()
