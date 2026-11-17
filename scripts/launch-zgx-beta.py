#!/usr/bin/python3
"""Launch the local Linux beta with its own engine and UI profile, or as a
window on regular Zeron's profile and engine.

  zeron-beta [main|--inspect|...]   the app
  zeron-beta --use-regular-profile  from now on, open on regular Zeron's
                                    profile (~/.zeron): its chats, projects and
                                    the engine zeron-host.service keeps running
  zeron-beta --use-own-profile      back to the beta's own profile
  zeron-beta --status | --stop
"""
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import time

base = Path.home() / '.local/share/zeron-beta'
release = (base / 'current').resolve(strict=True)
profile = base / 'profile'
port = 27946
# (Linux fork) A window on regular Zeron's profile: its engine is the one
# zeron-host.service runs (the regular release's), never the beta's own.
regular_marker = base / 'regular-profile'
regular = regular_marker.exists()
regular_data = Path.home() / '.zeron'
regular_port = 27654
data_dir = regular_data if regular else profile / 'engine'
if regular:
    port = regular_port


def processes():
    found = []
    for item in Path('/proc').iterdir():
        if not item.name.isdecimal():
            continue
        try:
            exe = (item / 'exe').resolve(strict=True)
            if base / 'releases' not in exe.parents:
                continue
            if exe.name not in {'zeron-beta', 'zeron-engine'}:
                continue
            env = (item / 'environ').read_bytes().split(b'\0')
            if ('ZERON_DATA_DIR=' + str(data_dir)).encode() in env:
                found.append((int(item.name), exe.name))
        except (OSError, RuntimeError):
            continue
    return found


def desktop_app_running():
    """Regular Zeron's own desktop app (it may host the engine itself)."""
    for item in Path('/proc').iterdir():
        if not item.name.isdecimal():
            continue
        try:
            exe = (item / 'exe').resolve(strict=True)
            argv = (item / 'cmdline').read_bytes().split(b'\0')
        except (OSError, RuntimeError):
            continue
        if exe.name == 'zeron' and 'zeron-linux-fork' in str(exe) and b'headless' not in argv and b'mcp' not in argv:
            return int(item.name)
    return None


def listening(at):
    with socket.socket() as probe:
        probe.settimeout(1)
        return probe.connect_ex(('127.0.0.1', at)) == 0


def use_regular_profile():
    """Switch to regular Zeron's profile: seed what the beta needs there (only
    what's missing) while its engine is stopped, then start that engine again
    on the installed regular release."""
    if not regular_data.is_dir():
        raise SystemExit(f'{regular_data} does not exist.')
    pid = desktop_app_running()
    if pid:
        raise SystemExit(f'Quit regular Zeron\'s desktop app first (pid {pid}): it may be running the engine itself.')
    if any(name == 'zeron-beta' for _, name in processes()):
        raise SystemExit('Quit Zeron Beta first (zeron-beta --stop).')
    if '--yes' not in sys.argv:
        print('This restarts regular Zeron\'s engine (zeron-host.service): running agent turns there stop.\n'
              'Run again with --yes to go ahead.')
        raise SystemExit(1)
    subprocess.run(['systemctl', '--user', 'stop', 'zeron-host.service'], check=True)
    for _ in range(60):
        if not listening(regular_port):
            break
        time.sleep(1)
    else:
        raise SystemExit(f'Something still serves port {regular_port}; nothing was changed.')
    # Zeron's own Git and GitHub plugins stay off, as in the beta's profile
    # (Zeron Git replaces them). Only a profile without decisions is seeded.
    state = regular_data / 'plugins/.state.json'
    if not state.exists():
        state.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        decisions = {p: {'allowed': True, 'off': True} for p in ['git', 'github']}
        state.write_text(json.dumps({'own': [], 'decisions': decisions}, indent=2) + '\n')
        state.chmod(0o600)
        print(f'Seeded {state}: stock Git and GitHub plugins off.')
    # The beta's theme, unless regular Zeron has one of its own.
    theme = regular_data / 'theme-choice.json'
    mine = profile / 'engine/theme-choice.json'
    if not theme.exists() and mine.exists():
        theme.write_bytes(mine.read_bytes())
        theme.chmod(0o600)
        print(f'Copied the beta\'s theme to {theme}.')
    regular_marker.write_text('regular\n')
    subprocess.run(['systemctl', '--user', 'start', 'zeron-host.service'], check=True)
    for _ in range(90):
        if listening(regular_port):
            break
        time.sleep(1)
    print('Zeron Beta now opens on regular Zeron\'s profile. Back: zeron-beta --use-own-profile')


args = sys.argv[1:]
if args and args[0] == '--use-regular-profile':
    use_regular_profile()
    raise SystemExit(0)
if args == ['--use-own-profile']:
    if any(name == 'zeron-beta' for _, name in processes()):
        raise SystemExit('Quit Zeron Beta first (zeron-beta --stop).')
    regular_marker.unlink(missing_ok=True)
    print('Zeron Beta opens on its own profile again.')
    raise SystemExit(0)
if args == ['--status']:
    print(json.dumps({'app': 'Zeron Beta', 'release': str(release),
                      'profile': str(data_dir if regular else profile), 'port': port,
                      'regular_profile': regular,
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
# (Not XDG_*_HOME: the engine, and every service, check, terminal and
# agent it starts, must see the user's own: under the beta's folders
# rootless podman saw none of the user's containers, so a project's shared
# databases never passed their ready check. ZERON_DATA_DIR, the port and the
# app id keep the beta apart.)
env.update({'ZERON_DATA_DIR': str(data_dir),
            'ZERON_IPC_PORT': str(port), 'ZERON_AUTO_UPDATE': '0',
            'RUST_MIN_STACK': '16777216', 'ZGX_CHECK': '0'})
if regular:
    env['ZERON_BETA_PROFILE'] = 'regular'
if not args or all(arg in {'--inspect', '--zeron-template'} for arg in args):
    args.insert(0, 'main')
if args[0] in {'main', 'intro', 'onboarding', 'gallery'}:
    with socket.socket() as probe:
        occupied = probe.connect_ex(('127.0.0.1', port)) == 0
    if occupied and not regular and not any(name == 'zeron-engine' for _, name in processes()):
        raise SystemExit(f'Beta port {port} is in use by another process; it was left untouched.')
    log = os.open(profile / 'logs/app.log', os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    os.dup2(log, 1)
    os.dup2(log, 2)
    os.close(log)
# Releases since 0.3.7 are flat (ui/ beside the app); older ones kept bin/.
binary = release / 'zeron-beta' if (release / 'zeron-beta').exists() else release / 'bin/zeron-beta'
os.chdir(profile / 'projects')
os.execve(binary, [str(binary), *args], env)
