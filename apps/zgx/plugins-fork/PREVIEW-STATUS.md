# zgx fork preview status

This branch preserves the separate Linux beta and the three fork plugins.
It is unfinished and is not the regular GPUI Zeron release.

Included work:

- Linux Workbench layout, file navigation, workspace search and release notes.
- The fork's Git selection, staging, menus, history and syntax highlighting.
- Project Services and worktree settings as an independent plugin.
- Separate Linux beta identity, launcher, installer and app profile.
- Explicit saved-account and preference transfer, including private saved
  environment snapshots needed by followed worktree environment files.

Known gaps and validation limits:

- The Linux beta web view is reported broken; this branch does not claim a
  working bundled web runtime.
- Project setup can reject a send before the agent starts when configured
  environment/dependency sources are missing. The preview's delivery UI
  does not adequately explain that failure. Copying saved environment
  snapshots fixes the transfer omission, but stale dependency paths still
  require project configuration repair.
- GPUI chat drag-out and the old terminal clipboard guard are not ported.
- Prior native/UI checks cover selected workflows, not complete parity or
  production readiness. Bounded real Codex and Claude message exchanges
  succeeded independently of the failing project worktree setup.

Do not copy this interface over the normal Zeron installation. The normal
app's native plugin work continues separately on the regular branch.
