"""Exercise the updater in disposable local repositories, without network access."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


UPDATER = Path(__file__).resolve().parents[1] / "update-upstream.sh"


class UpdateUpstreamTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.upstream = self.root / "upstream"
        self.fork = self.root / "fork"
        self.env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull,
                        GIT_CONFIG_NOSYSTEM="1", GIT_AUTHOR_NAME="Updater Test",
                        GIT_AUTHOR_EMAIL="test@example.invalid",
                        GIT_COMMITTER_NAME="Updater Test",
                        GIT_COMMITTER_EMAIL="test@example.invalid")
        self.upstream.mkdir()
        self.git(self.upstream, "init", "-q", "-b", "main")
        (self.upstream / "shared.txt").write_text("base\n")
        self.commit(self.upstream, "base")
        self.git(self.root, "clone", "-q", str(self.upstream), str(self.fork))
        self.git(self.fork, "remote", "add", "upstream", str(self.upstream))
        scripts = self.fork / "scripts"
        scripts.mkdir()
        shutil.copy2(UPDATER, scripts / "update-upstream.sh")
        self.checker = scripts / "check-linux-fork.sh"
        self.checker.write_text("#!/bin/sh\nexit 0\n")
        self.checker.chmod(0o755)
        (self.fork / "custom.txt").write_text("Linux customization\n")
        self.commit(self.fork, "custom fork")

    def run_command(self, cwd, *args, check=True):
        return subprocess.run(args, cwd=cwd, env=self.env, text=True,
                              stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                              check=check)

    def git(self, cwd, *args):
        return self.run_command(cwd, "git", "-c", "commit.gpgsign=false", *args).stdout.strip()

    def commit(self, cwd, message):
        self.git(cwd, "add", "-A")
        self.git(cwd, "commit", "-qm", message)

    def release(self, path="upstream.txt"):
        (self.upstream / path).write_text("upstream update\n")
        self.commit(self.upstream, "release")
        self.git(self.upstream, "tag", "v9.9.9")

    def update(self):
        # Disable signing locally in these temporary repositories only.
        self.git(self.fork, "config", "commit.gpgsign", "false")
        return self.run_command(self.fork, "bash", "scripts/update-upstream.sh",
                                "v9.9.9", check=False)

    def test_merge_preserves_customizations_and_repeat_is_noop(self):
        self.release()
        result = self.update()
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertEqual((self.fork / "custom.txt").read_text(), "Linux customization\n")
        self.assertEqual((self.fork / "upstream.txt").read_text(), "upstream update\n")
        self.assertEqual(self.git(self.fork, "status", "--porcelain"), "")
        head = self.git(self.fork, "rev-parse", "HEAD")
        self.assertEqual(self.update().returncode, 0)
        self.assertEqual(self.git(self.fork, "rev-parse", "HEAD"), head)
        self.assertTrue(self.git(self.fork, "branch", "--list", "backup/pre-upstream-*"))

    def test_dirty_work_is_preserved(self):
        self.release()
        (self.fork / "custom.txt").write_text("unsaved local work\n")
        (self.fork / "untracked.txt").write_text("keep me\n")
        self.git(self.fork, "add", "custom.txt")
        head = self.git(self.fork, "rev-parse", "HEAD")
        self.assertNotEqual(self.update().returncode, 0)
        self.assertEqual(self.git(self.fork, "rev-parse", "HEAD"), head)
        self.assertEqual(self.git(self.fork, "show", ":custom.txt"), "unsaved local work")
        self.assertEqual((self.fork / "untracked.txt").read_text(), "keep me\n")

    def test_conflict_can_be_aborted_without_losing_the_fork(self):
        (self.fork / "shared.txt").write_text("fork edit\n")
        self.commit(self.fork, "overlapping fork edit")
        self.release("shared.txt")
        head = self.git(self.fork, "rev-parse", "HEAD")
        self.assertNotEqual(self.update().returncode, 0)
        self.assertIn("shared.txt", self.git(self.fork, "diff", "--name-only", "--diff-filter=U"))
        self.assertEqual(self.git(self.fork, "rev-parse", "HEAD"), head)
        self.git(self.fork, "merge", "--abort")
        self.assertEqual((self.fork / "shared.txt").read_text(), "fork edit\n")

    def test_failed_checks_leave_merge_uncommitted(self):
        self.checker.write_text("#!/bin/sh\nexit 1\n")
        self.commit(self.fork, "failing checks")
        self.release()
        head = self.git(self.fork, "rev-parse", "HEAD")
        self.assertNotEqual(self.update().returncode, 0)
        self.assertEqual(self.git(self.fork, "rev-parse", "HEAD"), head)
        self.git(self.fork, "rev-parse", "--verify", "MERGE_HEAD")
        self.assertNotEqual(self.update().returncode, 0)


if __name__ == "__main__":
    unittest.main()
