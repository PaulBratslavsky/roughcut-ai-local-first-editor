# mcp

## Overview

`roughcut_core::mcp` (declared `pub mod mcp;` in `core/src/lib.rs`) exposes the
editor to models outside the app:

- **Server** (`server.rs`): exposes the tool registry (`core/src/tools.rs`) to
  external MCP clients as JSON-RPC at `POST http://127.0.0.1:<port>/mcp`,
  guarded by a per-install bearer token. Claude Desktop only launches stdio
  subprocesses, so it reaches this endpoint through the separate `mcp-shim`
  crate (`mcp-shim/src/main.rs`). Unless the shim is given the endpoint by
  arguments or environment variables, it finds the running app by reading the
  `mcp.json` file that `start` writes (see Invariants). The app also passes the
  endpoint to its in-app terminal, where Claude Code can connect to it over
  HTTP directly.

The old outbound "frontier" client (`client.rs`: `connect_external`,
`escalate_to_frontier`) was removed with Hosted mode (ADR-0007): the user's
LOCAL / HOSTED switch (`crate::engine::route`, `crate::adapters::anthropic`)
now covers every AI step, with the key in the OS key store
(`crate::secrets`).

The JSON-RPC layer is hand-written on top of axum. `rmcp` is not a dependency.
The `mod.rs` doc says the build spec names `rmcp`, and that the module keeps
rmcp's shape so a transport swap stays inside this directory. `mod.rs` holds
only that doc comment and the `pub mod server` declaration. It has no re-exports.

In this README, `<data dir>` means `crate::store::data_dir()`. That is
`dirs::data_dir()/roughcut`, or `./roughcut` (relative to the working
directory) when the OS has no data dir.

## Interface

### `server.rs`

| Item | Signature | Used by |
| --- | --- | --- |
| `start` | `pub async fn start(editor: Editor) -> crate::error::Result<EndpointInfo>`: `start_with` writing `<data dir>/mcp.json` | `app/src-tauri/src/lib.rs` (app startup) |
| `start_with` | `pub async fn start_with(editor: Editor, discovery_file: Option<&std::path::Path>) -> crate::error::Result<EndpointInfo>`: `None` writes no discovery file | `core/tests/e2e.rs` (with `None`) |
| `EndpointInfo` | `#[derive(Debug, Clone, Serialize)] pub struct EndpointInfo { pub url: String, pub token: String }` | `app/src-tauri/src/lib.rs`: kept in `AppState.mcp`, returned by the `mcp_endpoint_info` command, sent as the `mcp-ready` event payload, and passed by `pty_spawn` to `core/src/pty.rs` |

The private items are `PROTOCOL_VERSION` (`"2024-11-05"`), `McpState { editor,
token }` (the axum router state), `authorized(&HeaderMap, &str) -> bool`, and
`handle` (the only axum handler).

Wire surface (one route, `POST /mcp`, handled by `handle`):

| JSON-RPC method | `result` |
| --- | --- |
| `initialize` | `{protocolVersion, capabilities: {tools: {}}, serverInfo: {name: "roughcut", version: CARGO_PKG_VERSION}, instructions}` |
| `ping` | `{}` |
| `tools/list` | `{tools: [{name, description, inputSchema}]}` from `tools::defs(tools::Caller::Mcp)` |
| `tools/call` | `{content: [{type: "text", text}], isError}` from `tools::dispatch(&editor, params.name, &params.arguments, tools::Caller::Mcp)` |
| any other method | JSON-RPC error `-32601`, `method not found: <method>` |

The `initialize` `instructions` string tells the client how to use the tools
(`list_projects`, `open_project`, `read_transcript`, `outline_transcript`,
`find_segments`, `cut_range`, `apply_edits`, `review_flow`). Those tools are defined in
`core/src/tools.rs`, not here. Update the string if they are renamed.

## Invariants

### Server

- **Body parsing comes before auth.** The handler takes `Json<Value>`, so
  axum's `Json` extractor runs before `handle`'s body. A request with a missing
  or non-JSON `Content-Type` gets HTTP 415, and a body that is not valid JSON
  gets HTTP 400. Both come from axum with a plain-text body (not JSON-RPC),
  and both are sent before the token is checked.
- **Auth comes first inside `handle`.** `handle` calls `authorized` before
  anything else, even for notifications. A request passes only when its
  `authorization` header is valid ASCII and equals `Bearer <token>` exactly.
  The check is a plain `==`, not a constant-time comparison. On failure the
  server returns HTTP 401 with JSON-RPC error `-32001`,
  `invalid or missing auth token`.
- **MCP clients never get meta tools.** Both `tools/list` and `tools/call` pass
  `tools::Caller::Mcp`. For that caller, `Caller::can_use` returns `!spec.meta`,
  so the meta tool `apply_instruction` is not
  listed. A `tools/call` for one of them returns `isError: true` with
  `not available to MCP clients`. `Caller::Mcp` also turns on strict argument
  validation and credits edits to `ActionSource::McpClient`. Keep passing
  `Caller::Mcp`. `Caller::Ui` can use every tool, including `apply_instruction`.
- **External writes need the user's approval.** The `export`, `export_clip`,
  `export_clips`, and `delete_project` handlers in `core/src/tools.rs` check for
  `ActionSource::McpClient` and first await `Editor::request_confirmation`
  (`core/src/engine/confirm.rs`). That returns `false` when the user denies or
  does not answer within 120 s, and the tool then fails with
  `CoreError::Unavailable` (`isError: true`). It returns `true` without asking
  when the editor's `require_confirm` flag is off. `Editor::bootstrap_with_store`
  turns the flag on unless `ROUGHCUT_NO_CONFIRM=1`. `Editor::new`, and so
  `Editor::test_instance`, leaves it off.
- **Only the UI can move inference off this machine.**
  `Editor::update_preferences` (`core/src/engine/projects.rs`), behind the
  `set_preferences` tool, rejects a change of `inference_endpoint` to a
  non-loopback URL from any source other than `ActionSource::Ui`, MCP clients
  included. The error is `CoreError::InvalidArg`.
- **Notifications.** A request with no `id` key gets HTTP 202 and `{}`, and no
  method runs. `"id": null` counts as present, so the request is handled
  normally. A body that is not a JSON object (an array, such as a JSON-RPC
  batch, or a scalar) has no `id` key either, so it also gets 202 `{}` and is
  dropped.
- **Tool errors are not JSON-RPC errors.** `tools/call` always answers HTTP 200
  with a `result`. On success, `text` is the tool's return value serialized with
  `serde_json::to_string`. On failure, `isError` is `true` and `text` is
  `CoreError::to_json()`.
- **Status codes.** `handle` returns 401 (auth failure), 202 (notification),
  or 200 (everything else, including `-32601`). The 415 and 400 responses
  above come from axum before `handle` runs.
- **Protocol version.** `initialize` echoes back the client's
  `params.protocolVersion` without checking it. It falls back to
  `PROTOCOL_VERSION` when the field is missing or is not a string.
- **Token.** The token is 40 alphanumeric characters from `rand::thread_rng`.
  It is generated once, kept in store kv `mcp_token`, and reused on every later
  `start`.
- **Port.** The listener binds `127.0.0.1` only. `start` tries the port saved
  in kv `mcp_port` first. If none is saved or that bind fails, it binds port 0
  and gets an ephemeral port. Either way, the port actually bound is written
  back to `mcp_port`. `EndpointInfo.url` is `http://127.0.0.1:<port>/mcp`.
- **`mcp.json` is a contract with `mcp-shim`.** `start` writes `EndpointInfo`
  as pretty JSON (`{url, token}`) to `<data dir>/mcp.json`. The shim reads `url`
  and `token` from `dirs::data_dir()/roughcut/mcp.json`. Renaming the fields or
  moving the file breaks the shim. The two paths match only when the OS has a
  data dir. Without one, `start` writes `./roughcut/mcp.json`, and the shim
  fails with `no data dir on this platform`.
- **Shim discovery order.** The shim takes the first of these that is
  available:
  1. `--endpoint <url>` and `--token <token>` arguments (both required).
  2. The `ROUGHCUT_MCP_ENDPOINT` and `ROUGHCUT_MCP_TOKEN` environment variables
     (both required).
  3. `mcp.json`, as above.

  An endpoint from 1 or 2 is pinned: the shim never re-discovers it and does
  not retry a failed send. With 3, the shim reads the file on the first
  message and keeps that endpoint until a request cannot be sent. If the file
  cannot be read or a request cannot be sent, it reads the file again and
  retries, up to 5 attempts per message, 700 ms apart. An HTTP error status is
  not a failed send; its body is passed back as is. When all attempts fail,
  the shim answers with JSON-RPC error `-32000`.
- **The in-app terminal gets the endpoint by different names.** `pty_spawn` in
  `app/src-tauri/src/lib.rs` passes `url` and `token` to `pty::spawn`, which
  sets `ROUGHCUT_MCP_URL` and `ROUGHCUT_MCP_TOKEN` in the shell's environment.
  A shell spawned before the server is up gets neither. The URL variable is
  not the shim's `ROUGHCUT_MCP_ENDPOINT`, so a shim started from that shell
  still falls back to `mcp.json`.

## Side Effects

### Server (`start` and `handle`)

- **Network.** `start` binds a TCP listener on `127.0.0.1`.
- **Background task.** `start` runs `axum::serve` in a detached `tokio::spawn`
  and returns no shutdown handle. If serving fails, it prints
  `mcp server stopped: <e>` to stderr.
- **Store.** `start` writes kv `mcp_token` on first run only and `mcp_port` on
  every start.
- **Disk.** `start` runs `create_dir_all` on `<data dir>`, then writes
  `<data dir>/mcp.json` with the bearer token in plaintext. It sets no file
  permissions. The path comes from `crate::store::data_dir()`, not from the
  `Editor`'s store, so any caller of `start` writes the real per-user file.
  `start_with(editor, None)` writes no file; the e2e test uses it so a test
  server never replaces the running app's endpoint.
- **Tool calls.** `tools/call` has whatever side effects the dispatched tool
  has, credited to `ActionSource::McpClient`. For `export`, `export_clip`,
  `export_clips`, and `delete_project`, that includes a confirmation request
  to the UI (see Invariants).

## Verification

- Run (from the workspace root): `cargo test -p roughcut-core`
- To test only the MCP server:
  `cargo test -p roughcut-core --test e2e mcp_server_drives_an_edit_over_http`.
  This test checks the 401 response without a token, then `initialize`,
  `tools/list` (with the meta tools hidden), `tools/call`, and the refusal of
  meta tools over the wire.
- This directory has no `#[cfg(test)]` modules.
- The e2e test starts the server with `start_with(.., None)`, so it never
  writes `<data dir>/mcp.json` and cannot point `mcp-shim` at a test server.
