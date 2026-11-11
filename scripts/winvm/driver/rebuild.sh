#!/usr/bin/env bash
# rebuild.sh — build the VSync display driver inside the running VM and install it there.
#
# Needs Microsoft's Enterprise WDK ISO (19 GB, https://go.microsoft.com/fwlink/?linkid=2271957) as
# $WINVM_HOME/media/ewdk-24h2.iso. The driver source is fetched on first use: virtio-win's
# kvm-guest-drivers-windows at the commit vsync.patch was written against.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd); WINVM=$HERE/../winvm
VM=${WINVM_HOME:-$HOME/.local/share/winvm}
SRC=$VM/driver/kvm-guest-drivers-windows
COMMIT=defa22a
if [ ! -d "$SRC" ]; then
  mkdir -p "$VM/driver"
  git clone -q https://github.com/virtio-win/kvm-guest-drivers-windows "$SRC"
  git -C "$SRC" checkout -q "$COMMIT"
  git -C "$SRC" apply "$HERE/vsync.patch"
fi
[ -f "$VM/media/ewdk-24h2.iso" ] || { echo "missing $VM/media/ewdk-24h2.iso (see the top of this script)"; exit 1; }
letter=$("$WINVM" media "$VM/media/ewdk-24h2.iso")
[ -n "$letter" ] || { echo "the driver kit did not show up in the VM"; exit 1; }
tar -C "$VM/driver" --exclude=.git -czf "$VM/driver/src.tgz" kvm-guest-drivers-windows
"$WINVM" exec -- 'New-Item -Force -ItemType Directory C:\src | Out-Null; if (Test-Path C:\src\kvm-guest-drivers-windows) { Remove-Item -Recurse -Force C:\src\kvm-guest-drivers-windows }'
"$WINVM" copy "$VM/driver/src.tgz" "$HERE/build.cmd" 'C:\src'
"$WINVM" exec --timeout 600 -- "cd C:\\src; tar -xzf src.tgz; cmd /c \"C:\\src\\build.cmd $letter\" | Select-Object -Last 3"
mkdir -p "$VM/driver/out"
"$WINVM" copy --from-vm 'C:\winvm\viogpudo-vsync\viogpudo.sys' 'C:\winvm\viogpudo-vsync\viogpudo.inf' 'C:\winvm\viogpudo-vsync\viogpudo.cat' "$VM/driver/out/"
cp "$SRC/build/VirtIOTestCert.cer" "$HERE/install.ps1" "$VM/driver/out/"
"$WINVM" copy "$VM/driver/out/install.ps1" "$VM/driver/out/VirtIOTestCert.cer" 'C:\winvm\viogpudo-vsync'
"$WINVM" exec --timeout 300 -- 'powershell -NoProfile -ExecutionPolicy Bypass -File C:\winvm\viogpudo-vsync\install.ps1; Start-Sleep 4; Get-CimInstance Win32_VideoController | ForEach-Object { $_.Name + " " + $_.CurrentHorizontalResolution + "x" + $_.CurrentVerticalResolution + "@" + $_.CurrentRefreshRate + " " + $_.Status }'
rm -f "$VM/driver/src.tgz"
