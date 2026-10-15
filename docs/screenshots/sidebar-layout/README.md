# Sidebar fixture

The `sidebar-fixture` example uses synthetic projects, sessions, icons, and pull
request metadata without an engine or account connection.

```sh
cargo run -p zeron-ui --example sidebar-fixture --features project-palette-fixture
```

Set `ZERON_SIDEBAR_COMPACT=1` for compact rows,
`ZERON_SIDEBAR_HIDE_LABEL=1` to hide project/device labels, or
`ZERON_SIDEBAR_ALL_PROJECTS=1` for the All projects layout.

```sh
WAYLAND_DISPLAY= ZERON_SIDEBAR_COMPACT=1 ZERON_SIDEBAR_ALL_PROJECTS=1 \
cargo run -p zeron-ui --example sidebar-fixture --features project-palette-fixture
```
