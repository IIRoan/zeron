"""Run build-preview.sh against fake gh, git and curl commands."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]

# Run 1 outcome comes from $WATCH_1_EXIT and $RUN_1_STEPS; later runs succeed.
FAKE_GH = r'''#!/bin/bash
echo "gh $*" >> "$CALLS"
case "$1 $2" in
  "workflow run")
    n=$(( $(cat "$COUNT" 2>/dev/null || echo 0) + 1 )); echo "$n" > "$COUNT"
    echo "https://github.com/IIRoan/zeron/actions/runs/$n" ;;
  "run watch") [ "$3" = 1 ] && exit "$WATCH_1_EXIT"; exit 0 ;;
  "run view")
    case "$*" in
      *--log*) echo "page: https://expo.dev/accounts/a/projects/b/builds/abc-123" ;;
      *) [ "$3" = 1 ] && echo "$RUN_1_STEPS" || echo 9 ;;
    esac ;;
esac
'''

FAKE_GIT = r'''#!/bin/bash
echo "git $*" >> "$CALLS"
case "$1" in
  rev-parse) echo deadbeef ;;
  log) echo "deadbee subject" ;;
  show) echo '{"expo":{"extra":{"eas":{"projectId":"proj"}}}}' ;;
esac
'''


class BuildPreviewTests(unittest.TestCase):
    def run_script(self, watch_exit, steps):
        with tempfile.TemporaryDirectory() as temp:
            temp = Path(temp)
            for name, body in {'gh': FAKE_GH, 'git': FAKE_GIT, 'curl': '#!/bin/bash\n'}.items():
                (temp / name).write_text(body)
                (temp / name).chmod(0o755)
            env = dict(os.environ, PATH=f'{temp}:{os.environ["PATH"]}', CALLS=str(temp / 'calls'),
                       COUNT=str(temp / 'count'), WATCH_1_EXIT=str(watch_exit), RUN_1_STEPS=str(steps))
            result = subprocess.run([ROOT / 'scripts/ios/build-preview.sh'], env=env, text=True,
                                    capture_output=True)
            return result, (temp / 'calls').read_text().splitlines()

    def test_no_runner_retries_on_the_other_pool_then_prints_the_link(self):
        result, calls = self.run_script(watch_exit=1, steps=0)
        self.assertEqual(result.returncode, 0, result.stderr)
        pools = [c.split('runner=')[1] for c in calls if c.startswith('gh workflow run')]
        self.assertEqual(pools, ['macos-26', 'macos-15'])
        self.assertIn('builds%2Fabc-123%2Fmanifest.plist', result.stdout)
        self.assertEqual(calls[-1], 'git push --quiet origin --delete ios-preview')

    def test_real_build_failure_stops_without_retrying(self):
        result, calls = self.run_script(watch_exit=1, steps=9)
        self.assertEqual(result.returncode, 1)
        self.assertIn('build failed', result.stderr)
        self.assertEqual(sum(c.startswith('gh workflow run') for c in calls), 1)
        self.assertEqual(calls[-1], 'git push --quiet origin --delete ios-preview')


if __name__ == '__main__':
    unittest.main()
