# install.ps1 — install the vsync-patched virtio display driver from this folder (run elevated inside the VM).
# The driver is test-signed with the virtio-win project's public test certificate, so Windows must be in test-signing
# mode (Secure Boot off): the first run enables it and asks for a reboot; run it again afterwards.
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
certutil -f -addstore Root "$here\VirtIOTestCert.cer" | Out-Null
certutil -f -addstore TrustedPublisher "$here\VirtIOTestCert.cer" | Out-Null
if (-not ((bcdedit /enum '{current}') -match 'testsigning\s+Yes')) { bcdedit /set testsigning on | Out-Null; 'Test signing enabled: reboot, then run this again.'; exit 0 }
$mine = (pnputil /enum-drivers | Out-String) -split '(?m)^\s*$' | Where-Object { $_ -match 'viogpudo.inf' -and $_ -match 'test certificate' } | ForEach-Object { if ($_ -match 'Published Name:\s+(\S+)') { $Matches[1] } }
foreach ($p in $mine) { pnputil /delete-driver $p /uninstall /force | Out-Null }
pnputil /add-driver "$here\viogpudo.inf" /install | Select-String 'installed on device|Failed|error'
# the host viewer shows the guest's pointer shape itself, so let the driver hand it over instead of drawing it into the frame
$k = Get-ChildItem 'HKLM:\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}' -ErrorAction SilentlyContinue | Where-Object { (Get-ItemProperty $_.PSPath -ErrorAction SilentlyContinue).DriverDesc -like '*VirtIO GPU*' }
if ($k) { Set-ItemProperty $k.PSPath -Name HWCursor -Value 1 -Type DWord; pnputil /restart-device (Get-PnpDevice -Class Display | Where-Object { $_.FriendlyName -like '*VirtIO*' }).InstanceId | Out-Null }
