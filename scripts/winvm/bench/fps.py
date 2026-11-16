#!/usr/bin/env python3
"""fps.py [scene ...] — how many frames per second reach this machine from the VM's screen.

Opens a test page (fps.html) in Edge inside the VM, which animates and stamps a frame counter into every frame, and
counts the frames that arrive through QEMU's display (listen.py). Scenes: ui, scroll, canvas, layers (default: the
first three). The VM must be running; its connected ports are replaced for the run and restored afterwards, and the
viewer loses and regains its control channel.

Expected with the VSync driver: 59-60 delivered for ui, scroll and canvas. About 40 means the stock display driver.
"""
import http.server, json, os, signal, subprocess, sys, threading, time
from pathlib import Path

HERE = Path(__file__).resolve().parent
WINVM = str(HERE.parent / 'winvm')
PORT = 8765
EDGE = r'"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"'
posted = {}


class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *a, **k): super().__init__(*a, directory=str(HERE), **k)
    def do_POST(self):
        result = json.loads(self.rfile.read(int(self.headers.get('content-length', 0))))
        posted[result.get('tag')] = result; self.send_response(200); self.end_headers()
    def log_message(self, *a): pass


def winvm(*args, check=True):
    return subprocess.run([WINVM, *args], capture_output=True, text=True, check=check).stdout


def main():
    scenes = sys.argv[1:] or ['ui', 'scroll', 'canvas']
    before = json.loads(winvm('status', '--json'))
    if before['state'] != 'running': sys.exit('the VM is not running (winvm up)')
    server = http.server.ThreadingHTTPServer(('127.0.0.1', PORT), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    ports = lambda to_vm: ['--to-vm', ','.join(map(str, to_vm)), '--from-vm', ','.join(map(str, before['fromVm']))]
    winvm('ports', *ports(sorted(set(before['toVm']) | {PORT})))
    try:
        for scene in scenes:
            tag = f'{scene}-{int(time.time())}'
            winvm('exec', '--', 'taskkill /im msedge.exe /f 2>$null | Out-Null; exit 0', check=False)
            listener = subprocess.Popen([sys.executable, str(HERE / 'listen.py'), '45'], stdout=subprocess.PIPE, text=True)
            time.sleep(1)
            winvm('launch', '--', f'{EDGE} --kiosk "http://localhost:{PORT}/fps.html?tag={tag}&only={scene}" --edge-kiosk-type=fullscreen --no-first-run')
            deadline = time.time() + 40
            while tag not in posted and time.time() < deadline: time.sleep(0.2)
            listener.send_signal(signal.SIGTERM)
            host = json.loads(listener.communicate(timeout=10)[0] or '{}')
            page = (posted.get(tag) or {}).get('scenes') or [{}]
            if not host.get('fps'):
                print(f'{scene:7} no frames counted' + ('' if tag in posted else ' (the page did not finish)'))
                continue
            print(f"{scene:7} delivered {host['fps']:5} fps  (page rendered {page[0].get('fps', '?')}), "
                  f"frame gaps p50 {host['p50']} / p95 {host['p95']} / p99 {host['p99']} ms, {host['dropped_pct']}% not delivered")
    finally:
        winvm('exec', '--', 'taskkill /im msedge.exe /f 2>$null | Out-Null; exit 0', check=False)
        winvm('ports', *ports(before['toVm']), check=False)


if __name__ == '__main__':
    main()
