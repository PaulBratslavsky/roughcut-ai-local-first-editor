# mcp

## Overview

`roughcut_core::mcp` (declared `pub mod mcp;` in `core/src/lib.rs`) is where the
editor talks to models outside the app. It has two halves:

- **Server** (`server.rs`): exposes the tool registry (`core/src/tools.rs`) to
  external MCP clients as JSON-RPC at `POST http://127.0.0.1:<port>/mcp`,
  guarded by a per-install bearer token. Claude Desktop only launches stdio
  subprocesses, so it reaches this endpoint through the separate `mcp-shim`
  crate (`mcp-shim/src/main.rs`). Unless the shim is given the endpoint by
  arguments or environment variables, it finds the running app by reading the
  `mcp.json` file that `start` writes (see Invariants). The app also passes the
  endpoint to its in-app terminal, where Claude Code can connect to it over
  HTTP directly.
- **Client** (`client.rs`): the opt-in outbound path. It stores the user's API
  key for a frontier provider and runs the same agent loop
  (`crate::agent::run_instruction_with`) against that provider through an
  OpenAI-compatible client.

The JSON-RPC layer is hand-written on top of axum. `rmcp` is not a dependency.
The `mod.rs` doc says the build spec names `rmcp`, and that the module keeps
rmcp's shape so a transport swap stays inside this directory. `mod.rs` holds
only that doc comment and the two `pub mod` declarations. It has no re-exports.

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
(`list_projects`, `read_transcript`, `outline_transcript`, `find_segments`,
`cut_range`, `apply_edits`, `review_flow`). Those tools are defined in
`core/src/tools.rs`, not here. Update the string if they are renamed.

### `client.rs`

| Item | Signature | Used by |
| --- | --- | --- |
| `connect_external` | `pub fn connect_external(editor: &Editor, provider: &str, api_key: &str) -> Result<ExternalConnection>` | `h_connect_external` (tool `connect_external`) in `core/src/tools.rs` |
| `escalate_to_frontier` | `pub async fn escalate_to_frontier(editor: &Editor, project_id: Uuid, instruction: &str, connection_id: &str) -> Result<InstructionOutcome>` | `h_escalate_to_frontier` (tool `escalate_to_frontier`) in `core/src/tools.rs` |

The private items are `provider_endpoint`, `store_secret`, and `load_secret`.

This directory has no subdirectories.

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
  so `apply_instruction`, `connect_external`, and `escalate_to_frontier` are not
  listed. A `tools/call` for one of them returns `isError: true` with
  `not available to MCP clients`. `Caller::Mcp` also turns on strict argument
  validation and credits edits to `ActionSource::McpClient`. Keep passing
  `Caller::Mcp`. `Caller::Ui` can use every tool, including the ones that send
  data to a frontier provider.
- **External writes need the user's approval.** The `export`, `export_clip`,
  and `delete_project` handlers in `core/src/tools.rs` check for
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
  back to `mcp_port`. `EndpointInfo.url` is `http://127.0.0.1:<port>/mcp`. The
  header comment in `mcp-shim/src/main.rs` still says the app binds a new
  random port on each launch. That comment is out of date.
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

### Client

- **Opt-in only.** The module doc says these functions run only after an
  explicit user action. Both of their tools are registered with `meta: true`,
  so only `Caller::Ui` can call them. `Caller::Mcp` and `Caller::Agent` cannot.
- **One secret per provider string.** `connect_external` always creates a new
  `ExternalConnection` with id `Uuid::new_v4()`, `enabled: true`, and
  `api_key_ref = "roughcut-<provider>"`. Two connections with the same
  provider string share one stored secret, and the newer key replaces the older
  one.
- **The provider is not validated on connect.** `connect_external` accepts any
  provider string and stores the secret. An unknown provider is rejected only
  when `escalate_to_frontier` runs.
- **`escalate_to_frontier` errors.** The function checks these conditions in
  this order:

  | Condition | Error |
  | --- | --- |
  | `connection_id` is not a UUID | `CoreError::InvalidArg("connection_id is not a uuid")` |
  | No kv entry for the connection | `CoreError::NotFound("external connection (run connect_external first)")` |
  | The kv entry does not parse as an `ExternalConnection` | `CoreError::InvalidArg(<serde_json error>)` |
  | The connection has `enabled == false` | `CoreError::InvalidArg("connection is disabled")` |
  | On macOS, `security` cannot be spawned | `CoreError::Io` |
  | No secret in the keychain or the secrets file | `CoreError::NotFound("credential <key_ref>")` |
  | The provider string is unknown (checked after the secret loads) | `CoreError::InvalidArg("unknown provider '<provider>' — pass a full OpenAI-compatible base URL instead")` |

  Store errors and errors from the agent loop are returned unchanged.
- **Same agent loop, same undo path.** `escalate_to_frontier` calls
  `agent::run_instruction_with` with an empty history (`&[]`) and
  `ActionSource::McpClient`. Its edits therefore go through the normal undoable
  path and are marked as externally driven.
- **Provider mapping.** `provider_endpoint` maps the provider string to a base
  URL and default model:

  | Provider string | Base URL | Default model |
  | --- | --- | --- |
  | starts with `http://` or `https://` | the string itself | `"default"` |
  | `claude` or `anthropic` | `https://api.anthropic.com/v1` | `claude-sonnet-4-6` |
  | `openai` | `https://api.openai.com/v1` | `gpt-4o` |
  | anything else | none; returns `CoreError::InvalidArg` | |

  The env var `FRONTIER_MODEL`, when set, replaces the default model.
- **Trimmed secrets.** `load_secret` trims surrounding whitespace from the
  secret, whether it comes from the keychain or from the file.

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
  has, credited to `ActionSource::McpClient`. For `export`, `export_clip`, and
  `delete_project`, that includes a confirmation request to the UI (see
  Invariants).

### Client

- **`store_secret`.** On macOS it spawns `security add-generic-password -U -a
  roughcut -s <key_ref> -w <secret>`, which puts the secret in the process
  arguments. On other platforms, or when `security` exits non-zero, it writes
  the secret in plaintext to `<data dir>/secrets/<key_ref>`. On unix it then
  sets mode `0o600`. The mode is set after the write, so a newly created file
  briefly has the default, umask-based permissions. If `security` cannot be
  spawned at all, the error is returned and no file is written.
  - **Known gap:** for a URL provider, `key_ref` contains `/`. The file
    fallback creates only `secrets/`, so the write to the nested path fails.
    The macOS keychain path does not have this problem.
- **`load_secret`.** On macOS it spawns
  `security find-generic-password -a roughcut -s <key_ref> -w` and reads the
  secret from stdout. When that command exits non-zero, or on other platforms,
  it reads `<data dir>/secrets/<key_ref>`. If `security` cannot be spawned, it
  returns `CoreError::Io` and does not read the file.
- **`connect_external`.** It writes the secret as described above, then stores
  the connection as JSON under kv `external_connection:<uuid>`.
- **`escalate_to_frontier`.** It reads the kv entry, the secret, and the env
  var `FRONTIER_MODEL`. It then sends outbound HTTP requests carrying the API
  key to the provider through `crate::adapters::OpenAiCompatClient`. The agent
  loop can also edit the project.

## Verification

- Run (from the workspace root): `cargo test -p roughcut-core`
- To test only the MCP server:
  `cargo test -p roughcut-core --test e2e mcp_server_drives_an_edit_over_http`.
  This test checks the 401 response without a token, then `initialize`,
  `tools/list` (with the meta tools hidden), `tools/call`, and the refusal of
  meta tools over the wire.
- No test covers `client.rs`. This directory has no `#[cfg(test)]` modules.
- The e2e test starts the server with `start_with(.., None)`, so it never
  writes `<data dir>/mcp.json` and cannot point `mcp-shim` at a test server.
