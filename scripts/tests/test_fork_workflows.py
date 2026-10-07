"""Check isolation rules against representative workflows and unsafe merges."""
import importlib.util
from pathlib import Path
import tempfile
import unittest


spec = importlib.util.spec_from_file_location(
    "workflow_policy", Path(__file__).resolve().parents[1] / "check-fork-workflows.py")
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)


class WorkflowPolicyTests(unittest.TestCase):
    def errors(self, text, name="deploy.yml"):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / name
            path.write_text(text)
            return policy.errors_for(path)

    def test_guard_preserves_enclosed_upstream_conditions(self):
        for condition in [policy.UPSTREAM,
                          policy.UPSTREAM + " && (github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v'))",
                          "${{ " + policy.UPSTREAM + " && (always() || failure()) }}",
                          policy.UPSTREAM + " && (contains('literal ) text', 'text'))"]:
            with self.subTest(condition=condition):
                self.assertTrue(policy.upstream_only(condition))

    def test_repository_guard_cannot_be_bypassed_with_or(self):
        for condition in ["true", "!" + policy.UPSTREAM,
                          policy.UPSTREAM + " || true",
                          policy.UPSTREAM + " && (false) || (true)",
                          policy.UPSTREAM + " && false || true"]:
            with self.subTest(condition=condition):
                self.assertFalse(policy.upstream_only(condition))

    def test_new_upstream_job_without_guard_is_rejected(self):
        text = f"jobs:\n  guarded:\n    if: {policy.UPSTREAM}\n    runs-on: ubuntu-latest\n  new-publisher:\n    runs-on: ubuntu-latest\n"
        errors = self.errors(text)
        self.assertEqual(len(errors), 1)
        self.assertIn("new-publisher", errors[0])

    def test_updater_cannot_gain_an_automatic_trigger_or_write_token(self):
        text = ("on:\n  workflow_dispatch:\npermissions:\n  contents: read\n"
                f"jobs:\n  update:\n    if: {policy.MANUAL}\n    runs-on: ubuntu-latest\n")
        self.assertEqual(self.errors(text, "update-linux-fork.yml"), [])
        for unsafe in [text.replace("  workflow_dispatch:", "  push:\n  workflow_dispatch:"),
                       text.replace("contents: read", "contents: write"),
                       text.replace("    runs-on:", "    permissions:\n      contents: write\n    runs-on:")]:
            with self.subTest(unsafe=unsafe):
                self.assertTrue(self.errors(unsafe, "update-linux-fork.yml"))

    def test_unsupported_job_formats_cannot_hide_unguarded_jobs(self):
        guarded = f"jobs:\n  guarded:\n    if: {policy.UPSTREAM}\n    runs-on: ubuntu-latest\n"
        for unsafe in ["name: deploy\njobs: {}\n",
                       guarded + "  new-publisher: {runs-on: ubuntu-latest}\n",
                       guarded + "  'new-publisher':\n    runs-on: ubuntu-latest\n",
                       guarded + "name: deploy\njobs:\n  new-publisher:\n    runs-on: ubuntu-latest\n"]:
            with self.subTest(unsafe=unsafe):
                self.assertTrue(self.errors(unsafe))

    def test_fork_tests_allow_automatic_runs_with_read_only_permissions(self):
        text = ("on:\n  pull_request:\n  push:\n    branches: [main]\n  workflow_dispatch:\n"
                "permissions:\n  contents: read\n"
                f"jobs:\n  core-tests:\n    if: {policy.FORK}\n    runs-on: ubuntu-24.04\n")
        self.assertEqual(self.errors(text, "linux-fork-tests.yml"), [])
        for unsafe in [text.replace(policy.FORK, "true"),
                       text.replace("contents: read", "contents: write"),
                       text.replace("    runs-on:", "    permissions:\n      contents: write\n    runs-on:"),
                       text.replace("  workflow_dispatch:", "  schedule:\n  workflow_dispatch:")]:
            with self.subTest(unsafe=unsafe):
                self.assertTrue(self.errors(unsafe, "linux-fork-tests.yml"))

    def test_only_fork_test_workflow_can_run_automatically_in_fork(self):
        text = f"jobs:\n  publish:\n    if: {policy.FORK}\n    runs-on: ubuntu-latest\n"
        self.assertTrue(self.errors(text, "deploy.yml"))


if __name__ == "__main__":
    unittest.main()
