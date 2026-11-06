# ADR-0009: Link import — a registry tool that downloads, turned on by the user

**Status:** accepted (2026-11-05)

## Context

The owner wants RoughCut driven by programs — Claude Code over MCP, shell
scripts through a `roughcut` CLI — including starting from a link: "import
this YouTube video, cut the silences, export". RoughCut had no way to fetch a
video; the user downloaded it with another tool and imported the file.

Two earlier decisions stand in the way:

*   [ADR-0004](0004-provisioning-outside-the-tool-registry.md) keeps
    downloads out of the tool registry: a download tool "would let a confused
    agent loop — or any MCP client — pull gigabytes from an arbitrary URL".
*   [ADR-0007](0007-hosted-model-mode.md) and
    [`docs/07-architecture.md`](../07-architecture.md) principle 1: the app
    makes no network traffic unless the user starts it, and only a human
    action in the app can *enable* egress.

The owner decided (`docs/designs/url-import-and-cli.md`) that the app and MCP
clients may import links **without a per-download approval**, because
unattended scripts are the point. The EGM review (2026-11-05) required that
this be reconciled with both rules rather than slipped past them.

## Decision

1.  **One registry tool, `import_url`, for the UI and MCP clients only**
    (`agent: false`: the in-app agent never creates projects). It downloads
    one video with yt-dlp into the download folder and creates a project.
    This amends ADR-0004's "no download tool in the registry" for this one
    tool; installers stay outside the registry.
2.  **The user turns it on, in the app.** Link import works only when the
    app's own yt-dlp is in `<data dir>/bin/`. Installing it (with Deno, the
    JavaScript runtime YouTube needs) is a Tauri shell command in Setup that
    no client can call; Setup says that connected clients can then import
    links. Remove turns it off. A yt-dlp on PATH or from Homebrew is never
    used, so nothing else can turn it on. This keeps ADR-0007's rule: a human
    action in the app enables the egress.
3.  **Bounds on every call**, enforced in the core
    (`core/src/engine/media.rs`, `core/src/adapters/fetch.rs`):
    *   one video: a metadata pass refuses playlists and channels before
        anything is downloaded;
    *   no live or upcoming streams;
    *   only `http` / `https` links, and none naming `localhost`, loopback,
        private-network or link-local hosts (read as written: a DNS name or
        redirect to such an address isn't caught);
    *   files only inside the download folder, which only the user changes
        (`set_preferences` refuses `download_dir` from non-UI callers);
        `out_dir` must resolve inside it;
    *   never overwriting a file: yt-dlp names the file
        `<title> [<video id>].<ext>`, and an existing one is reused;
    *   the user's own yt-dlp config is ignored, no cookies are passed, and
        the link is always the last argument, after `--`.
4.  **Principle 1 gains one exception**: after the user installs yt-dlp,
    connected clients may start link imports. Nothing new runs at startup:
    Setup's status reads recorded versions and never runs the binaries.

## Consequences

*   A client misled by text it read can download any public video into the
    download folder once link import is on. Accepted by the owner; bounded as
    above; logged (`url_import_*` events record the host and yt-dlp version,
    never the full link or title).
*   yt-dlp breaks when sites change. Setup shows yt-dlp's version date and
    marks it out of date after 60 days; Update reinstalls the latest release.
*   RoughCut doesn't bundle yt-dlp (youtube-dl's repository was taken down by
    a DMCA notice in 2020): the user downloads it at runtime from yt-dlp's
    own releases, checked against the published SHA-256.
*   `docs/zero-egress-verification.md` must look at yt-dlp child processes
    as well as the app, since `lsof -p <app pid>` can't see their sockets.
