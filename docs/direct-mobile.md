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

The **Direct instance iOS** workflow builds on `macos-26-intel`, using Intel
simulator binaries and ARM64 iPhone binaries. Branch pushes run Rust
transport/integration tests, simulator unit tests and an unsigned iPhone archive.
The uploaded `.xcarchive.zip` is build evidence, not an installable IPA.

For a preview that installs directly on registered Apple devices, configure
repository secrets:

- `IOS_DISTRIBUTION_P12`: base64-encoded Apple distribution certificate/P12.
- `IOS_DISTRIBUTION_PASSWORD`: its password.
- `IOS_APP_PROFILE`: base64-encoded ad hoc profile for the app bundle ID.
- `IOS_ACTIVITY_PROFILE`: base64-encoded ad hoc profile for `<IOS_BUNDLE_ID>.LiveActivity`.
- `EXPO_TOKEN`: Expo access token for uploading the built IPA and sharing its installation page.

Configure repository variables `IOS_TEAM_ID` and `IOS_BUNDLE_ID` for your own
Apple team/app. Both profiles must include the same registered devices and the
imported certificate. Existing Rocal profiles cannot sign Zeron because Apple
ties each profile to its app ID. One authenticated Apple Developer session is
needed to create the Zeron profiles; subsequent builds use the saved credentials.
No TestFlight app or App Store Connect API key is required.

With `eas` and `gh` already signed in locally, provision the two Zeron app IDs
and profiles using the existing EAS certificate:

```sh
node scripts/ios/provision-preview.cjs          # check only
node scripts/ios/provision-preview.cjs --apply  # Apple login, then save GitHub secrets
```

The helper includes the team's enabled iPhones and iPads, reuses matching
profiles when available and does not revoke existing credentials. Apple
passwords and verification codes stay in the local EAS login flow.

Manually run **Direct instance iOS** with `distribution=preview`. GitHub's macOS
runner builds and signs the native app, verifies the exported IPA and uploads
it to Expo. Open the installation link in the workflow summary on your registered
iPhone/iPad (iOS/iPadOS 26 or newer). The `scripts/ios/expo` metadata only links
the native build to Expo's distribution project; it does not convert the app to
React Native or use Expo's build servers.
The Expo project requires account access to download internal builds.

The signing keychain and installed profiles are removed after the job.
Device identifiers are masked in build logs. Regenerate both profiles when
adding a device or renewing the certificate. Choose `distribution=unsigned`
for build validation without signing credentials.

## Implementation and validation

`zeron-remote` authenticates and forwards to local IPC. The engine's direct APIs
expose the registry and stream incremental Loro chat updates. The client retains
its existing renderer and persistent SQLite outbox. Registry writes reach the
host before commands for new chats. Updates are acknowledged only after the host
persists them; replay uses the existing CRDT and processed-command ledger.

The regression suite uses a real engine, WebSocket gateway, mobile Rust client
and mock harness. Run:

```sh
cargo test --locked -p zeron-rpc
cargo test --locked -p zeron-engine --test direct_mobile
cargo test --locked -p zeron-client
cargo test --locked -p zeron-doc direct_tests
```

Real iPhone installation, network switching, background suspension and wake
behavior require testing on a signed device build.
