#!/usr/bin/python3
"""Launch the Linux fork's Zeron (installed as ~/.local/bin/zeron).

  zeron [zeron://… | main | --inspect]   the app's window
  zeron headless | mcp | status | …      the engine's commands, unchanged
  zeron --status | --stop                this install's running window

The app is a window on the engine zeron-host.service keeps running on the
regular profile (~/.zeron, port 27654); it starts none of its own. Its
engine commands go to `zeron-engine` beside it, with this process's
environment as given (an agent's `zeron mcp`, the host's `zeron headless`).
"""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys

home = Path.home()
# Where it was installed (scripts/install-linux-fork.py writes it here), so
# the engine host and agents find it whatever HOME they run with.
INSTALLED = None
base = Path(INSTALLED) if INSTALLED else home / '.local/share/zeron-linux-fork'
release = (base / 'current').resolve(strict=True)
app = release / 'zeron'
data = home / '.zeron'
# The app's own words for a window (src/main.rs `dev_launch`), and a link.
WINDOW = {'main', 'intro', 'onboarding', 'gallery', '--inspect', '--zeron-template', '--safe-mode'}
args = sys.argv[1:]
window = all(arg in WINDOW or arg.startswith('zeron://') for arg in args)

# Everyone: no official updater (this install's own installer updates it),
# Homebrew's tools (git, gh) for desktop launches, and the stack the engine's
# deep futures need.
os.environ['ZERON_AUTO_UPDATE'] = '0'
path = os.environ.get('PATH', '').split(os.pathsep)
os.environ['PATH'] = os.pathsep.join(dict.fromkeys(
    [*path, '/home/linuxbrew/.linuxbrew/bin', '/home/linuxbrew/.linuxbrew/sbin']))
os.environ.setdefault('RUST_MIN_STACK', '16777216')
# A Muute database service saved while the engine was stopped.
pending = data / 'muute-database-service.pending.json'
if pending.exists() and (release / 'configure-muute-service.py').exists():
    subprocess.run(['python3', str(release / 'configure-muute-service.py'), str(data)], check=False)


def windows():
    """This install's app windows: its `zeron` with no engine command."""
    found = []
    for item in Path('/proc').iterdir():
        if not item.name.isdecimal():
            continue
        try:
            exe = (item / 'exe').resolve(strict=True)
            argv = [a.decode() for a in (item / 'cmdline').read_bytes().split(b'\0')[1:] if a]
        except (OSError, RuntimeError):
            continue
        if exe.name == 'zeron' and base / 'releases' in exe.parents \
                and all(a in WINDOW or a.startswith('zeron://') for a in argv):
            found.append(int(item.name))
    return found


if args == ['--status']:
    print(json.dumps({'app': 'Zeron', 'release': str(release), 'profile': str(data),
                      'port': int(os.environ.get('ZERON_IPC_PORT', '27654')), 'windows': windows()}, indent=2))
    raise SystemExit(0)
if args == ['--stop']:
    for pid in windows():
        os.kill(pid, signal.SIGTERM)
        print(f'Stopped Zeron ({pid})')
    raise SystemExit(0)
if not window:
    os.execv(str(app), [str(app), *args])

# A second plain launch goes back to the window there is (Niri focuses the
# exact one); a link goes to the app, which hands it to that window.
running = windows()
if running and not any(arg.startswith('zeron://') for arg in args):
    if os.environ.get('NIRI_SOCKET'):
        try:
            listed = json.loads(subprocess.check_output(
                ['niri', 'msg', '--json', 'windows'], stderr=subprocess.DEVNULL, text=True, timeout=2))
            found = next(w for w in listed if w.get('pid') in running)
            subprocess.run(['niri', 'msg', 'action', 'focus-window', '--id', str(found['id'])],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=2)
        except (OSError, subprocess.SubprocessError, json.JSONDecodeError, StopIteration):
            pass
    raise SystemExit(0)

# The desktop's own variables, without IPC, data or agent overrides another
# Zeron process (a terminal in it) passed on: the window always opens on the
# regular profile. (Not XDG_*_HOME: everything the engine starts must see the
# user's own, or rootless podman sees none of their containers.)
desktop_keys = {'HOME', 'PATH', 'USER', 'LOGNAME', 'LANG', 'SHELL', 'TERM',
                'TMPDIR', 'TZ', 'DISPLAY', 'WAYLAND_DISPLAY', 'XDG_RUNTIME_DIR',
                'DBUS_SESSION_BUS_ADDRESS', 'XDG_CURRENT_DESKTOP',
                'XDG_SESSION_TYPE', 'XDG_SESSION_DESKTOP', 'XDG_SESSION_ID',
                'XDG_ACTIVATION_TOKEN', 'DESKTOP_STARTUP_ID', 'GDK_BACKEND',
                'VK_ICD_FILENAMES', 'DRI_PRIME', 'NIRI_SOCKET',
                'ZERON_AUTO_UPDATE', 'RUST_MIN_STACK'}
env = {key: value for key, value in os.environ.items()
       if key in desktop_keys or key.startswith(('LC_', '__NV_', 'WGPU_'))}
env['PATH'] = os.pathsep.join(dict.fromkeys(
    [str(home / '.local/bin'), str(home / '.cargo/bin'), *env['PATH'].split(os.pathsep)]))
env['ZGX_CHECK'] = '0'
logs = base / 'logs'
logs.mkdir(parents=True, exist_ok=True, mode=0o700)
log = os.open(logs / 'app.log', os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
os.dup2(log, 1)
os.dup2(log, 2)
os.close(log)
os.execve(str(app), [str(app), *args], env)
