"""Exercise sync-zgx-fork.py in disposable local repositories, without network access."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "sync-zgx-fork.py"
LINES = [f"line {n}\n" for n in range(1, 11)]


class SyncTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.repo = self.root / "zeron"
        self.env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1",
                        GIT_AUTHOR_NAME="Sync Test", GIT_AUTHOR_EMAIL="test@example.invalid",
                        GIT_COMMITTER_NAME="Sync Test", GIT_COMMITTER_EMAIL="test@example.invalid",
                        GIT_EDITOR="true")
        self.env.pop("GIT_DIR", None)
        self.write({"apps/zgx/ui/screens/shell/init.luau": "".join(LINES),
                    "apps/zgx/ui/ds/menu.luau": 'local Icon = require("@self/icon")\n' + "".join(LINES),
                    "apps/zgx/plugins-sdk/zeron.luau": "sdk 1\n",
                    ".gitignore": "/target\n"})
        self.git(self.repo, "init", "-q", "-b", "main")
        self.v1 = self.commit(self.repo, "upstream v1")
        self.git(self.repo, "tag", "v1")
        fork_lines = LINES.copy()
        fork_lines[1] = "line 2, the fork's\n"
        self.write({"apps/zgx/plugins-fork/fork_sdk.luau": "fork sdk\n",
                    "apps/zgx/plugins-fork/fork_motion.luau": "motion\n",
                    "apps/zgx/plugins-fork/plug/zeron.luau": "sdk 1\n",
                    "apps/zgx/plugins-fork/plug/fork_sdk.luau": "fork sdk\n",
                    "apps/zgx/plugins-fork/plug/ui/fork_motion.luau": "motion\n",
                    "apps/zgx/plugins-fork/plug/shell.luau": "".join(fork_lines),
                    "apps/zgx/plugins-fork/plug/menu.luau":
                        'local Icon = require("@ds/icon")\n' + "".join(fork_lines),
                    "apps/zgx/plugins-fork/upstream-copies.txt":
                        "# copy upstream base\n"
                        f"plug/shell.luau  ui/screens/shell/init.luau  {self.v1}\n"
                        f"plug/menu.luau   ui/ds/menu.luau             {self.v1}\n"})
        (self.repo / "scripts").mkdir()
        shutil.copy2(SCRIPT, self.repo / "scripts/sync-zgx-fork.py")
        self.commit(self.repo, "fork")

    def write(self, files, root=None):
        for name, text in files.items():
            path = (root or self.repo) / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)

    def git(self, cwd, *args):
        return subprocess.run(["git", "-c", "commit.gpgsign=false", *args], cwd=cwd, env=self.env,
                              text=True, capture_output=True, check=True).stdout.strip()

    def commit(self, cwd, message):
        self.git(cwd, "add", "-A")
        self.git(cwd, "commit", "-qm", message)
        return self.git(cwd, "rev-parse", "HEAD")

    def sync(self, *args):
        return subprocess.run(["python3", "scripts/sync-zgx-fork.py", *args], cwd=self.repo, env=self.env,
                              text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)

    def release(self, line_index, text, sdk="sdk 1\n"):
        """An upstream v2 that changes a line of the screen and adds a require to the ds file, merged."""
        self.git(self.repo, "switch", "-q", "-c", "upstream", "v1")
        lines = LINES.copy()
        lines[line_index] = text
        self.write({"apps/zgx/ui/screens/shell/init.luau": "".join(lines),
                    "apps/zgx/ui/ds/menu.luau": 'local Icon = require("@self/icon")\n'
                                                'local Text = require("@self/text")\n' + "".join(LINES),
                    "apps/zgx/plugins-sdk/zeron.luau": sdk})
        v2 = self.commit(self.repo, "upstream v2")
        self.git(self.repo, "switch", "-q", "main")
        self.git(self.repo, "merge", "-q", "--no-ff", "--no-commit", v2)
        return v2

    def fork_file(self, name):
        return (self.repo / "apps/zgx/plugins-fork" / name).read_text()

    def test_upstream_change_is_merged_into_the_copies_and_rerun_is_a_noop(self):
        v2 = self.release(8, "line 9, upstream's\n", sdk="sdk 2\n")
        result = self.sync("--to", v2)
        self.assertEqual(result.returncode, 0, result.stdout)
        shell = self.fork_file("plug/shell.luau")
        self.assertIn("line 2, the fork's", shell)
        self.assertIn("line 9, upstream's", shell)
        menu = self.fork_file("plug/menu.luau")
        self.assertIn('require("@ds/text")', menu)
        self.assertNotIn("@self/", menu)
        self.assertEqual(self.fork_file("plug/zeron.luau"), "sdk 2\n")
        self.assertIn(v2, self.fork_file("upstream-copies.txt"))
        self.assertNotIn(self.v1, self.fork_file("upstream-copies.txt"))
        self.assertEqual(self.git(self.repo, "diff", "--name-only"), "")  # all staged
        before = self.git(self.repo, "diff", "--cached")
        result = self.sync("--to", v2)
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertEqual(self.git(self.repo, "diff", "--cached"), before)
        self.assertEqual(self.sync("--check").returncode, 0)

    def test_conflict_is_left_for_git_and_reported_until_resolved(self):
        v2 = self.release(1, "line 2, upstream's\n")
        result = self.sync("--to", v2)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("CONFLICT: apps/zgx/plugins-fork/plug/shell.luau", result.stdout)
        self.assertIn("<<<<<<< plug/shell.luau", self.fork_file("plug/shell.luau"))
        unmerged = self.git(self.repo, "ls-files", "--unmerged", "--", "apps/zgx/plugins-fork/plug/shell.luau")
        self.assertEqual(len(unmerged.splitlines()), 3)
        self.assertIn(v2, self.fork_file("upstream-copies.txt"))
        again = self.sync("--to", v2)
        self.assertNotEqual(again.returncode, 0)
        self.assertIn("CONFLICT", again.stdout)
        self.assertEqual(self.fork_file("plug/shell.luau").count("<<<<<<<"), 1)
        self.assertNotEqual(self.sync("--check").returncode, 0)
        self.write({"apps/zgx/plugins-fork/plug/shell.luau": "".join(LINES)})
        self.git(self.repo, "add", "apps/zgx/plugins-fork/plug/shell.luau")
        self.assertEqual(self.sync("--check").returncode, 0)
        self.git(self.repo, "merge", "--abort")
        self.assertIn(self.v1, self.fork_file("upstream-copies.txt"))
        self.assertEqual(self.git(self.repo, "status", "--porcelain"), "")

    def test_check_wants_a_recorded_base_for_every_copy(self):
        manifest = self.repo / "apps/zgx/plugins-fork/upstream-copies.txt"
        manifest.write_text(manifest.read_text() + "plug/extra.luau ui/screens/shell/init.luau\n")
        result = self.sync("--check")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("expected <copy> <upstream> <base>", result.stdout)
        manifest.write_text(manifest.read_text().replace("init.luau\n", f"init.luau {'0' * 40}\n"))
        self.write({"apps/zgx/plugins-fork/plug/extra.luau": "x\n"})
        result = self.sync("--check")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("plug/extra.luau: no recorded base", result.stdout)

    # The zgx fork: upstream's pin moves; the fork's branch follows it.

    def zgx(self, conflict=False, checked_out=True):
        up, fork = self.root / "zgx-upstream", self.root / "zgx-fork"
        self.write({"lib.rs": "upstream\n", "web.rs": "web\n"}, up)
        self.git(self.root, "init", "-q", "-b", "main", str(up))
        old_pin = self.commit(up, "zgx A")
        self.git(self.root, "clone", "-q", str(up), str(fork))
        self.git(fork, "switch", "-q", "-c", "zeron-fork")
        self.write({"web.rs": "web\nsnapshot\n"}, fork)
        fork_commit = self.commit(fork, "Web pages: snapshots")
        if not checked_out:
            self.git(fork, "switch", "-q", "main")
        self.write({"web.rs": "web\nupstream's line\n" if conflict else "web\n", "lib.rs": "upstream 2\n"}, up)
        new_pin = self.commit(up, "zgx B")  # After the clone: the sync fetches it.
        cargo = ('[dependencies]\n'
                 f'zgx = {{ git = "file://{up}", rev = "{old_pin}" }}\n'
                 '[patch.crates-io]\n'
                 f'winit = {{ git = "file://{up}", rev = "{old_pin}" }}\n\n'
                 f'[patch."file://{up}"]\n'
                 f'zgx = {{ git = "file://{fork}", rev = "{fork_commit}" }}\n'
                 f'zgx-run = {{ git = "file://{fork}", rev = "{fork_commit}" }}\n')
        self.write({"apps/zgx/Cargo.toml": cargo})
        self.commit(self.repo, "zgx fork patch")
        self.assertEqual(self.sync("--check").returncode, 0)
        # Upstream's merge moves its pin (and only that).
        self.write({"apps/zgx/Cargo.toml": cargo.replace(f'git = "file://{up}", rev = "{old_pin}"',
                                                         f'git = "file://{up}", rev = "{new_pin}"')})
        return fork, fork_commit, new_pin

    def patch_revs(self):
        text = (self.repo / "apps/zgx/Cargo.toml").read_text().split("[patch.\"")[1]
        return {line.split('rev = "')[1][:40] for line in text.splitlines() if 'rev = "' in line}

    def test_zgx_fork_is_rebased_onto_the_new_pin(self):
        fork, fork_commit, new_pin = self.zgx()
        stale = self.sync("--check")
        self.assertNotEqual(stale.returncode, 0)
        self.assertIn("doesn't contain upstream's pin", stale.stdout)
        result = self.sync()
        self.assertEqual(result.returncode, 0, result.stdout)
        tip = self.git(fork, "rev-parse", "zeron-fork")
        self.assertEqual(self.patch_revs(), {tip})
        self.git(fork, "merge-base", "--is-ancestor", new_pin, tip)
        self.assertEqual(self.git(fork, "show", f"{tip}:web.rs"), "web\nsnapshot")
        self.assertEqual(self.git(fork, "show", f"{tip}:lib.rs"), "upstream 2")
        self.assertEqual(self.git(fork, "rev-parse", f"backup/zeron-fork-{fork_commit[:12]}"), fork_commit)
        self.assertIn(f'rev = "{new_pin}"', self.git(self.repo, "show", ":apps/zgx/Cargo.toml"))
        again = self.sync()
        self.assertEqual(again.returncode, 0, again.stdout)
        self.assertIn("already contains", again.stdout)
        self.assertEqual(self.git(fork, "rev-parse", "zeron-fork"), tip)
        self.assertEqual(self.sync("--check").returncode, 0)

    def test_zgx_rebase_conflict_stops_and_resumes(self):
        fork, fork_commit, new_pin = self.zgx(conflict=True, checked_out=False)
        result = self.sync()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("rebasing the fork's commits onto upstream's pin", result.stdout)
        self.assertIn("CONFLICT (content): Merge conflict in web.rs", result.stdout)
        self.assertEqual(self.patch_revs(), {fork_commit})
        spare = self.repo / "target/zgx-rebase-zeron-fork"
        self.assertNotEqual(self.sync().returncode, 0)  # Still mid-rebase: nothing moves.
        (spare / "web.rs").write_text("web\nupstream's line\nsnapshot\n")
        self.git(spare, "add", "web.rs")
        self.git(spare, "rebase", "--continue")
        result = self.sync()
        self.assertEqual(result.returncode, 0, result.stdout)
        tip = self.git(fork, "rev-parse", "zeron-fork")
        self.assertEqual(self.patch_revs(), {tip})
        self.git(fork, "merge-base", "--is-ancestor", new_pin, tip)
        self.assertFalse(spare.exists())


if __name__ == "__main__":
    unittest.main()
