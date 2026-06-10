# RoughCut AI Draft — DaVinci Resolve script (works in the free version).
#
# Drop this file into Resolve's scripts folder (see install.sh) and run it
# from Workspace ▸ Scripts ▸ RoughCut AI Draft with a clip selected in the
# media pool. It drives the RoughCut app's local tool API:
#
#   create_project → transcribe (on-device whisper) → generate_rough_cut
#   (silences/fillers/duplicate takes removed) → export xmeml → import the
#   timeline right back into Resolve.
#
# Everything runs on this machine; the RoughCut app must be running (the
# script launches it if installed). Progress prints to the Resolve console.
#
# Standalone test mode (no Resolve): `python3 "RoughCut AI Draft.py" <video>`
# runs the whole pipeline and prints the XML path instead of importing.

import json
import os
import platform
import subprocess
import sys
import tempfile
import time
import urllib.request

POLL_LAUNCH_SECS = 20
HTTP_TIMEOUT_SECS = 3600  # transcription of long footage is slow; be patient


# --------------------------------------------------------------- RoughCut API

def data_dir():
    home = os.path.expanduser("~")
    sysname = platform.system()
    if sysname == "Darwin":
        return os.path.join(home, "Library", "Application Support", "roughcut")
    if sysname == "Windows":
        return os.path.join(os.environ.get("APPDATA", home), "roughcut")
    return os.path.join(
        os.environ.get("XDG_DATA_HOME", os.path.join(home, ".local", "share")), "roughcut"
    )


def discover_endpoint(launch_if_needed=True):
    path = os.path.join(data_dir(), "mcp.json")
    if not os.path.isfile(path) and launch_if_needed and platform.system() == "Darwin":
        print("RoughCut app not running — trying to launch it…")
        subprocess.call(["open", "-a", "RoughCut"])
        for _ in range(POLL_LAUNCH_SECS):
            if os.path.isfile(path):
                break
            time.sleep(1)
    if not os.path.isfile(path):
        raise RuntimeError(
            "RoughCut does not appear to be running (no endpoint file at %s). "
            "Start the RoughCut app and run this script again." % path
        )
    with open(path) as f:
        info = json.load(f)
    return info["url"], info["token"]


def call_tool(url, token, name, arguments):
    body = json.dumps(
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        }
    ).encode()
    req = urllib.request.Request(
        url,
        data=body,
        headers={"content-type": "application/json", "authorization": "Bearer " + token},
    )
    with urllib.request.urlopen(req, timeout=HTTP_TIMEOUT_SECS) as resp:
        payload = json.load(resp)
    result = payload.get("result", {})
    text = result.get("content", [{}])[0].get("text", "{}")
    data = json.loads(text)
    if result.get("isError"):
        raise RuntimeError("%s failed: %s" % (name, data.get("error", data)))
    return data


def make_rough_draft(file_path):
    """Run the RoughCut pipeline; returns (xml_path, summary_dict)."""
    url, token = discover_endpoint()
    name = os.path.splitext(os.path.basename(file_path))[0] + " — AI rough draft"

    print("[1/4] Creating RoughCut project for %s" % file_path)
    project = call_tool(url, token, "create_project", {"name": name, "file_path": file_path})
    pid = project["id"]

    print("[2/4] Transcribing on-device (whisper) — this is the long step…")
    t0 = time.time()
    transcript = call_tool(url, token, "transcribe", {"project_id": pid})
    print("       %d segments in %.0fs" % (len(transcript.get("segments", [])), time.time() - t0))

    print("[3/4] AI rough cut (silences, fillers, duplicate takes)…")
    rough = call_tool(url, token, "generate_rough_cut", {"project_id": pid})
    timeline = rough.get("timeline", {})
    cut_count = rough.get("cut_count", 0)
    included = sum(
        c["source_out"] - c["source_in"] for c in timeline.get("clips", []) if c.get("included")
    )
    print(
        "       %s cuts · %.1f min kept of %.1f min"
        % (cut_count, included / 60.0, timeline.get("duration", 0) / 60.0)
    )

    print("[4/4] Exporting timeline XML…")
    xml_path = os.path.join(tempfile.gettempdir(), "roughcut-draft-%s.xml" % pid[:8])
    call_tool(
        url, token, "export", {"project_id": pid, "target": "premiere_xml", "out_path": xml_path}
    )
    return xml_path, {
        "cuts": cut_count,
        "kept_min": included / 60.0,
        "source_min": timeline.get("duration", 0) / 60.0,
        "project": name,
    }


# ----------------------------------------------------------------- Resolve UI

def get_resolve():
    r = globals().get("resolve")
    if r:
        return r
    try:
        import DaVinciResolveScript as dvr  # noqa: N813

        return dvr.scriptapp("Resolve")
    except Exception:
        return None


def selected_clip_path(media_pool):
    """The selected media-pool clip's file path, with sensible fallbacks."""
    clips = []
    get_selected = getattr(media_pool, "GetSelectedClips", None)
    if callable(get_selected):
        selected = get_selected() or {}
        clips = list(selected.values()) if isinstance(selected, dict) else list(selected)
    if not clips:
        root = media_pool.GetRootFolder()
        clips = list(root.GetClipList() or [])
    for clip in clips:
        path = clip.GetClipProperty("File Path")
        if path:
            return path
    return None


def run_in_resolve(resolve):
    pm = resolve.GetProjectManager()
    project = pm.GetCurrentProject()
    if not project:
        print("Open (or create) a Resolve project first.")
        return
    media_pool = project.GetMediaPool()
    file_path = selected_clip_path(media_pool)
    if not file_path:
        print("Select a clip in the media pool first (or add one).")
        return

    xml_path, summary = make_rough_draft(file_path)

    print("Importing timeline into Resolve…")
    timeline = media_pool.ImportTimelineFromFile(xml_path)
    if timeline:
        print(
            "Done: “%s” — %s cuts, %.1f min kept of %.1f min. "
            "Every cut is an editable clip; fine-tune away."
            % (summary["project"], summary["cuts"], summary["kept_min"], summary["source_min"])
        )
    else:
        print(
            "RoughCut finished but Resolve declined the import. The XML is at %s — "
            "try File ▸ Import ▸ Timeline on it and report what Resolve says." % xml_path
        )


def main():
    resolve = get_resolve()
    if resolve:
        run_in_resolve(resolve)
        return
    # Standalone mode: verify the whole RoughCut side without Resolve.
    if len(sys.argv) < 2:
        print("Not inside Resolve. Standalone test: python3 '%s' <video file>" % sys.argv[0])
        sys.exit(2)
    xml_path, summary = make_rough_draft(os.path.abspath(sys.argv[1]))
    print("OK (standalone): %s cuts, %.1f→%.1f min, XML at %s"
          % (summary["cuts"], summary["source_min"], summary["kept_min"], xml_path))


main()
