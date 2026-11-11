#!/usr/bin/env python3
"""listen.py <seconds> — attach to QEMU's D-Bus display and count the frames that reach this machine (used by fps.py).
The guest page stamps a 16-bit frame counter + 4-bit scene id into the top-left pixels of every frame; this prints,
as JSON, how many distinct ones arrived and how they were spaced. Stops early on SIGTERM."""
import json, mmap, os, signal, socket, sys, time
import gi
gi.require_version('Gio', '2.0')
from gi.repository import Gio, GLib
try:
    gi.require_version('GLibUnix', '2.0'); from gi.repository import GLibUnix
    on_signal = GLibUnix.signal_add
except (ValueError, ImportError):          # GLib before 2.80
    on_signal = GLib.unix_signal_add

RUN = os.path.join(os.environ.get('XDG_RUNTIME_DIR', '/tmp'), 'winvm')
LIMIT = float(sys.argv[1]) if len(sys.argv) > 1 else 60

def qmp_add_client(fd):
    s = socket.socket(socket.AF_UNIX); s.connect(RUN + '/qmp.sock'); f = s.makefile('r'); f.readline()
    def call(obj, fds=()):
        data = (json.dumps(obj) + '\n').encode()
        socket.send_fds(s, [data], list(fds)) if fds else s.sendall(data)
        while True:
            r = json.loads(f.readline())
            if 'return' in r or 'error' in r:
                assert 'return' in r, r; return r
    call({'execute': 'qmp_capabilities'})
    call({'execute': 'getfd', 'arguments': {'fdname': 'dbusfd'}}, [fd])
    call({'execute': 'add_client', 'arguments': {'protocol': '@dbus-display', 'fdname': 'dbusfd'}})

def dbus_over(sock, delay=False):
    flags = Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | (Gio.DBusConnectionFlags.DELAY_MESSAGE_PROCESSING if delay else 0)
    stream = Gio.Socket.new_from_fd(sock.detach()).connection_factory_create_connection()
    return Gio.DBusConnection.new_sync(stream, None, flags, None, None)

XML = """<node>
<interface name="org.qemu.Display1.Listener">
 <method name="Scanout"><arg type="u" direction="in"/><arg type="u" direction="in"/><arg type="u" direction="in"/><arg type="u" direction="in"/><arg type="ay" direction="in"/></method>
 <method name="Update"><arg type="i" direction="in"/><arg type="i" direction="in"/><arg type="i" direction="in"/><arg type="i" direction="in"/><arg type="u" direction="in"/><arg type="u" direction="in"/><arg type="ay" direction="in"/></method>
 <method name="ScanoutDMABUF"><arg type="h" direction="in"/><arg type="u" direction="in"/><arg type="u" direction="in"/><arg type="u" direction="in"/><arg type="u" direction="in"/><arg type="t" direction="in"/><arg type="b" direction="in"/></method>
 <method name="UpdateDMABUF"><arg type="i" direction="in"/><arg type="i" direction="in"/><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
 <method name="Disable"/>
 <method name="MouseSet"><arg type="i" direction="in"/><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
 <method name="CursorDefine"><arg type="i" direction="in"/><arg type="i" direction="in"/><arg type="i" direction="in"/><arg type="i" direction="in"/><arg type="ay" direction="in"/></method>
 <property name="Interfaces" type="as" access="read"/>
</interface>
<interface name="org.qemu.Display1.Listener.Unix.Map">
 <method name="ScanoutMap"><arg type="h" direction="in"/><arg type="u" direction="in"/><arg type="u" direction="in"/><arg type="u" direction="in"/><arg type="u" direction="in"/><arg type="u" direction="in"/></method>
 <method name="UpdateMap"><arg type="i" direction="in"/><arg type="i" direction="in"/><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
</interface></node>"""

st = {'map': None, 'off': 0, 'stride': 0, 'w': 0, 'h': 0, 'calls': {}, 'frames': [], 'last': None, 'area': 0, 'done': False}
loop = GLib.MainLoop()

def decode():
    m, base = st['map'], st['off'] + 8 * st['stride']
    v = 0
    for i in range(24):
        if m[base + (i * 16 + 8) * 4] > 127: v |= 1 << i
    return v

def on_call(conn, sender, path, iface, method, params, inv):
    st['calls'][method] = st['calls'].get(method, 0) + 1
    now = time.perf_counter()
    if method == 'ScanoutMap':
        h, off, w, hh, stride, fmt = params.unpack()
        fd = inv.get_message().get_unix_fd_list().get(h)
        st.update(map=mmap.mmap(fd, off + stride * hh, mmap.MAP_SHARED, mmap.PROT_READ), off=off, stride=stride, w=w, h=hh)
        os.close(fd)
    elif method == 'UpdateMap' and st['map'] is not None:
        x, y, w, h = params.unpack(); st['area'] += w * h
        v = decode()
        if (v >> 20) == 0b1010 and v != st['last']:
            st['last'] = v; scene = (v >> 16) & 15
            st['frames'].append((now, scene, v & 0xffff))
            if scene == 15: st['done'] = True; GLib.timeout_add(300, loop.quit)
    elif method in ('Scanout', 'Update'):
        st['calls']['_nomap'] = st['calls'].get('_nomap', 0) + 1
    inv.return_value(None)

def on_get(conn, sender, path, iface, prop):
    return GLib.Variant('as', ['org.qemu.Display1.Listener.Unix.Map'])

a0, a1 = socket.socketpair(); qmp_add_client(a1.fileno()); a1.close()
conn = dbus_over(a0)
b0, b1 = socket.socketpair()
fdl = Gio.UnixFDList.new(); idx = fdl.append(b1.fileno())
conn.call_with_unix_fd_list(None, '/org/qemu/Display1/Console_0', 'org.qemu.Display1.Console', 'RegisterListener', GLib.Variant('(h)', (idx,)), None, 0, -1, fdl, None, None, None)
b1.close()
lconn = dbus_over(b0, delay=True)
for i in Gio.DBusNodeInfo.new_for_xml(XML).interfaces: lconn.register_object('/org/qemu/Display1/Listener', i, on_call, on_get, None)
lconn.start_message_processing()
on_signal(GLib.PRIORITY_DEFAULT, signal.SIGTERM, loop.quit)
GLib.timeout_add(int(LIMIT * 1000), loop.quit)
loop.run()

rows = [f for f in st['frames'] if f[1] != 15]
out = {'delivered': 0}
if len(rows) > 3:
    cut = rows[0][0] + 1.5                               # the page's own warm-up
    t = [r[0] for r in rows if r[0] >= cut]; c = [r[2] for r in rows if r[0] >= cut]
    if len(t) > 3:
        d = sorted((b - a) * 1000 for a, b in zip(t, t[1:])); p = lambda q: round(d[min(len(d) - 1, int(len(d) * q))], 1)
        produced = ((c[-1] - c[0]) & 0xffff) + 1
        out = {'delivered': len(t), 'fps': round((len(t) - 1) / (t[-1] - t[0]), 1), 'rendered': produced,
               'dropped_pct': round(100 * (produced - len(t)) / produced, 1), 'p50': p(.5), 'p95': p(.95), 'p99': p(.99), 'max': round(d[-1], 1)}
print(json.dumps(out))
