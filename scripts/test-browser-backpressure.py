#!/usr/bin/env python3
"""Measure real WebKit input latency while the frame consumer is stalled.

Run with the distribution runtime (unset LD_LIBRARY_PATH), a working display,
and --helper pointing at the compiled Linux browser helper. No user data is used.
"""
import argparse
import http.server
import json
import queue
import struct
import subprocess
import tempfile
import threading
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--helper', required=True)
    parser.add_argument('--allow-stall', action='store_true')
    args = parser.parse_args()
    received = threading.Event()

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path == '/probe':
                received.set()
            body = b'''<!doctype html><title>Ready</title><body style="margin:0">
            <div style="width:100vw;height:100vh;background:linear-gradient(45deg,red,blue)"></div>
            <script>let n=0;function tick(){document.body.firstElementChild.style.filter=
            'hue-rotate('+(++n)+'deg)';requestAnimationFrame(tick)}tick()</script>'''
            self.send_response(200)
            self.send_header('Content-Type', 'text/html')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_):
            pass

    with http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler) as server, tempfile.TemporaryDirectory() as profile:
        threading.Thread(target=server.serve_forever, daemon=True).start()
        process = subprocess.Popen([args.helper, '--profile', profile],
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        events = queue.Queue()
        reading = threading.Event()
        reading.set()
        frames = []

        def read_exact(length):
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
                    reading.wait()
                    kind, page, length = struct.unpack('<cII', read_exact(9))
                    data = read_exact(length)
                    if kind == b'F':
                        frames.append(time.monotonic())
                    else:
                        events.put((kind, data))
            except (EOFError, ValueError):
                events.put((b'X', b''))

        def command(**value):
            data = json.dumps(dict(id=1, **value)).encode()
            process.stdin.write(struct.pack('<I', len(data)) + data)
            process.stdin.flush()

        thread = threading.Thread(target=reader, daemon=True)
        thread.start()
        try:
            assert events.get(timeout=15)[0] == b'R', 'helper failed to start'
            command(cmd='create')
            command(cmd='resize', width=1600, height=1000, scale=1)
            command(cmd='load', url=f'http://127.0.0.1:{server.server_port}/')
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                kind, data = events.get(timeout=20)
                if kind == b'S' and json.loads(data).get('title') == 'Ready':
                    break
                assert kind != b'X', 'helper exited'
            else:
                raise AssertionError('page never loaded')
            start = time.monotonic()
            time.sleep(2)
            fps = sum(t >= start for t in frames) / 2
            reading.clear()
            time.sleep(.3)  # Fill stdout with a frame, independently of input.
            start = time.monotonic()
            command(cmd='eval', script="fetch('/probe'); true")
            responsive = received.wait(2)
            latency = (time.monotonic() - start) * 1000
            print(json.dumps(dict(fps=round(fps, 1), stalled_consumer_input_ms=round(latency, 1),
                                  responsive=responsive)), flush=True)
            assert responsive or args.allow_stall, 'frame backpressure blocked browser input'
        finally:
            reading.set()
            process.stdin.close()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
            thread.join(timeout=2)
            process.stdout.close()
            server.shutdown()


if __name__ == '__main__':
    main()
