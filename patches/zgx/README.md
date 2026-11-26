# Zeron's local ZGX framework fork

The app's Cargo patch currently uses the local repository at
`/var/home/roan/Documents/zgx`. `zeron-fork.bundle` preserves the exact fork
commits, including their signatures, so the revisions referenced by
`apps/zgx/Cargo.toml` are recoverable from this repository.

The bundle contains 6 commits after upstream's pin
`7a8d5a4298c6c4ca7b47f922a18ddab563158c9a`. Its tip is
`0df642cd954865b21de75c3145dbb482ff85b28f`: the existing collection and
conversion-cache fixes plus the Wayland empty-frame pacing fix and its
native regression probe. The source is ZGX; its MIT license is included in this directory.

To recover the fork into a local clone of https://github.com/zeronsh/zgx:

```sh
git -C /var/home/roan/Documents/zgx fetch https://github.com/zeronsh/zgx 7a8d5a4298c6c4ca7b47f922a18ddab563158c9a
git -C /var/home/roan/Documents/zgx bundle verify /absolute/path/to/zeron/patches/zgx/zeron-fork.bundle
git -C /var/home/roan/Documents/zgx fetch /absolute/path/to/zeron/patches/zgx/zeron-fork.bundle refs/heads/fix/zeron-wayland-empty-frames
```

Fetching imports the pinned commit without changing the local checkout or
its branches. For another filesystem location, change the five Cargo patch
URLs and regenerate `apps/zgx/Cargo.lock` to match. The fork updater remains
`scripts/sync-zgx-fork.py`.

The pacing fix preserves a granted presentation when a frame paints nothing;
without a surface commit, Wayland provides no new frame callback. Waiting
for the existing refresh timer prevents immediate redraw requests from
spinning the UI thread. The native regression probe and its recorded
validation are in commit `0df642cd9548`.
