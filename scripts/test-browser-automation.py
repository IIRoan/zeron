#!/usr/bin/env python3
"""Real WebKit automation regression probe, with a temporary profile and page.

env -u LD_LIBRARY_PATH python3 scripts/test-browser-automation.py --helper /path/to/zeron-webkit
Requires a display; never attaches to the user's browser profile.
"""
import argparse
import base64
import http.server
import json
from pathlib import Path
import queue
import struct
import subprocess
import tempfile
import threading
import time
import uuid

PAGE = b'''<!doctype html><title>Automation fixture</title><style>body{background:white;color:black}</style>
<h1>Test page</h1><label>Name <input id="name"></label>
<label>Password <input type="password" value="secret-do-not-export"></label>
<button id="save" onclick="window.clicks=(window.clicks||0)+1;window.trusted=event.isTrusted">Save</button>
<select id="choice"><option value="a">Alpha</option><option value="b">Beta</option></select>
<p id="status">Ready</p><script>window.changes=0;document.querySelector('#name').addEventListener('input',()=>window.changes++)</script>'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--helper', required=True)
    args = parser.parse_args()
    template = (Path(__file__).resolve().parents[1] / 'crates/ui/src/browser/automation.js').read_text()

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200)
            self.send_header('Content-Type', 'text/html')
            self.send_header('Content-Length', str(len(PAGE)))
            self.end_headers()
            self.wfile.write(PAGE)

        def log_message(self, *_):
            pass

    with http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler) as server, tempfile.TemporaryDirectory() as profile:
        threading.Thread(target=server.serve_forever, daemon=True).start()
        process = subprocess.Popen([args.helper, '--profile', profile], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        events = queue.Queue()

        def read(length):
            result = bytearray()
            while len(result) < length:
                part = process.stdout.read(length - len(result))
                if not part:
                    raise EOFError()
                result.extend(part)
            return result

        def reader():
            try:
                while True:
                    kind, page, length = struct.unpack('<cII', read(9))
                    data = read(length)
                    if kind != b'F':
                        events.put((kind, data))
            except EOFError:
                events.put((b'X', b''))

        def command(**value):
            data = json.dumps(dict(id=1, **value)).encode()
            process.stdin.write(struct.pack('<I', len(data)) + data)
            process.stdin.flush()

        def evaluate(script=None, screenshot=False):
            identity = str(uuid.uuid4())
            command(cmd='screenshot' if screenshot else 'automate', requestId=identity, script=script or '')
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                kind, data = events.get(timeout=10)
                assert kind != b'X', 'helper exited'
                if kind == b'A':
                    response = json.loads(data)
                    if response['requestId'] == identity:
                        assert 'error' not in response, response
                        return response['result']
            raise AssertionError('automation response timed out')

        def action(**value):
            return evaluate(template.replace('__ZERON_COMMAND__', json.dumps(value)))

        def snapshot():
            result = action(action='snapshot')
            assert 'error' not in result, result
            assert 'secret-do-not-export' not in json.dumps(result), 'password exposed'
            return result

        thread = threading.Thread(target=reader, daemon=True)
        thread.start()
        try:
            assert events.get(timeout=15)[0] == b'R'
            command(cmd='create')
            command(cmd='resize', width=800, height=600, scale=1)
            command(cmd='load', url=f'http://127.0.0.1:{server.server_port}/')
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                kind, data = events.get(timeout=20)
                assert kind != b'X'
                if kind == b'S':
                    state = json.loads(data)
                    assert not state.get('error'), state
                    if state.get('title') == 'Automation fixture' and not state.get('loading'):
                        break
            else:
                raise AssertionError('page did not load')
            tree = snapshot()
            refs = {e['name']:e['ref'] for e in tree['elements'] if 'ref' in e}
            assert 'Name' in refs and 'Save' in refs, tree
            assert action(action='fill', ref=refs['Name'], value='Zeron')['filled']
            assert evaluate("({value:document.querySelector('#name').value,changes:window.changes})") == {'value':'Zeron','changes':1}
            command(cmd='text', text=' native')
            deadline = time.monotonic() + 3
            while evaluate("document.querySelector('#name').value") != 'Zeron native':
                assert time.monotonic() < deadline, 'native text insertion failed'
                time.sleep(.02)
            command(cmd='keypress', key='BackSpace', text='')
            deadline = time.monotonic() + 3
            while evaluate("document.querySelector('#name').value") != 'Zeron nativ':
                assert time.monotonic() < deadline, 'native key press failed'
                time.sleep(.02)
            select = next(e['ref'] for e in tree['elements'] if e['role'] == 'combobox')
            assert action(action='select', ref=select, value='b')['selected'] == 'b'
            assert evaluate("document.querySelector('#choice').value") == 'b'
            point = action(action='click', ref=refs['Save'])['point']
            command(cmd='click', x=point['x'], y=point['y'], button=1)
            deadline = time.monotonic() + 3
            while not evaluate('window.clicks || 0'):
                assert time.monotonic() < deadline, 'native click did not arrive'
                time.sleep(.02)
            assert evaluate('window.trusted'), 'click did not use native input'
            evaluate("document.querySelector('#save').textContent='Changed'")
            assert 'browser_stale_ref' in action(action='click', ref=refs['Save'])['error']
            snapshot()
            assert 'browser_stale_ref' in action(action='fill', ref=refs['Name'], value='wrong')['error']
            image = evaluate(screenshot=True)
            png = base64.b64decode(image['data'])
            assert image['mimeType'] == 'image/png' and png.startswith(b'\x89PNG\r\n\x1a\n')
            dimensions = struct.unpack('>II', png[16:24])
            ratio = evaluate('devicePixelRatio')
            assert dimensions == (int(800 * ratio), int(600 * ratio)), (dimensions, ratio)
            command(cmd='visible', value=0)
            hidden = evaluate(screenshot=True)
            assert base64.b64decode(hidden['data']).startswith(b'\x89PNG'), 'background screenshot failed'
            print(json.dumps(dict(snapshot=True, password_redaction=True, fill=True, native_type=True, native_keypress=True, select=True, native_click=True,
                                  stale_refs=True, screenshot=True, hidden_screenshot=True)), flush=True)
        finally:
            process.stdin.close()
            try:
                assert process.wait(timeout=5) == 0
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
                raise
            thread.join(timeout=2)
            process.stdout.close()
            server.shutdown()


if __name__ == '__main__':
    main()
