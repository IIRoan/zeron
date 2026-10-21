# Browser on Linux

The sidebar browser uses a separate WebKit helper. Builds with WPE WebKit 2.54
or newer prefer its GPU compositor. Builds without WPE use the WebKitGTK 4.1
offscreen renderer. The Linux installer does not install browser dependencies.
Keep the distribution's WebKitGTK 4.1 and JSON-GLib runtime packages available
for the GTK helper:

On Ubuntu or Debian:

```sh
sudo apt install libwebkit2gtk-4.1-0 libjson-glib-1.0-0
```

On Fedora:

```sh
sudo dnf install webkit2gtk4.1 json-glib
```

Zeron starts its browser helper when a page is first opened. The main application does not link to GTK or WebKit, so other app features remain available if the browser runtime is missing. WebKit runs in a separate process and uses a persistent website-data context shared by tabs and windows in the same Zeron profile. Persistent cookies, localStorage and IndexedDB survive closing tabs and restarting Zeron. See [browser profiles and login persistence](browser-profiles.md) for storage locations, profile isolation, and regression tests.

The helper sends live offscreen frames to GPUI, which draws the page alongside
the rest of the app. Both X11 and Wayland use this path, including clipping,
sidebar transitions, tooltips, and frosted overlays. WPE renders through EGL and
delivers composited DMA-BUF buffers; the bridge maps and copies their pixels.
Frame copying and texture uploads remain, so this is not a zero-copy GPU import.
Hidden tabs stop copying and each page retains at most one outgoing frame and
the newest waiting compositor buffer. Input does not wait for a blocked frame
reader. WPE also supports software rendering when hardware is unavailable.

For development, install the development packages in addition to the normal GPUI build dependencies. These provide the `webkit2gtk-4.1` and `json-glib-1.0` pkg-config modules used to compile the helper.

On Ubuntu or Debian:

```sh
sudo apt install libwebkit2gtk-4.1-dev libjson-glib-dev
```

On Fedora:

```sh
sudo dnf install webkit2gtk4.1-devel json-glib-devel
```

The build embeds the helper executables, which are extracted to the user's cache
directory when needed. If `wpe-webkit-2.0 >= 2.54` is discoverable by pkg-config,
the WPE helper is built alongside GTK. A system WPE installation remains managed
by the distribution.

## Private WPE runtime

For Linux x86_64 systems without a suitable WPE package, prepare an isolated
runtime from checksum-pinned Debian packages. The preparation script is check-only
by default. `--apply` downloads and extracts into the requested directory without
installing system libraries:

```sh
python3 scripts/prepare-browser-runtime.py --output target/browser-runtime --apply
source scripts/linux-env.sh
export PKG_CONFIG_PATH="$PWD/target/browser-runtime/pkgconfig:$PKG_CONFIG_PATH"
cargo build --locked -p zeron
ZERON_BROWSER_WPE_RUNTIME="$PWD/target/browser-runtime" target/debug/zeron
```

To include this runtime in a Linux archive:

```sh
ZERON_BROWSER_WPE_RUNTIME="$PWD/target/browser-runtime" scripts/package-linux.sh
```

The archive places `browser-runtime` beside the executable, where Zeron discovers
it automatically. It preserves the packages' copyright files under
`browser-runtime/usr/share/doc`. Bundled WebKit updates require preparing and
shipping a new runtime; distribution updates do not update this copy.

The private runtime requires `bwrap`, permission to create its namespaces, and
compatible system graphics, GLib, GStreamer and other shared dependencies. It was
validated on Fedora 44 x86_64; it is not a self-contained runtime for every Linux
distribution. It mounts its libraries inside the helper's private namespace,
keeps the host filesystem read-only except for the browser profile, and exposes
`/dev/dri` for rendering. It does not attach to an X11 or Wayland display. The
main application continues using its normal libraries.

Set `ZERON_BROWSER_GPU_STATS=1` before launch to log the selected render node and
whether actual frames are DMA-BUF or shared memory. A render node alone does not
prove hardware rendering; inspect the frame diagnostic too. For troubleshooting,
`ZERON_BROWSER_RENDERER=software` requests Mesa software rendering in the helper.

## Storage upgrade and rollback

WPE's modern WebKit API migrates localStorage and IndexedDB into unified origin
storage that the legacy GTK API cannot read. Before opening an existing profile
with WPE, Zeron takes a `before-wpe-v1` snapshot of website data, cookies and
persistent cache storage while holding the profile lock. It then writes a
`wpe-storage-v1` marker. An incomplete snapshot prevents the upgrade.

Once marked, that profile cannot fall back to the GTK helper. If WPE is missing,
restore its runtime; software rendering can still use the WPE storage format.
This avoids presenting stale login state or apparently lost local data.

To return to the old GTK runtime, close all tabs and helpers using the profile,
preserve the upgraded profile by moving it aside, then copy its `before-wpe-v1`
snapshot into the original profile path. The snapshot represents data from before
the upgrade, so retain the upgraded directory for any newer website data. See
[profile locations](browser-profiles.md) before restoring. The storage regression
probe tests migration, guarded fallback, snapshot restoration and recovery from
a failed upgrade.

HTTP(S) links in chat open new Browser tabs in their conversation. If the runtime cannot start, the tab shows the browser error; use **Open in external browser** from the transcript link's context menu or the Browser toolbar's external-open button. See [transcript link interactions and fixtures](../transcript-browser-links.md).
