# Model discovery reliability

Initial validation on Linux, 2026-09-17, after merging origin/main at `15f6ec31`.

The Cursor fallback/cache policy described here is superseded by the
[2026-09-18 stability follow-up](cursor-stability/README.md), which includes
rate-limit handling, credential-scoped last-good catalogs, session recovery,
and substantially expanded live and fault-injection evidence.

## Claude automatic discovery follow-up (2026-11-10)

Claude now discovers models through the CLI's initialize response, scoped to
the executable and credential context. New concrete IDs need no curated catalog
entry: their reported names, descriptions, effort levels, fast mode and thinking
support populate the picker. A reported `[1m]` variant adds a context selector
when its base is also listed. No context-window capability is guessed from a
model's name. Complete effort reports and explicit capability flags override
curated fallbacks; omitted fields in older CLIs keep those fallbacks.

Opening the model picker forces discovery through both the model cache and the
shared initialize cache. Forced requests wait for the fresh response rather
than returning disk after 100 ms, while the picker keeps its previous rows
visible until the replacement lands. Provider changes refresh loaded model
slots too. Non-forced reads can still return disk promptly and finish discovery
in the background. Transient failures retain same-account last-good catalogs;
authentication failures and account changes cannot reuse another account's rows.

Discovered native `xhigh` support also controls launch and live reconfiguration
flags for future model families. The last successful capability snapshot stays
readable during refresh, without sharing it across credential or binary contexts.
Discovery does not start a model turn or install/update a provider CLI. The CLI
must itself expose a newly released model for automatic discovery to find it.

## Cursor failure and repair

The production shim called `process.exit()` immediately after writing its
catalog. Node does not guarantee that asynchronous pipe writes have drained
when exiting. The live failure returned exit status 0 and exactly 65,536 bytes
of truncated JSON. The Rust harness consequently returned only Auto and
Composer 2.5.

Every shim exit now waits for the stdout write callback, including model,
login, fatal-error, and session-close paths. SDK background handles can still
be terminated promptly after the output drains. The Rust probe also rejects
unsuccessful subprocess exits instead of accepting an accompanying catalog.

The pinned `@cursor/sdk@1.0.28` already resolves credentials in this order:
explicit key, `CURSOR_API_KEY`, saved SDK login (with expiry/backend checks).
The earlier "public, no auth" code comment was incorrect. No separate auth
file parser or credential injection is needed. Three live probes with
`CURSOR_API_KEY` removed returned complete 73,027-byte catalogs using the
saved SDK login: 40 entries, mapped to 39 picker rows after removing the
`default` alias. Parameters and default variants remain intact.

## Other harnesses

| Harness | Discovery and coverage |
| --- | --- |
| Cursor | SDK JSONL; drain before exit; refresh on subsequent calls; overlapping successful requests share a result. |
| ACP (Grok, Hermes, Pi, Antigravity) | Read complete JSON-RPC responses before shutdown; replace permanent model caching with refresh and overlapping-request sharing. |
| Devin | Already refreshes its native JSON catalog command and shares overlapping requests. Tests cover refresh, errors, and timeouts. |
| OpenCode | Read the complete HTTP provider catalog; replace permanent model caching with refresh and overlapping-request sharing. Commands can still share the discovery server. |
| Codex | Already reads complete JSON-RPC responses and all model-list pages on each discovery. Pagination tests pass. |
| Claude | Originally static; superseded by the automatic discovery follow-up above. |

Failed/empty discovery never permanently caches a fallback. Cursor and ACP
keep their existing per-request fallback behavior; OpenCode and Devin surface
errors so callers can retry. Refreshing may cost another subprocess startup
on a later picker open; overlapping successful probes remain coalesced.

## Validation

- `cursor-agent update`: upgraded from `2026.08.11-e8db854` to
  `2026.09.15-d2fe57e`; `cursor-agent --list-models` succeeds.
- `cargo run -p zeron-harness --example cursor_models_probe`: 39 picker models
  through the real managed SDK/shim path, previously only 2 fallback rows.
- `cargo run -p zeron-harness --example opencode_models_probe`: 8 live models.
- Local Codex app-server `model/list`: 6 models; automated pagination coverage
  also passes.
- `cargo test -p zeron-harness`: 282 passed, 0 failed, 10 opt-in tests ignored.
- `git diff --check`: clean.

New tests execute the production JavaScript shim with a synthetic SDK and
require Node. They exercise a catalog over 4 MiB, a 1 MiB fatal frame, and
large login output. Subprocess/HTTP fixtures additionally cover large model
responses, account changes, concurrent probes, malformed output, unsuccessful
exit status, provider disconnects, and recovery. Live checks were not performed
for every authenticated ACP provider; those paths have fixture coverage.
