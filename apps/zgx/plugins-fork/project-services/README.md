# Project services

This independent plugin uses the Linux fork's existing project service manager.
It appears in Settings -> Customize -> Plugins, on the Services page, in
the session panel, and the left activity menu in Linux Workbench. Services
uses a filterable sidebar with output beside it in wide panels and below
it in narrow panels. Arrow keys, Home and End select services. An empty
filter explains when nothing matches. The standalone project page also
works before opening a chat and with Linux Workbench disabled.

Start, stop, and restart affect the selected service. Right-click a service
for actions on that service. Service actions also contains bulk controls,
shared databases, and project settings. Edit services opens the standard
settings sheet. Output attaches to the existing engine
PTY, so changing tabs does not create another service process.

App services follow the current checkout. Shared databases run from the
project's main folder and retain their readiness checks. Worktree settings
edit project defaults from the main folder and overrides from linked
worktrees. Configuration drafts survive polling and ask before discarding.

The plugin requires sessions/files access and the `sh` command permission.
Its SDK bridge resolves device, project, and checkout from host records,
allows only named project operations, and never exposes an IPC token.
