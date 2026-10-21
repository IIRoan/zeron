"""Exercise the Xcode build script with isolated Cargo/bindgen stand-ins."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'ios/build-core.sh'
CARGO = r'''#!/bin/bash
set -eu
printf '%s\n' "$*" >> "$IOS_CORE_TEST_LOG"
target=
profile=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --target) target="$2"; shift ;;
    --profile) profile="$2"; shift ;;
  esac
  shift
done
if [ -n "$target" ]; then
  mkdir -p "$CARGO_TARGET_DIR/$target/$profile"
  printf '%s' "$target" > "$CARGO_TARGET_DIR/$target/$profile/libzeron_mobile.a"
else
  mkdir -p "$CARGO_TARGET_DIR/mobile"
  cat > "$CARGO_TARGET_DIR/mobile/uniffi-bindgen" <<'GEN'
#!/bin/bash
set -eu
while [ "$#" -gt 0 ]; do
  if [ "$1" = --out-dir ]; then out="$2"; shift; fi
  shift
done
mkdir -p "$out"
for file in zeron_coreFFI.h zeron_coreFFI.modulemap zeron_core.swift; do
  printf 'generated binding\n' > "$out/$file"
done
GEN
  chmod +x "$CARGO_TARGET_DIR/mobile/uniffi-bindgen"
fi
'''


class IosCoreBuildTests(unittest.TestCase):
    def run_build(self, platform, architecture, configuration='Debug'):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        script = root / 'scripts/ios/build-core.sh'
        script.parent.mkdir(parents=True)
        shutil.copyfile(SCRIPT, script)
        task_tools = root / 'test-tools'
        cargo = task_tools / 'cargo'
        cargo.parent.mkdir(parents=True)
        cargo.write_text(CARGO)
        cargo.chmod(0o755)
        # Intercept only the script's clean-environment Cargo launch, keeping the
        # actual HOME and installed toolchains untouched.
        launcher = task_tools / 'env'
        launcher.write_text('''#!/bin/bash
set -eu
while [ "$1" != cargo ]; do
  case "$1" in CARGO_TARGET_DIR=*) export "$1" ;; esac
  shift
done
shift
exec "$IOS_CORE_TEST_CARGO" "$@"
''')
        launcher.chmod(0o755)
        environment = {**os.environ, 'PATH': str(task_tools) + os.pathsep + os.environ['PATH'],
                       'IOS_CORE_TEST_CARGO': str(cargo),
                       'IOS_CORE_TEST_LOG': str(task_tools / 'cargo-calls'),
                       'ARCHS': architecture, 'CONFIGURATION': configuration, 'ZERON_SKIP_CORE': '0'}
        result = subprocess.run(['bash', str(script), platform], env=environment,
                                text=True, capture_output=True)
        return result, root, task_tools

    def test_intel_and_arm_simulators_use_matching_rust_targets(self):
        for arch, target in [('x86_64', 'x86_64-apple-ios'), ('arm64', 'aarch64-apple-ios-sim')]:
            with self.subTest(arch=arch):
                result, root, task_tools = self.run_build('iphonesimulator', arch)
                self.assertEqual(result.returncode, 0, result.stderr)
                library = root / 'target/ios-core/iphonesimulator/libzeron_mobile.a'
                self.assertEqual(library.read_text(), target)
                calls = (task_tools / 'cargo-calls').read_text()
                self.assertIn(f'--profile mobile --target {target}', calls)
                self.assertIn('--bin uniffi-bindgen', calls)
                self.assertTrue((root / 'apps/ios/Zeron/Core/Generated/zeron_core.swift').is_file())

    def test_iphone_archive_stays_arm64_on_an_intel_host(self):
        result, root, task_tools = self.run_build('iphoneos', 'x86_64', 'Release')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((root / 'target/ios-core/iphoneos/libzeron_mobile.a').read_text(),
                         'aarch64-apple-ios')
        self.assertIn('--profile mobile-dist --target aarch64-apple-ios',
                      (task_tools / 'cargo-calls').read_text())

    def test_unsupported_simulator_architecture_fails_before_build(self):
        result, _, task_tools = self.run_build('iphonesimulator', 'i386')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('set ARCHS', result.stderr)
        self.assertFalse((task_tools / 'cargo-calls').exists())


if __name__ == '__main__':
    unittest.main()
