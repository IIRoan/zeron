#!/usr/bin/env python3
"""viewer.py — window onto the VM through QEMU's D-Bus display (start the VM with DISPLAY_MODE=dbus).

Frames arrive as dma-bufs (GPU, no copies) or shared memory; the guest resolution follows this window,
never above MAX_W x MAX_H; text clipboard is shared both ways; the guest's cursor shape becomes the pointer."""
import http.server, json, mmap, os, signal, socket, subprocess, sys, threading, time
import gi
gi.require_version('Gtk', '4.0'); gi.require_version('Gdk', '4.0'); gi.require_version('Graphene', '1.0')
from gi.repository import Gdk, Gio, GLib, GObject, Graphene, Gtk
try:
    gi.require_version('GLibUnix', '2.0'); from gi.repository import GLibUnix
    on_signal = GLibUnix.signal_add
except (ValueError, ImportError):          # GLib before 2.80
    on_signal = GLib.unix_signal_add

RUN = os.path.join(os.environ.get('XDG_RUNTIME_DIR', '/tmp'), 'winvm')
MAX_W, MAX_H = int(os.environ.get('MAX_W', 1920)), int(os.environ.get('MAX_H', 1080))
STEP = int(os.environ.get('RES_STEP', 8))          # guest driver only takes sizes on this grid
CON, CLIP = '/org/qemu/Display1/Console_0', '/org/qemu/Display1/Clipboard'
TEXT = 'text/plain;charset=utf-8'
LOG = open(os.path.join(RUN, 'viewer.log'), 'a', buffering=1)
def log(*a): print(time.strftime('%H:%M:%S'), *a, file=LOG)

# evdev keycode -> QEMU key number: the main block is identical, extended keys are 0x80 | scancode
EXT = {96: 0x9c, 97: 0x9d, 98: 0xb5, 99: 0xb7, 100: 0xb8, 102: 0xc7, 103: 0xc8, 104: 0xc9, 105: 0xcb, 106: 0xcd, 107: 0xcf,
       108: 0xd0, 109: 0xd1, 110: 0xd2, 111: 0xd3, 113: 0xa0, 114: 0xae, 115: 0xb0, 119: 0xc6, 125: 0xdb, 126: 0xdc, 127: 0xdd}
def qnum(evdev): return EXT.get(evdev) or (evdev if 0 < evdev < 89 else None)
BUTTONS = {1: 0, 2: 1, 3: 2, 8: 5, 9: 6}           # GDK button -> QEMU (3/4 are wheel up/down)

LISTENER_XML = """<node>
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
CLIP_XML = """<node><interface name="org.qemu.Display1.Clipboard">
 <method name="Register"/><method name="Unregister"/>
 <method name="Grab"><arg type="u" direction="in"/><arg type="u" direction="in"/><arg type="as" direction="in"/></method>
 <method name="Release"><arg type="u" direction="in"/></method>
 <method name="Request"><arg type="u" direction="in"/><arg type="as" direction="in"/><arg type="s" direction="out"/><arg type="ay" direction="out"/></method>
 <property name="Interfaces" type="as" access="read"/>
</interface></node>"""

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
    return Gio.DBusConnection.new_sync(Gio.Socket.new_from_fd(sock.detach()).connection_factory_create_connection(), None, flags, None, None)


class Screen(Gtk.Widget):
    """Draws the latest guest frame, letterboxed; knows how to map pointer positions into the guest."""
    def __init__(self, on_resize):
        super().__init__(); self.tex = None; self.flip = False; self.gw = self.gh = 0; self.on_resize = on_resize
        self.set_focusable(True); self.set_hexpand(True); self.set_vexpand(True)
        self.black = Gdk.RGBA(); self.black.parse('black')

    def dest(self):
        w, h = self.get_width(), self.get_height()
        if not self.gw or not w or not h: return 0, 0, w, h, 1.0
        s = min(w / self.gw, h / self.gh); dw, dh = self.gw * s, self.gh * s
        return (w - dw) / 2, (h - dh) / 2, dw, dh, s

    def to_guest(self, x, y):
        ox, oy, dw, dh, s = self.dest()
        return max(0, min(self.gw - 1, int((x - ox) / s))), max(0, min(self.gh - 1, int((y - oy) / s)))

    def do_size_allocate(self, width, height, baseline): self.on_resize(width, height)

    def do_snapshot(self, snap):
        w, h = self.get_width(), self.get_height()
        snap.append_color(self.black, Graphene.Rect().init(0, 0, w, h))
        if self.tex is None: return
        ox, oy, dw, dh, _ = self.dest()
        if self.flip:
            snap.save(); snap.translate(Graphene.Point().init(0, 2 * oy + dh)); snap.scale(1, -1)
        snap.append_texture(self.tex, Graphene.Rect().init(ox, oy, dw, dh))
        if self.flip: snap.restore()


class Viewer(Gtk.Application):
    def __init__(self):
        super().__init__(application_id='dev.winvm.Viewer', flags=Gio.ApplicationFlags.NON_UNIQUE)
        self.fd = -1; self.builder = None; self.map = None; self.mapinfo = None
        self.pressed = set(); self.serial = 0; self.want = None; self.resize_gen = 0; self.scroll_acc = 0.0
        self.frames = 0; self.dumped = False; self.clip_ready = False; self.reconnecting = False

    # ---- wiring -------------------------------------------------------------------------------------------------
    def do_activate(self):
        self.win = Gtk.ApplicationWindow(application=self, title='Windows VM', default_width=1280, default_height=720)
        self.screen = Screen(self.resized); self.win.set_child(self.screen)
        key = Gtk.EventControllerKey(); key.set_propagation_phase(Gtk.PropagationPhase.CAPTURE)
        key.connect('key-pressed', lambda c, val, code, st: self.key(code, True)); key.connect('key-released', lambda c, val, code, st: self.key(code, False))
        self.win.add_controller(key)
        mot = Gtk.EventControllerMotion(); mot.connect('motion', self.motion); self.screen.add_controller(mot)
        leg = Gtk.EventControllerLegacy(); leg.connect('event', self.legacy); self.screen.add_controller(leg)
        self.win.connect('notify::is-active', lambda *a: self.win.is_active() or self.release_all())
        self.win.present(); GLib.idle_add(lambda: self.connect_qemu() and False)
        GLib.timeout_add_seconds(5, self.stats); self.serve_guest_clipboard()
        GLib.timeout_add_seconds(2, self.health)
        on_signal(GLib.PRIORITY_DEFAULT, signal.SIGUSR1, self.shot)   # kill -USR1: save what the window shows

    def connect_qemu(self):
        a0, a1 = socket.socketpair(); qmp_add_client(a1.fileno()); a1.close()
        self.conn = dbus_over(a0)
        self.conn.register_object(CLIP, Gio.DBusNodeInfo.new_for_xml(CLIP_XML).interfaces[0], self.clip_call, lambda *a: GLib.Variant('as', []), None)
        b0, b1 = socket.socketpair(); fdl = Gio.UnixFDList.new(); idx = fdl.append(b1.fileno())
        self.conn.call_with_unix_fd_list(None, CON, 'org.qemu.Display1.Console', 'RegisterListener', GLib.Variant('(h)', (idx,)), None, 0, -1, fdl, None, None, None)
        b1.close()
        self.lconn = dbus_over(b0, delay=True)
        for i in Gio.DBusNodeInfo.new_for_xml(LISTENER_XML).interfaces:
            self.lconn.register_object('/org/qemu/Display1/Listener', i, self.listener_call, lambda *a: GLib.Variant('as', ['org.qemu.Display1.Listener.Unix.Map']), None)
        self.lconn.start_message_processing()
        # QEMU answers Register by calling back into us synchronously, so only ask once our main loop is serving
        GLib.timeout_add(500, lambda: self.register_clipboard() and False)
        self.conn.connect('closed', lambda *a: self.reconnecting or (log('qemu went away'), self.quit()))
        log('connected')

    # QEMU serves its control objects (console, keyboard, mouse, clipboard) to one client at a time: when another
    # client attaches, ours answer "object does not exist" from then on. Notice and attach again.
    def health(self):
        def cb(conn, res, _):
            try: conn.call_finish(res)
            except GLib.Error as e:
                if conn is self.conn and not self.reconnecting: self.reconnect(e.message)
        self.conn.call(None, CON, 'org.freedesktop.DBus.Properties', 'Get', GLib.Variant('(ss)', ('org.qemu.Display1.Console', 'Label')), None, 0, 2000, None, cb, None)
        return True

    def reconnect(self, why):
        self.reconnecting = True; log('control channel lost, attaching again:', why)
        for c in (self.lconn, self.conn):
            try: c.close_sync(None)
            except GLib.Error: pass
        self.clip_ready = False; self.want = None; self.pressed.clear()
        def again():
            try: self.connect_qemu()
            except Exception as e: log('qemu went away:', repr(e)); self.quit(); return False
            self.reconnecting = False; self.resize_gen += 1
            self.apply_size(self.resize_gen, self.screen.get_width(), self.screen.get_height()); return False
        GLib.timeout_add(300, again)

    def done(self, what):
        def cb(conn, res, _):
            try: conn.call_finish(res)
            except GLib.Error as e: log(what, 'failed:', e.message)
        return cb

    def send(self, iface, method, params):
        self.conn.call(None, CON, 'org.qemu.Display1.' + iface, method, params, None, 0, -1, None, None, None)

    # ---- frames -------------------------------------------------------------------------------------------------
    def listener_call(self, conn, sender, path, iface, method, params, inv):
        try: getattr(self, 'on_' + method, lambda *a: None)(params, inv)
        except Exception as e: log(method, 'error:', repr(e))
        inv.return_value(None)

    def set_guest_size(self, w, h, how):
        if (w, h) != (self.screen.gw, self.screen.gh): log(f'guest {w}x{h} via {how}')
        self.screen.gw, self.screen.gh = w, h

    def on_ScanoutDMABUF(self, params, inv):
        h, w, hh, stride, fourcc, modifier, y0_top = params.unpack()
        fd = inv.get_message().get_unix_fd_list().get(h)
        if self.fd >= 0: os.close(self.fd)
        self.fd = fd; self.map = None
        b = Gdk.DmabufTextureBuilder.new(); b.set_display(self.win.get_display()); b.set_width(w); b.set_height(hh)
        b.set_fourcc(fourcc); b.set_modifier(modifier); b.set_n_planes(1); b.set_fd(0, fd); b.set_stride(0, stride); b.set_offset(0, 0)
        self.builder = b; self.screen.flip = bool(y0_top);   # QEMU: y0_top means the first row in memory is the bottom
        self.y0 = getattr(self, "y0", None)
        if self.y0 != y0_top: self.y0 = y0_top; log("scanout y0_top", y0_top)
        self.set_guest_size(w, hh, f'dmabuf fourcc {fourcc:#x} modifier {modifier:#x} y0_top {y0_top}')
        self.on_UpdateDMABUF(None, None)

    def on_UpdateDMABUF(self, params, inv):
        if self.builder is None: return
        self.screen.tex = self.builder.build(None, None); self.frame()

    def on_ScanoutMap(self, params, inv):
        h, off, w, hh, stride, fmt = params.unpack()
        fd = inv.get_message().get_unix_fd_list().get(h)
        self.map = mmap.mmap(fd, off + stride * hh, mmap.MAP_SHARED, mmap.PROT_READ); os.close(fd)
        self.mapinfo = (off, w, hh, stride); self.builder = None; self.screen.flip = False; self.set_guest_size(w, hh, 'shared memory')
        self.on_UpdateMap(None, None)

    def on_UpdateMap(self, params, inv):
        if self.map is None: return
        off, w, h, stride = self.mapinfo
        self.screen.tex = Gdk.MemoryTexture.new(w, h, Gdk.MemoryFormat.B8G8R8X8, GLib.Bytes.new(self.map[off:off + stride * h]), stride); self.frame()

    def on_Scanout(self, params, inv):
        w, h, stride, fmt = (params.get_child_value(i).unpack() for i in range(4))
        self.screen.tex = Gdk.MemoryTexture.new(w, h, Gdk.MemoryFormat.B8G8R8X8, params.get_child_value(4).get_data_as_bytes(), stride)
        self.builder = self.map = None; self.screen.flip = False; self.set_guest_size(w, h, 'message'); self.frame()

    def on_Disable(self, params, inv): self.screen.tex = None; self.screen.queue_draw()

    def on_CursorDefine(self, params, inv):
        w, h, hx, hy = (params.get_child_value(i).unpack() for i in range(4))
        tex = Gdk.MemoryTexture.new(w, h, Gdk.MemoryFormat.B8G8R8A8_PREMULTIPLIED, params.get_child_value(4).get_data_as_bytes(), w * 4)
        self.screen.set_cursor(Gdk.Cursor.new_from_texture(tex, hx, hy, None))

    def frame(self):
        self.frames += 1; self.screen.queue_draw()
        dump = os.environ.get('DUMP')
        if dump and not self.dumped and self.frames > 30: self.dumped = True; self.screen.tex.save_to_png(dump); log('dumped', dump)

    def shot(self):
        try:
            snap = Gtk.Snapshot(); w, h = self.screen.get_width(), self.screen.get_height()
            Gtk.WidgetPaintable.new(self.screen).snapshot(snap, w, h)
            self.win.get_renderer().render_texture(snap.to_node(), None).save_to_png(os.path.join(RUN, 'shot.png')); log('shot saved', w, h, 'flip', self.screen.flip)
        except Exception as e: log('shot failed:', repr(e))
        return True

    def stats(self):
        if self.frames: log(f'{self.frames / 5:.1f} frames/s  guest {self.screen.gw}x{self.screen.gh}  window {self.screen.get_width()}x{self.screen.get_height()}')
        self.frames = 0; return True

    # ---- resolution follows the window, capped ---------------------------------------------------------------------
    def resized(self, w, h):
        self.resize_gen += 1; gen = self.resize_gen
        GLib.timeout_add(250, lambda: self.apply_size(gen, w, h) and False)

    def apply_size(self, gen, w, h):
        if gen != self.resize_gen or w < 64 or h < 64: return
        s = min(1.0, MAX_W / w, MAX_H / h)
        want = (max(640, int(w * s) // STEP * STEP), max(480, int(h * s) // STEP * STEP))
        if want == self.want: return
        self.want = want; log(f'window {w}x{h} -> ask guest for {want[0]}x{want[1]}')
        self.send('Console', 'SetUIInfo', GLib.Variant('(qqiiuu)', (int(want[0] * 0.2646), int(want[1] * 0.2646), 0, 0, want[0], want[1])))

    # ---- input --------------------------------------------------------------------------------------------------
    def key(self, keycode, down):
        q = qnum(keycode - 8)
        if q is None: return True
        (self.pressed.add if down else self.pressed.discard)(q)
        self.send('Keyboard', 'Press' if down else 'Release', GLib.Variant('(u)', (q,))); return True

    def release_all(self):
        for q in list(self.pressed): self.send('Keyboard', 'Release', GLib.Variant('(u)', (q,)))
        self.pressed.clear()

    def motion(self, ctrl, x, y):
        if self.screen.gw: self.send('Mouse', 'SetAbsPosition', GLib.Variant('(uu)', self.screen.to_guest(x, y)))

    def click(self, button, down): self.send('Mouse', 'Press' if down else 'Release', GLib.Variant('(u)', (button,)))

    def legacy(self, ctrl, ev):
        ev = ev or ctrl.get_current_event()
        if ev is None: return False
        t = ev.get_event_type()
        if t in (Gdk.EventType.BUTTON_PRESS, Gdk.EventType.BUTTON_RELEASE):
            b = BUTTONS.get(ev.get_button())
            if t == Gdk.EventType.BUTTON_PRESS: self.screen.grab_focus()
            if b is not None: self.click(b, t == Gdk.EventType.BUTTON_PRESS)
            return True
        if t == Gdk.EventType.SCROLL:
            d = ev.get_direction()
            if d == Gdk.ScrollDirection.SMOOTH: self.scroll_acc += ev.get_deltas()[1]
            else: self.scroll_acc += {Gdk.ScrollDirection.UP: -1, Gdk.ScrollDirection.DOWN: 1}.get(d, 0)
            while abs(self.scroll_acc) >= 1:
                b = 4 if self.scroll_acc > 0 else 3; self.click(b, True); self.click(b, False)
                self.scroll_acc -= 1 if self.scroll_acc > 0 else -1
            return True
        return False

    # ---- clipboard (text) ---------------------------------------------------------------------------------------
    def serve_guest_clipboard(self):
        # guest -> host: the watcher in Windows (clipwatch.ps1) posts copied text here; loopback only
        app = self
        class H(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                text = self.rfile.read(min(int(self.headers.get('content-length', 0)), 1 << 20)).decode('utf-8', 'replace').replace('\r\n', '\n')
                self.send_response(204); self.end_headers(); GLib.idle_add(lambda: app.guest_copied(text) and False)
            def log_message(self, *a): pass
        try: srv = http.server.ThreadingHTTPServer(('127.0.0.1', int(os.environ.get('CLIP_PORT', 8766))), H)
        except OSError as e: return log('guest->host clipboard off:', e)
        threading.Thread(target=srv.serve_forever, daemon=True).start()

    # The host clipboard goes through wl-copy / wl-paste: unlike a normal Wayland client they work while unfocused.
    def guest_copied(self, text):
        log('guest copied', len(text), 'chars'); self.from_guest = text
        subprocess.run(['wl-copy'], input=text.encode(), timeout=3)

    def host_text(self):
        try:
            r = subprocess.run(['wl-paste', '-n'], capture_output=True, timeout=2)
            if r.returncode: log('wl-paste:', r.stderr.decode(errors='replace').strip()[:120])
            return r.stdout.decode('utf-8', 'replace')
        except Exception as e: log('wl-paste failed:', repr(e)); return ''

    def clip_registered(self, conn, res, _):
        try: conn.call_finish(res)
        except GLib.Error as e:
            # QEMU only offers its clipboard object once the guest agent channel is up: keep trying
            if not getattr(self, 'clip_retry_logged', False): self.clip_retry_logged = True; log('clipboard not available yet, retrying:', e.message)
            GLib.timeout_add_seconds(3, lambda: self.register_clipboard() and False); return
        self.clip_ready = True; log('clipboard sharing on')
        if getattr(self, 'watch', None): return self.host_clip_changed()
        self.watch = Gio.Subprocess.new(['wl-paste', '--watch', 'echo', 'changed'], Gio.SubprocessFlags.STDOUT_PIPE)   # one line per host copy
        lines = Gio.DataInputStream.new(self.watch.get_stdout_pipe())
        def got(stream, res):
            if stream.read_line_finish(res)[0] is None: return
            self.host_clip_changed(); stream.read_line_async(0, None, got)
        lines.read_line_async(0, None, got)

    def register_clipboard(self):
        self.conn.call(None, CLIP, 'org.qemu.Display1.Clipboard', 'Register', None, None, 0, -1, None, self.clip_registered, None)

    def clip_grab_done(self, conn, res, _):
        try: conn.call_finish(res)
        except GLib.Error as e:   # e.g. the guest restarted and QEMU dropped our registration
            log('clipboard offer failed, registering again:', e.message); self.clip_ready = False
            GLib.timeout_add_seconds(2, lambda: self.register_clipboard() and False)

    def host_clip_changed(self):
        text = self.host_text()
        if not self.clip_ready or not text or text == getattr(self, 'from_guest', None): return     # not connected, empty, or our own echo of a guest copy
        self.serial += 1; log('host copied', len(text), 'chars -> offering to guest')
        self.conn.call(None, CLIP, 'org.qemu.Display1.Clipboard', 'Grab', GLib.Variant('(uuas)', (0, self.serial, [TEXT])), None, 0, -1, None, self.clip_grab_done, None)

    def clip_call(self, conn, sender, path, iface, method, params, inv):
        if method == 'Grab':
            sel, serial, mimes = params.unpack(); inv.return_value(None)
            log('guest offers clipboard, serial', serial, mimes)
            if sel != 0 or not any(m.startswith('text/plain') for m in mimes): return
            self.serial = max(self.serial, serial)
            def got(c, res, _):
                try: data = c.call_finish(res).get_child_value(1).get_data_as_bytes().get_data() or b''
                except GLib.Error as e: return log('guest clipboard read failed:', e.message)
                text = data.decode('utf-8', 'replace').rstrip('\0').replace('\r\n', '\n')
                self.guest_copied(text)
            conn.call(None, CLIP, 'org.qemu.Display1.Clipboard', 'Request', GLib.Variant('(uas)', (0, [TEXT])), None, 0, 5000, None, got, None)
        elif method == 'Request':
            sel, mimes = params.unpack()
            if sel == 0:
                text = self.host_text()
                data = GLib.Variant.new_from_bytes(GLib.VariantType('ay'), GLib.Bytes.new(text.encode()), True)
                inv.return_value(GLib.Variant.new_tuple(GLib.Variant.new_string(TEXT), data)); log('guest pasted', len(text), 'chars')
            else: inv.return_dbus_error('org.qemu.Display1.Error.Failed', 'only the clipboard selection is shared')
        else: inv.return_value(None)


if __name__ == '__main__':
    try: sys.exit(Viewer().run(None))
    except Exception as e: log('fatal:', repr(e)); raise
