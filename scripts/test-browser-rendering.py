#!/usr/bin/env python3
"""Check real helper pixels, resizing, native scrolling and frame backpressure.

Uses a synthetic local page and a temporary profile. --require-gpu also checks
the WPE helper's DMA-BUF diagnostic. Run the software and WPE helpers separately.
"""
import argparse
import http.server
import json
import os
import queue
import struct
import subprocess
import tempfile
import threading
import time
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--helper', required=True)
    parser.add_argument('--require-gpu', action='store_true')
    args = parser.parse_args()
    page = b'''<!doctype html><title>Rendering fixture</title>
    <style>body{margin:0;background:red;height:200vh}#right{position:fixed;left:50%;top:0;width:50%;height:100%;background:blue}</style><div id=right></div>'''

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200)
            self.send_header('Content-Type', 'text/html')
            self.send_header('Content-Length', str(len(page)))
            self.end_headers()
            self.wfile.write(page)

        def log_message(self, *_):
            pass

    with http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler) as server, tempfile.TemporaryDirectory() as profile:
        threading.Thread(target=server.serve_forever, daemon=True).start()
        env = dict(os.environ, ZERON_BROWSER_GPU_STATS='1', G_DEBUG='fatal-criticals')
        process = subprocess.Popen([args.helper, '--profile', profile], env=env,
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        events, errors = queue.Queue(), []
        condition, reading = threading.Condition(), threading.Event()
        reading.set()
        latest, sequence = None, 0

        def exact(length):
            data = bytearray()
            while len(data) < length:
                part = process.stdout.read(length - len(data))
                if not part:
                    raise EOFError()
                data.extend(part)
            return data

        def reader():
            nonlocal latest, sequence
            try:
                while True:
                    reading.wait()
                    kind, identity, length = struct.unpack('<cII', exact(9))
                    data = exact(length)
                    if kind == b'F':
                        with condition:
                            latest = data
                            sequence += 1
                            condition.notify_all()
                    else:
                        events.put((kind, data))
            except EOFError:
                events.put((b'X', b''))

        def stderr():
            for line in process.stderr:
                errors.append(line.decode(errors='replace'))

        threading.Thread(target=reader, daemon=True).start()
        threading.Thread(target=stderr, daemon=True).start()

        def command(**value):
            data = json.dumps(dict(id=1, **value)).encode()
            process.stdin.write(struct.pack('<I', len(data)) + data)
            process.stdin.flush()

        def evaluate(script):
            identity = str(uuid.uuid4())
            command(cmd='automate', requestId=identity, script=script)
            while True:
                kind, data = events.get(timeout=15)
                assert kind != b'X', ''.join(errors)
                if kind == b'A':
                    result = json.loads(data)
                    if result['requestId'] == identity:
                        assert 'error' not in result, result
                        return result['result']

        def pixel(frame, x, y):
            width, height, scale = struct.unpack('<IIf', frame[:12])
            return list(frame[12 + (y * width + x) * 4:12 + (y * width + x + 1) * 4])

        def wait_frame(predicate):
            deadline = time.monotonic() + 15
            with condition:
                while latest is None or not predicate(latest):
                    remaining = deadline - time.monotonic()
                    assert remaining > 0, 'expected frame did not arrive: ' + ''.join(errors)
                    condition.wait(timeout=remaining)

        def split_frame(frame, width, height, scale):
            return (struct.unpack('<IIf', frame[:12]) == (width, height, scale)
                    and all(pixel(frame, 10, y) == [0, 0, 255, 255]
                            and pixel(frame, width - 10, y) == [255, 0, 0, 255]
                            for y in (10, height // 2, height - 10)))

        try:
            assert events.get(timeout=15)[0] == b'R', ''.join(errors)
            command(cmd='create')
            command(cmd='resize', width=801, height=603, scale=1)
            command(cmd='load', url=f'http://127.0.0.1:{server.server_port}/')
            wait_frame(lambda frame: split_frame(frame, 801, 603, 1))
            command(cmd='resize', width=1203, height=707, scale=1.5)
            wait_frame(lambda frame: split_frame(frame, 1203, 707, 1.5))
            # Native scroll must preserve direction and work after scaling.
            command(cmd='scroll', x=20, y=20, dx=0, dy=1)
            deadline = time.monotonic() + 5
            while evaluate('scrollY') <= 0:
                assert time.monotonic() < deadline, 'native scroll did not move down'
                time.sleep(.02)
            command(cmd='visible', value=0)
            evaluate("document.body.style.background='rgb(0,255,0)';document.querySelector('#right').style.background='rgb(0,255,0)';true")
            time.sleep(.3)
            hidden_sequence = sequence
            time.sleep(.3)
            assert sequence <= hidden_sequence + 1, 'hidden tab kept copying frames'
            command(cmd='visible', value=1)
            wait_frame(lambda frame: pixel(frame, 10, 10) == [0, 255, 0, 255])
            evaluate("window.animating=true;window.phase=0;function tick(){if(!window.animating)return;document.body.style.background='rgb('+(++window.phase%255)+',0,0)';requestAnimationFrame(tick)}tick();true")
            time.sleep(.3)
            reading.clear()
            time.sleep(.3)
            command(cmd='automate', requestId='final', script="window.animating=false;document.body.style.background='rgb(255,255,0)';true")
            time.sleep(.3)
            reading.set()
            # The last static update must not get dropped behind a blocked F packet.
            wait_frame(lambda frame: pixel(frame, 10, 10) == [0, 255, 255, 255])
            deadline = time.monotonic() + 3
            while True:
                settled = sequence
                time.sleep(.3)
                if sequence == settled:
                    break
                assert time.monotonic() < deadline, f'idle page kept copying frames ({sequence - settled} in 300 ms)'
            if args.require_gpu:
                assert any('frame: DMA-BUF' in line for line in errors), ''.join(errors)
            print(json.dumps(dict(pixels=True, padded_stride=True, resize=True, fractional_scale=True,
                                  native_scroll=True, hidden_resume=True, latest_frame=True, idle=True,
                                  gpu=args.require_gpu)), flush=True)
        finally:
            reading.set()
            process.stdin.close()
            try:
                status = process.wait(timeout=8)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
                raise AssertionError('helper did not shut down: ' + ''.join(errors))
            assert status == 0, ''.join(errors)
            server.shutdown()


if __name__ == '__main__':
    main()
