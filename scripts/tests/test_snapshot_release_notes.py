"""Release summaries are pinned, bounded, offline-safe, and fork-aware."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / "snapshot-release-notes.py"
spec = importlib.util.spec_from_file_location("release_notes", SCRIPT)
notes = importlib.util.module_from_spec(spec)
spec.loader.exec_module(notes)


class ReleaseNotesTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1",
                        GIT_AUTHOR_NAME="Test", GIT_COMMITTER_NAME="Test",
                        GIT_AUTHOR_EMAIL="test@example.invalid", GIT_COMMITTER_EMAIL="test@example.invalid")
        self.git("init", "-q", "-b", "main")
        self.version("1.0.0")
        self.commit("Bump version to 1.0.0")
        self.git("tag", "v1.0.0")
        (self.root / "feature.txt").write_text("new feature\n")
        self.commit("Add upstream feature [details]")
        self.version("1.0.1")
        self.commit("Bump version to 1.0.1")
        self.git("tag", "v1.0.1")
        self.upstream = self.git("rev-parse", "HEAD")
        (self.root / "custom.txt").write_text("Linux fork\n")
        self.commit("Improve Linux service handling")
        self.fork = self.git("rev-parse", "HEAD")

    def git(self, *args):
        return subprocess.check_output(["git", "-c", "commit.gpgsign=false", *args],
                                       cwd=self.root, env=self.env, text=True,
                                       stderr=subprocess.PIPE).strip()

    def version(self, version):
        (self.root / "Cargo.toml").write_text(f'[workspace.package]\nversion="{version}"\n')

    def commit(self, message):
        self.git("add", "-A")
        self.git("commit", "-qm", message)

    def snapshot(self, offline=True):
        return notes.snapshot(self.root, "v1.0.1", self.fork, offline=offline)

    def test_offline_uses_only_upstream_range_and_separates_fork(self):
        catalog = self.snapshot()
        current, previous = catalog["releases"]
        self.assertEqual([e["version"] for e in catalog["releases"]], ["1.0.1", "1.0.0"])
        self.assertEqual(current["upstream_commit"], self.upstream)
        self.assertEqual(current["notes_source"], "git_history")
        self.assertIn("Add upstream feature", current["upstream_notes"])
        self.assertNotIn("Improve Linux", current["upstream_notes"])
        self.assertNotIn("Bump version", current["upstream_notes"])
        self.assertIn(r"\[details\]", current["upstream_notes"])
        self.assertEqual(current["fork_changes"], ["Improve Linux service handling"])
        self.assertEqual(previous["fork_changes"], [])
        notes.check(self.root)

    def test_release_notes_are_cached_across_repeated_runs_and_api_outages(self):
        with patch.object(notes, "release_body", return_value=("Official upstream notes", "2026-10-09")):
            first = self.snapshot(offline=False)
        with patch.object(notes, "release_body", side_effect=AssertionError("cached notes should not fetch")):
            second = self.snapshot()
        self.assertEqual(first, second)
        self.assertEqual(second["releases"][0]["notes_source"], "github_release")

    def test_previous_version_is_discovered_without_its_local_tag(self):
        self.git("tag", "-d", "v1.0.0")
        catalog = self.snapshot()
        self.assertEqual(catalog["releases"][1]["version"], "1.0.0")

    def test_later_api_access_promotes_fallback_without_losing_fork_notes(self):
        before = self.snapshot()
        with patch.object(notes, "release_body", return_value=("Official notes", "2026-10-09")):
            updated = self.snapshot(offline=False)
        self.assertEqual(updated["releases"][0]["notes_source"], "github_release")
        self.assertEqual(updated["releases"][0]["fork_changes"], before["releases"][0]["fork_changes"])
        self.assertEqual(updated["releases"][0]["upstream_notes"], "Official notes")

    def test_next_release_keeps_previous_notes_and_only_new_fork_commits(self):
        before = self.snapshot()
        self.commit("Bundle initial notes")
        (self.root / "custom.txt").write_text("Linux fork improved\n")
        self.commit("Improve fork UI")
        fork_head = self.git("rev-parse", "HEAD")
        self.git("switch", "-qc", "upstream-next", self.upstream)
        (self.root / "feature.txt").write_text("next upstream feature\n")
        self.commit("Improve upstream feature")
        self.version("1.0.2")
        self.commit("Bump version to 1.0.2")
        self.git("tag", "v1.0.2")
        self.git("switch", "-q", "main")
        self.git("merge", "--no-ff", "--no-edit", "v1.0.2")
        result = notes.snapshot(self.root, "v1.0.2", fork_head, offline=True)
        self.assertEqual(result["releases"][1], before["releases"][0])
        self.assertIn("Improve fork UI", result["releases"][0]["fork_changes"])
        self.assertNotIn("Improve Linux service handling", result["releases"][0]["fork_changes"])
        self.assertNotIn("Bundle initial notes", result["releases"][0]["fork_changes"])
        self.assertNotIn("Improve upstream feature", result["releases"][0]["fork_changes"])
        self.assertIn("Improve upstream feature", result["releases"][0]["upstream_notes"])
        notes.check(self.root)

    def test_cannot_bundle_an_unmerged_release_or_wrong_repository(self):
        self.version("1.0.0")
        with self.assertRaisesRegex(ValueError, "Merge the release"):
            self.snapshot()
        self.version("1.0.1")
        self.snapshot()
        path = self.root / notes.CATALOG
        catalog = json.loads(path.read_text())
        catalog["upstream_repository"] = "other/repo"
        path.write_text(json.dumps(catalog))
        with self.assertRaisesRegex(ValueError, "different repository"):
            notes.check(self.root)

    def test_api_payloads_cannot_substitute_a_newer_release(self):
        valid = {"tag_name": "v1.0.1", "body": "Release notes", "published_at": "2026-10-09"}
        for invalid in [dict(valid, tag_name="v1.0.2"), dict(valid, draft=True),
                        dict(valid, prerelease=True), dict(valid, body="x" * 65537),
                        dict(valid, body=42), [], "malformed JSON"]:
            response = invalid if isinstance(invalid, str) else json.dumps(invalid)
            with self.subTest(invalid=str(invalid)[:40]), patch.object(notes.subprocess, "check_output", return_value=response):
                self.assertIsNone(notes.release_body("v1.0.1", False))
        with patch.object(notes.subprocess, "check_output", return_value=json.dumps(valid)):
            self.assertEqual(notes.release_body("v1.0.1", False), ("Release notes", "2026-10-09"))

    def test_api_timeout_falls_back_and_offline_never_calls_api(self):
        with patch.object(notes.subprocess, "check_output", side_effect=subprocess.TimeoutExpired("gh", 20)):
            self.assertIsNone(notes.release_body("v1.0.1", False))
        with patch.object(notes.subprocess, "check_output", side_effect=AssertionError("network disabled")):
            self.assertIsNone(notes.release_body("v1.0.1", True))


if __name__ == "__main__":
    unittest.main()
