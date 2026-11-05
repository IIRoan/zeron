#!/usr/bin/python3
"""Launch the local Linux beta with its own engine and UI profile."""
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys

base = Path.home() / '.local/share/zeron-beta'
release = (base / 'current').resolve(strict=True)
profile = base / 'profile'
port = 27946


def processes():
    found = []
    for item in Path('/proc').iterdir():
        if not item.name.isdecimal():
            continue
        try:
            exe = (item / 'exe').resolve(strict=True)
            if exe.parent.parent.parent != base / 'releases':
                continue
            if exe.name not in {'zeron-beta', 'zeron-engine'}:
                continue
            env = (item / 'environ').read_bytes().split(b'\0')
            if ('ZERON_DATA_DIR=' + str(profile / 'engine')).encode() in env:
                found.append((int(item.name), exe.name))
        except (OSError, RuntimeError):
            continue
    return found


args = sys.argv[1:]
if args == ['--status']:
    print(json.dumps({'app': 'Zeron Beta', 'release': str(release),
                      'profile': str(profile), 'port': port,
                      'shared_agent_logins': True, 'processes': processes()}, indent=2))
    raise SystemExit(0)
if args == ['--stop']:
    for pid, name in processes():
        os.kill(pid, signal.SIGTERM)
        print(f'Stopped {name} ({pid})')
    raise SystemExit(0)

# A second desktop launch returns to this beta rather than attempting to
# claim its application id again. Niri can focus the exact owned window;
# other desktops retain their existing instance without launching another.
gui = not args or args[0] in {'main', 'intro', 'onboarding', 'gallery', '--inspect', '--zeron-template'}
running = next((pid for pid, name in processes() if name == 'zeron-beta'), None)
if gui and running:
    if os.environ.get('NIRI_SOCKET'):
        try:
            windows = json.loads(subprocess.check_output(
                ['niri', 'msg', '--json', 'windows'], stderr=subprocess.DEVNULL, text=True, timeout=2))
            window = next(w for w in windows if w.get('pid') == running)
            subprocess.run(['niri', 'msg', 'action', 'focus-window', '--id', str(window['id'])],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=2)
        except (OSError, subprocess.SubprocessError, json.JSONDecodeError, StopIteration):
            pass
    raise SystemExit(0)

profile.mkdir(parents=True, exist_ok=True, mode=0o700)
profile.chmod(0o700)
for folder in ['engine', 'config', 'data', 'cache', 'logs', 'projects']:
    (profile / folder).mkdir(parents=True, exist_ok=True, mode=0o700)

# Keep desktop integration and the user's normal CLI login locations, while
# discarding IPC/data/agent overrides inherited from another Zeron process.
desktop_keys = {'HOME', 'PATH', 'USER', 'LOGNAME', 'LANG', 'SHELL', 'TERM',
                'TMPDIR', 'TZ', 'DISPLAY', 'WAYLAND_DISPLAY', 'XDG_RUNTIME_DIR',
                'DBUS_SESSION_BUS_ADDRESS', 'XDG_CURRENT_DESKTOP',
                'XDG_SESSION_TYPE', 'XDG_SESSION_DESKTOP', 'XDG_SESSION_ID',
                'XDG_ACTIVATION_TOKEN', 'DESKTOP_STARTUP_ID', 'GDK_BACKEND',
                'VK_ICD_FILENAMES', 'DRI_PRIME'}
env = {key: value for key, value in os.environ.items()
       if key in desktop_keys or key.startswith(('LC_', '__NV_', 'WGPU_'))}
paths = [str(Path.home() / '.local/bin'), str(Path.home() / '.cargo/bin'),
         '/var/home/linuxbrew/.linuxbrew/bin', '/usr/local/bin', '/usr/bin', '/bin']
env['PATH'] = os.pathsep.join(dict.fromkeys([*paths, *env.get('PATH', '').split(os.pathsep)]))
env.update({'ZERON_DATA_DIR': str(profile / 'engine'),
            'ZERON_IPC_PORT': str(port), 'ZERON_AUTO_UPDATE': '0',
            'XDG_CONFIG_HOME': str(profile / 'config'),
            'XDG_DATA_HOME': str(profile / 'data'),
            'XDG_CACHE_HOME': str(profile / 'cache'),
            'RUST_MIN_STACK': '16777216', 'ZGX_CHECK': '0'})
if not args or all(arg in {'--inspect', '--zeron-template'} for arg in args):
    args.insert(0, 'main')
if args[0] in {'main', 'intro', 'onboarding', 'gallery'}:
    with socket.socket() as probe:
        occupied = probe.connect_ex(('127.0.0.1', port)) == 0
    if occupied and not any(name == 'zeron-engine' for _, name in processes()):
        raise SystemExit(f'Beta port {port} is in use by another process; it was left untouched.')
    log = os.open(profile / 'logs/app.log', os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    os.dup2(log, 1)
    os.dup2(log, 2)
    os.close(log)
binary = release / 'bin/zeron-beta'
os.chdir(profile / 'projects')
os.execve(binary, [str(binary), *args], env)
