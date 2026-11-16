# Windows VM (Linux)

A project can be connected to a Windows machine that runs on this computer:
a QEMU/KVM guest managed by `winvm` (`scripts/winvm`). Its agents can then
start it, run commands in it, and see and drive its screen, and the project's
dev servers are reachable from Windows on the same `localhost` ports.

```sh
zeron vm enable ~/Documents/muute-monorepo --viewer --start-with desktop
zeron vm status
zeron vm disable ~/Documents/muute-monorepo
```

- **Ports are automatic.** Every port a server of the project listens on is
  the same `localhost` port in Windows while it listens: servers in the main
  folder, in any worktree, and in the project's [sandbox](sandboxes.md) (a
  server on `localhost:3000` in the container is `localhost:3000` on this
  machine and in Windows). The engine looks every few seconds and adds or
  drops ports on the live connection; the other ports' open connections are
  not touched, and a server that restarts keeps its port (a port stays 45
  seconds after its server stopped). Nothing is listed per worktree.
  `--no-auto-ports` turns this off.
- `--to-vm`: a port (or `a-b` range) on this machine that the VM always
  reaches, besides the automatic ones: a server that runs outside the
  project's folders. At most 64 ports per direction cross at once.
- `--from-vm`: a VM port this machine reaches on its `localhost`, such as a
  debugger an app in Windows listens on. For a sandboxed project to reach it
  too, add the port to the sandbox's host ports.
- `--viewer`: open the viewer window when the VM starts.
- `--start-with`: the name of one of the project's
  [services](reference/project-terminals.md), such as the Windows app's.
  Starting that service in Zeron also starts the VM and connects the ports,
  in the background: the service does not wait for Windows. Starting all of
  the project's services counts. Stopping a service leaves the VM running.
- Connecting starts nothing and builds nothing. The VM starts with a
  `--start-with` service, from Project settings, with `winvm up`, or when an
  agent calls `vm_start`.
- `enable` replaces the project's whole connection: name every port and
  option each time.
- One VM serves every connected project. Starting it for a project connects
  that project's ports, replacing another project's.

## A sandboxed project, start to finish

A project whose services run in its [sandbox](sandboxes.md): a web app on
port 3352, its API on 4352, and a desktop app that is built and run on
Windows.

1. Build the VM once: `winvm create`, then the 60 fps driver (below).
2. Connect the project, naming the service that means "I am working on the
   Windows app" (its servers' ports need no listing):

   ```sh
   zeron vm enable ~/Documents/muute-monorepo --viewer --start-with "Desktop (Windows)"
   ```

3. Start that service in Zeron. The VM starts with it and its viewer opens.
   In Windows, `http://localhost:3352` is the web app in the container and
   `http://localhost:4352` its API, exactly as on this machine.
4. In a chat of the project, the agent uses `vm_exec` to build and start the
   desktop app, `vm_launch` to show it, and `vm_screenshot` / `vm_input` to
   see and drive it.

## In the app

Project settings → Environment (project defaults) has a **Windows VM** group
with the same settings: automatic ports, more ports into the VM, ports from
the VM, the services the VM starts with, and whether the viewer opens. Its
first row shows the ports connected now. **Connect** / **Apply** saves them; **Start VM**, **Stop VM** and
**Open viewer** act on the VM; **Reconnect ports** gives a running VM this
project's ports (after a change, or after another project used it);
**Disconnect** removes the project's connection and leaves the VM as it is. Agents never reach these settings
(`GetProjectWindowsVm`, `SetProjectWindowsVm` and `ControlProjectWindowsVm` are
app-only).

The workbench's left rail has an **Environment** entry (also under All
views and the tab strip's +). It opens a panel beside the chat with the
project's Windows VM and sandbox as cards, read again every five seconds
while it shows.

- **Windows VM**: its screen, as a picture taken again every five seconds
  (a click opens the live viewer); its state; Windows' edition, uptime and
  who is signed in; how much of the processor, memory and disk C: is taken;
  the ports connected now. **Open viewer**, **Reconnect ports** and **Shut
  down** sit under it; when Windows is off, **Start VM** stands where the
  screen would be.
- **Run in Windows**: a command line. **Run in PowerShell** (or Enter) waits
  up to two minutes and shows the exit code and what was printed; **Start on
  the desktop** starts a program whose windows show on the screen.
- **Health**: the display mode and, each folded with its count, the biggest
  processes, listening ports, stopped automatic services and the last hour's
  errors. It is read when the panel opens; the button beside the heading
  reads it again.
- **Sandbox**: the container's name and state, its config and ports, **Open
  terminal**, **Rebuild** (the image and container again with the settings
  they have; it asks once more first, because it stops what runs in the
  container) and **Show log** (the last build's log).

The gear opens Project settings → Environment, where both are set up. The
VM's actions here are the person's (`ControlProjectWindowsVm` is app-only);
agents have the tools below.

The settings group and the VM's part of the panel are the **Windows VM**
plugin (Settings → Plugins), on by default. Turning it off hides both;
connected projects stay connected, and their agents keep their tools, until
they are disconnected.

## What agents can do

In a connected project, each chat's agent has these tools (`zeron mcp`), also
from inside a project sandbox:

| Tool | Purpose |
| --- | --- |
| `vm_status` | The VM's state (stopped, starting, running), the connected ports, and why ports are not connected. |
| `vm_diagnostics` | The running VM's health in one reading: Windows version, uptime, CPU load, memory and disk free, display mode, the biggest processes, listening ports with their process, stopped automatic services, the last hour's event-log errors. |
| `vm_start` | Start the VM if needed, wait for Windows, connect the project's ports. |
| `vm_stop` | Shut Windows down and stop the VM. |
| `vm_exec` | Run a PowerShell command as the VM's administrator; returns the exit code and the last 100 KB of stdout and stderr. Default timeout 120 s, at most 600 s. |
| `vm_launch` | Start a program on the logged-in desktop, where its windows show. `vm_exec` has no desktop. |
| `vm_screenshot` | The screen as a PNG image, taken from the display itself, so it shows whatever desktop Windows is showing. |
| `vm_input` | Click, double-click, right-click or move at screenshot pixels; type text; press key combinations. |

Reading (`vm_status`, `vm_diagnostics`, `vm_screenshot`) is open to every
chat of the project.
Everything else needs a chat with full access outside read-only and plan
mode. A chat in a project that is not connected gets an error that says how
the person connects it.

## What crosses, and what does not

- The VM reaches the internet, the connected projects' own servers (automatic
  ports) and the `--to-vm` ports. It reaches no other port of this machine:
  not unconnected projects' servers, not databases, not Zeron's own port or
  a login's callback port. `winvm` runs its network without a route to this
  machine's `localhost`; the listed ports are carried by one SSH connection
  that `winvm` opens into the VM.
- The registry (`~/.zeron/windows-vm.json`) is written by `zeron vm` and the
  app. No agent tool changes which project is connected or which ports cross.
- The VM has its own disk. A project's files are not in it unless something
  copies them there (`vm_exec` can fetch, clone or receive files).
- Whatever is stored in the VM is readable by every agent of every connected
  project. Keep credentials there to what the work needs.

## The VM

`winvm` keeps the VM in `~/.local/share/winvm` (`WINVM_HOME`): the disk image
with its snapshots, the SSH key and password of its `dev` administrator, and
`config.json`:

```json
{ "vcpus": 12, "pin": "0-11", "memory": "8G" }
```

`pin` names the host CPUs the VM may use, as `taskset` takes them. The
defaults are 8 virtual CPUs (fewer on small machines), no pinning and 8 GB.

```sh
winvm up --to-vm 3000,3001 --view   # start, wait for Windows, connect ports, open the viewer
winvm status
winvm ports --to-vm 3000,5173       # change the connected ports of a running VM (open connections on kept ports stay)
winvm diag                          # the guest's health as JSON
winvm exec -- 'Get-ComputerInfo | Select-Object OsName'
winvm copy ./installer.exe 'C:/Users/dev/Downloads/'   # --from-vm copies the other way
winvm media ~/Downloads/tools.iso   # attach an ISO as a disc; prints its drive letter
winvm launch -- notepad.exe
winvm screenshot /tmp/vm.png
winvm input click 400 300
winvm input type 'hello'
winvm input key ctrl+s
winvm view                          # the viewer window
winvm snapshot save before-driver   # disk snapshots, with the VM stopped
winvm down
```

Zeron looks for `winvm` in `ZERON_WINVM`, then `~/.local/bin/winvm`, then on
`PATH`. Link the copy in this checkout:

```sh
ln -s ~/Documents/zeron/scripts/winvm/winvm ~/.local/bin/winvm
```

It needs `qemu-system-x86_64` with KVM, `passt`, `swtpm`, `ssh`, and Python 3
with PyGObject and GTK 4 for the viewer; `winvm create` also needs `7z`. When
the system's QEMU has no D-Bus display module (`qemu-ui-dbus`), `winvm`
downloads the package with `dnf` and unpacks the module into
`~/.local/share/winvm-qemu-modules`, without installing anything.

### The viewer

`winvm view` opens a window onto the VM (`scripts/winvm/viewer.py`). The
guest's resolution follows the window, up to 1920×1080. Text copied on either
side can be pasted on the other. The pointer takes the guest's cursor shape.
Closing the window leaves the VM running.

### Building the guest

```sh
winvm create                 # --size 96G by default
```

`create` installs Windows 11 unattended into a new VM in `WINVM_HOME` and
leaves it stopped with a snapshot named `fresh`. It takes about 20 minutes
after the downloads (the Windows 11 Enterprise evaluation ISO, the virtio-win
ISO and the OpenSSH installer, kept in `media/`). The install follows
`scripts/winvm/guest/autounattend.xml` and `setup.ps1`: the virtio drivers
and guest tools, OpenSSH with the VM's key, the resolution service, and
`clipwatch.ps1` (it sends text copied in Windows to the viewer). It refuses
to replace an existing VM. A second VM lives in another folder
(`WINVM_HOME=~/vm2 winvm create`) with its own `ssh_port` and `clip_port` in
`config.json`.

A new guest has the stock display driver. Build the 60 fps one next.

### 60 frames per second

The stock virtio display driver gives Windows no vertical blank, so the
compositor and applications run on unrelated clocks and about a third of the
frames never reach the screen. `scripts/winvm/driver/vsync.patch` adds an
emulated one to the open-source driver
([kvm-guest-drivers-windows](https://github.com/virtio-win/kvm-guest-drivers-windows),
`viogpudo`): a 60 Hz mode, a high-resolution timer that raises the
display-only VSync interrupt on a fixed timeline, and (`VSyncOversample`, 2 by
default) two compositor passes per refresh so that a frame is shown whenever
in the refresh it arrives. With it a browser at default settings delivers 59
to 60 frames per second for ordinary pages; content the guest cannot render
that fast on its CPU stays slower.

```sh
scripts/winvm/driver/rebuild.sh     # the VM must be running
scripts/winvm/bench/fps.py          # frames per second a browser page delivers
```

`rebuild.sh` builds the driver in the guest with Microsoft's Enterprise WDK
and installs it. It expects the EWDK ISO (about 19 GB, from Microsoft's WDK
download page) at `media/ewdk-24h2.iso`, fetches and patches the driver's
source on first use, and keeps the built package in `driver/out`; a rebuild
takes about two minutes. The driver is test-signed with the virtio-win
project's public test certificate, so the guest runs in test-signing mode
(the "Test Mode" watermark) with Secure Boot off. On a new guest the first
run only turns that mode on: restart the VM (`winvm down`, `winvm up`) and
run `rebuild.sh` again. Its registry values (the display adapter's
driver key) are `VSyncRate` (60; 0 turns the emulation off) and
`VSyncOversample` (1 to 4).

## Troubleshooting

- **`vm_status` says unavailable**: `winvm` is not installed where Zeron
  looks, or `~/.local/share/winvm` holds no VM. Run `winvm status` in a
  terminal for its own message.
- **A server's port is not in Windows**: the server must run in the
  project's main folder, one of its worktrees or its sandbox, listen on
  `localhost` or all addresses, and use port 1024 or above. `vm_status` and
  the settings group list the ports connected now; a new server's port
  appears within about ten seconds.
- **Some ports are not connected** (`tunnelError` names them): a port
  is already in use inside Windows (3389 and 5040 usually are), or a
  `--from-vm` port is in use on this machine. The other ports are connected.
  Fix the list, then use **Reconnect ports** (or `vm_start`, or `winvm ports`):
  Windows keeps running.
- **Windows shows a page on `192.168.122.1`**: nothing answers there. Use
  `localhost` with a `--to-vm` port.
- **The VM's network did not start**: another program has the VM's SSH port
  (2222). Set `"ssh_port"` in `config.json`.
- **The VM did not start with its service**: the name under **Start with
  services** must be the service's name as the Services list shows it. The
  engine's log has the reason for a start that failed.
- **A start takes minutes**: the first start after `winvm snapshot save`
  is slow; later ones take about 20 seconds.
- **A screenshot is black, or shows only the clock**: the guest's display
  went to sleep and Windows has not repainted since. `guest/setup.ps1` turns
  display sleep off; in an older guest run
  `powercfg /change monitor-timeout-ac 0` with `vm_exec`, then move a window.
- **A program started with `vm_exec` shows no window**: that session has no
  desktop. Use `vm_launch`.
