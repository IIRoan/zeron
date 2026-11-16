# Sends text copied in Windows to the host viewer. QEMU's own clipboard channel only works host -> guest
# with the Windows spice agent (it drops guest grabs that carry no serial).
Add-Type -Namespace W -Name C -MemberDefinition @'
[DllImport("user32.dll")] public static extern uint GetClipboardSequenceNumber();
[DllImport("user32.dll")] public static extern IntPtr GetClipboardOwner();
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
'@
$last = [W.C]::GetClipboardSequenceNumber()
while ($true) {
  Start-Sleep -Milliseconds 200
  $n = [W.C]::GetClipboardSequenceNumber()
  if ($n -eq $last) { continue }
  $last = $n
  try {
    $owner = 0; [void][W.C]::GetWindowThreadProcessId([W.C]::GetClipboardOwner(), [ref]$owner)
    if ($owner -and (Get-Process -Id $owner -ErrorAction SilentlyContinue).Name -eq 'vdagent') { continue }   # that is the host's own copy
    $t = Get-Clipboard -Raw -ErrorAction Stop
    if ($t -and $t.Length -lt 1000000) {
      Invoke-WebRequest -UseBasicParsing -Method Post -Uri http://127.0.0.1:8766/clip -Body ([Text.Encoding]::UTF8.GetBytes($t)) -ContentType 'text/plain; charset=utf-8' -TimeoutSec 3 | Out-Null
    }
  } catch {}
}
