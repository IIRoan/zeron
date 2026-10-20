# Direct iPhone control

The iOS app can pair with one running Zeron instance without WorkOS or the
Zeron edge service. Agents run on the computer; the phone renders live chat
updates and sends durable commands. The instance must remain awake and running.

This first version supports existing chats, new chats, text replies, steering,
queued messages, stopping a run and answering agent questions. Image sending,
remote voice and push notifications are not supported in direct mode yet.

## Computer

Build the updated engine and the separate gateway from the same revision:

```sh
cargo build --locked --release -p zeron -p zeron-rpc --bin zeron --bin zeron-remote
```

Restart the desktop app with the new engine when current work permits. The gateway
also works with `zeron headless`. It attaches to the running engine's local IPC;
never start a second engine against the same data directory.

```sh
zeron-remote serve
```

The default gateway address is `127.0.0.1:27812`; the engine IPC port is `27811`.
Use `serve --ipc-port PORT` if your desktop uses a different IPC port.
Put the gateway behind a trusted HTTPS proxy. A private-network example is:

```sh
tailscale serve 27812
zeron-remote pair
```

On the iPhone choose **Connect to My Instance**, enter the HTTPS address from
the proxy and paste the pairing key. Keys expire after ten minutes and can be
redeemed only once. The app stores its resulting credential in Keychain.

`zeron-remote devices` lists paired phones. Revoke one with
`zeron-remote revoke DEVICE_ID`; existing connections close within five seconds
and new requests are rejected immediately. Protect the gateway data directory
(default `~/.local/share/zeron-remote`), which stores the instance identity and
hashed device credentials. `--data-dir` must be the same for all gateway commands.
Changing engine identity or profile requires a separate gateway directory and
new pairing. Pair only devices you trust with this workspace.

The listener refuses non-loopback bind addresses and browser Origin headers.
Credentials travel in WebSocket authorization headers, never URL queries. Remote
addresses require TLS; plain HTTP is accepted only for localhost test fixtures.
The gateway exposes an explicit subset of RPC methods and cannot stop the engine,
sign it out, change agent accounts or route requests to another device.

## GitHub macOS build

The **Direct instance iOS** workflow builds on `macos-26`. Branch pushes run Rust
transport/integration tests, simulator unit tests and an unsigned iPhone archive.
The uploaded `.xcarchive.zip` is build evidence, not an installable IPA.

For internal TestFlight, configure repository secrets:

- `AC_API_KEY_P8`: App Store Connect team API private key contents.
- `AC_API_KEY_ID`: key ID.
- `AC_API_ISSUER_ID`: issuer ID.

Configure repository variables `IOS_TEAM_ID` and `IOS_BUNDLE_ID` for your own
Apple team/app. Create the App Store Connect app and matching explicit App ID;
the Live Activity extension uses `<IOS_BUNDLE_ID>.LiveActivity`. The API key
needs permission to manage signing and upload builds. Then manually run the
workflow with `testflight=true`. It archives unsigned, exports with automatic
Apple distribution signing, uploads internally and waits for processing.
Add the resulting build to your internal testing group in App Store Connect.

## Implementation and validation

`zeron-remote` authenticates and forwards to local IPC. The engine's direct APIs
expose the registry and stream incremental Loro chat updates. The client retains
its existing renderer and persistent SQLite outbox. Registry writes reach the
host before commands for new chats. Updates are acknowledged only after the host
persists them; replay uses the existing CRDT and processed-command ledger.

The regression suite uses a real engine, WebSocket gateway, mobile Rust client
and mock harness. Run:

```sh
cargo test --locked -p zeron-rpc remote::tests
cargo test --locked -p zeron-engine --test direct_mobile
cargo test --locked -p zeron-client
```

Real iPhone installation, network switching, background suspension and wake
behavior require testing on a signed device build.
