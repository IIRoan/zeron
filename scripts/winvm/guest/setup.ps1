$ErrorActionPreference = 'Continue'
Start-Transcript -Path C:\winvm-setup.log -Append
New-Item -Force -ItemType Directory C:\winvm | Out-Null
$src = Split-Path -Parent $MyInvocation.MyCommand.Path
# High performance first: the timeouts below belong to the active scheme. The display must never
# sleep: after it wakes, the screen stays black until every window repaints.
powercfg /setactive 8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c
powercfg /change monitor-timeout-ac 0
powercfg /change monitor-timeout-dc 0
powercfg /change standby-timeout-ac 0
powercfg /change standby-timeout-dc 0
powercfg /hibernate off
# a development VM: no surprise restarts, and nothing replaces the display driver
Stop-Service wuauserv -Force -ErrorAction SilentlyContinue
Set-Service wuauserv -StartupType Disabled
New-Item -Force HKLM:\SOFTWARE\Policies\Microsoft\Edge | Out-Null
Set-ItemProperty HKLM:\SOFTWARE\Policies\Microsoft\Edge HideFirstRunExperience 1 -Type DWord
Set-ItemProperty HKLM:\SOFTWARE\Policies\Microsoft\Edge StartupBoostEnabled 0 -Type DWord
Start-Process msiexec -ArgumentList '/i', "`"$src\OpenSSH-Win64.msi`"", '/qn', '/norestart' -Wait
New-Item -Force -ItemType Directory C:\ProgramData\ssh | Out-Null
Copy-Item "$src\authorized_keys" C:\ProgramData\ssh\administrators_authorized_keys -Force
icacls C:\ProgramData\ssh\administrators_authorized_keys /inheritance:r /grant "*S-1-5-32-544:F" /grant "*S-1-5-18:F"
New-Item -Force HKLM:\SOFTWARE\OpenSSH | Out-Null
New-ItemProperty -Path HKLM:\SOFTWARE\OpenSSH -Name DefaultShell -Value "C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe" -PropertyType String -Force
Set-Service sshd -StartupType Automatic
Start-Service sshd
New-NetFirewallRule -Name winvm-ssh -DisplayName 'winvm ssh' -Enabled True -Direction Inbound -Protocol TCP -Action Allow -LocalPort 22 -Profile Any
Start-Process "$src\virtio-win-guest-tools.exe" -ArgumentList '/install', '/passive', '/norestart' -Wait
# guest resolution follows the host window; clipboard agent talks to the host
New-Item -Force -ItemType Directory 'C:\Program Files\VioGpu' | Out-Null
Copy-Item "$src\vgpusrv.exe", "$src\viogpuap.exe" 'C:\Program Files\VioGpu\' -Force
& 'C:\Program Files\VioGpu\vgpusrv.exe' -i
Set-Service spice-agent -StartupType Automatic
# text copied in Windows goes to the host viewer (QEMU's channel only carries host -> guest with this agent)
Copy-Item "$src\clipwatch.ps1" C:\winvm\ -Force
Set-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name winvm-clip -Value 'conhost.exe --headless powershell -NoProfile -ExecutionPolicy Bypass -File C:\winvm\clipwatch.ps1'
'done' | Out-File C:\winvm-setup.done
Stop-Transcript
