# Direct Zeron ZGX on Windows

The portable Windows build opens Zeron's ZGX workspace over an authenticated
LAN connection. The paired Linux engine owns the workspace, runs agents and
terminals, and serves project files. The Windows build keeps its own app
preferences and saves its pairing with Windows DPAPI.

The viewer uses the packaged ZGX interface. Host plugins require a
viewer-compatible plugin transport.

## Pairing

Extract the portable archive and open `Zeron.exe` from the extracted folder.
Run `zeron-remote --desktop pair` on the Linux host and enter the key in
the Windows app. The installed `zeron-zgx-pair` shortcut runs this command.
The key expires after ten minutes. Later starts load the saved credential.
Windows associates the credential with the configured address and CA.
Run `Zeron.exe --pair` to pair again. A credential saved by the first
portable build needs a fresh pairing with this version.

On the host, `zeron-remote --desktop devices` lists connections paired to the
desktop gateway. Revoke one with `zeron-remote --desktop revoke DEVICE_ID`.
Desktop keys and device credentials live in a separate store from iPhone
pairings. Desktop pairing authorizes control of the host workspace, including
its terminals. Pair only a Windows account you trust with your Linux shell.

## Building

Start the host engine in local-only mode. Run the loopback gateway with
`zeron-remote --desktop serve --listen 127.0.0.1:27815 --ipc-port 27654`
and put an HTTPS proxy such as `scripts/zeron-lan-proxy.py` in front of it.
Use a server certificate valid for the host's LAN address and restrict the
firewall rule to the local subnet and HTTPS port.

Set `ZERON_ZGX_DIRECT_INSTANCE` during the Windows build to a JSON object
with `address` (the LAN IP and port) and `ca` (an array of bytes for the
public CA certificate in DER format). Keep the CA and server private keys
on the Linux host.

From `apps/zgx` on a Windows build machine with the MSVC toolchain, run
`cargo build --locked --release --features beta`. Package the resulting
`zeron-zgx.exe` as `Zeron.exe` with the ZGX `ui` and `plugins-builtin`
trees in `beta/<version>/` beside the executable, as in the Windows
packaging script. Include the DirectML runtime from the build.
