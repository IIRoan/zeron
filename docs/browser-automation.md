# Browser automation

The Linux desktop exposes its existing browser tabs to agents through `zeron mcp`.
Open a browser tab in a chat, then enable its **bot** toolbar button. This grants
that chat's agent control of that tab, including its logged-in website session.
Turn the button off to stop accepting new automation commands. Consent is kept
only for the lifetime of the tab. Other chats cannot list or control it.
Popups from an enabled tab inherit its consent and open in the background in
the same chat, so an agent can follow a link without moving your focus.

Ask the agent to inspect the page, fill a form, click through a flow, or check an
assertion. It can use:

| Tool | Purpose |
| --- | --- |
| `browser_list_tabs` | Discover this chat's tabs and whether control is enabled. |
| `browser_new_tab` | Open a background tab, inheriting consent from an enabled source tab. |
| `browser_navigate`, `browser_back`, `browser_forward`, `browser_reload` | Navigate without changing the user's focus. |
| `browser_snapshot` | Read visible DOM roles, names, field values and element references. |
| `browser_click` | Click with native pointer input after checking visibility and obstruction. |
| `browser_fill`, `browser_type`, `browser_keypress`, `browser_select` | Edit fields, insert text, press keys, and select options. |
| `browser_scroll` | Scroll by CSS pixels. |
| `browser_wait` | Wait for text after navigation or an interaction. |
| `browser_evaluate` | Run a synchronous JavaScript assertion and return JSON. |
| `browser_screenshot` | Return the current viewport as an MCP PNG image. |
| `browser_close_tab` | Close a tab. |

After navigating, wait for the expected page text before interacting. References
come from the latest snapshot of that document and expire on the next snapshot,
navigation, element replacement, or a changed target. A stale reference returns
an error, so the agent must inspect again. Password field values are excluded
from snapshots; agent control still grants access to the page and its session.

Snapshots describe the DOM rather than the operating system's accessibility
tree. Open shadow roots are included; closed shadow roots and frame contents
are not. To inspect a frame, open its URL in a separate tab. JavaScript results
are limited to 1 MB, screenshots to 8 MB of PNG data, and page operations have a
deadline. Timeouts do not prove a click or navigation had no effect: inspect the
page before retrying. Mutating browser requests are never automatically replayed
after a connection failure.

## Implementation

The engine brokers requests to a connected Linux desktop. It stores only tab
inventories and pending request IDs. It does not start WebKit in the daemon.
Each desktop publishes an ordered inventory; older updates cannot restore
revoked consent. The desktop checks chat ownership and consent again before
executing. Disconnecting removes its tabs and fails outstanding requests.
The feature requires a daemon advertising `browser-control-v1`. Other desktop
platforms continue to use their current browser and do not register automation
hosts yet. Remote-device routing is not added by this feature.

On Linux, frame output runs on a helper writer thread, with at most one queued
or in-flight frame per page. Damage stays pending while that frame is sent.
Input uses a bounded writer mailbox rather than blocking the GPUI thread on a
pipe. Frame notifications are coalesced. Builds with WPE WebKit 2.54 or newer
prefer its GPU compositor and map its DMA-BUF frames into the GPUI image bridge.
Only the newest waiting frame is retained, hidden tabs stop copying, and idle
pages stop polling. GPUI still receives copied pixels: this is GPU page rendering,
with a CPU readback and texture upload, rather than zero-copy texture sharing.
The WebKitGTK helper remains available for profiles that have not been migrated.
See [Linux runtime, builds and storage compatibility](reference/linux-browser.md).

## Validation

Run focused Rust checks for `zeron-proto`, `zeron-engine`, `zeron-mcp`, and
`zeron-ui`. The following probes use synthetic pages and temporary browser
profiles. The GTK helper requires a working display; WPE runs headless with access
to a DRM render node for hardware rendering:

```sh
env -u LD_LIBRARY_PATH python3 scripts/test-browser-automation.py --helper /path/to/zeron-webkit
env -u LD_LIBRARY_PATH python3 scripts/test-browser-backpressure.py --helper /path/to/zeron-webkit
env -u LD_LIBRARY_PATH python3 scripts/test-browser-storage.py --helper /path/to/zeron-webkit
env -u LD_LIBRARY_PATH python3 scripts/test-browser-rendering.py --helper /path/to/zeron-webkit-wpe --require-gpu
```

For storage migration and rollback coverage, pass the WPE helper as `--helper`
and the GTK helper as `--alternate-helper` to the storage probe. Run a bundled
helper through its private library namespace, as the desktop does; setting a
global library path for the main application mixes system ABIs.

The backpressure probe reports frame rate and input latency while its frame
reader is paused. Compare before and after on the same display and workload;
these measurements are not a general browser benchmark.
