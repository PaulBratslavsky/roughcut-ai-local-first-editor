# RoughCut Import Cut — DaVinci Resolve script (works in the FREE version).
#
# Imports your CURRENT RoughCut cut into Resolve: most recently edited
# project → export Resolve XML → ImportTimelineFromFile. Run it from
# Workspace ▸ Scripts ▸ Utility after editing in the RoughCut app.
#
# Why this exists: the free Resolve cannot be driven by outside apps
# (external scripting is Studio-only), but scripts launched from Resolve's
# own menu have full API access. Same hand-off, triggered from this side.
#
# The RoughCut app must be running. If it asks to approve the export, click
# Approve in the RoughCut window (external edits always ask).

import json
import os
import sys
import tempfile
import urllib.request


def data_dir():
    return os.path.expanduser("~/Library/Application Support/roughcut")


def discover_endpoint():
    path = os.path.join(data_dir(), "mcp.json")
    if not os.path.exists(path):
        raise RuntimeError(
            "RoughCut does not appear to be running (no endpoint file at %s). "
            "Launch the RoughCut app and try again." % path
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
    with urllib.request.urlopen(req, timeout=600) as resp:
        out = json.load(resp)
    payload = json.loads(out["result"]["content"][0]["text"])
    if isinstance(payload, dict) and payload.get("error"):
        raise RuntimeError(payload["error"].get("message", "tool error"))
    return payload


def get_resolve():
    r = globals().get("resolve")
    if r:
        return r
    try:
        import DaVinciResolveScript as dvr  # type: ignore

        return dvr.scriptapp("Resolve")
    except Exception:
        return None


def main():
    url, token = discover_endpoint()
    listed = call_tool(url, token, "list_projects", {})
    projects = listed.get("projects", [])
    if not projects:
        print("RoughCut has no projects — edit something there first.")
        return
    project = projects[0]  # most recently edited
    print("Importing the current cut of %r…" % project["name"])
    print("(if RoughCut asks to approve the export, click Approve there)")

    xml_path = os.path.join(tempfile.gettempdir(), "roughcut-import-%s.xml" % project["id"][:8])
    exported = call_tool(
        url,
        token,
        "export",
        {"project_id": project["id"], "target": "resolve_xml", "out_path": xml_path},
    )
    xml_path = exported.get("path", xml_path)

    r = get_resolve()
    if not r:
        print("Exported XML: %s (run this from Resolve's Scripts menu to auto-import)" % xml_path)
        return
    pm = r.GetProjectManager()
    proj = pm.GetCurrentProject() if pm else None
    if not proj:
        print("Open (or create) a Resolve project first.")
        return
    tl = proj.GetMediaPool().ImportTimelineFromFile(xml_path)
    if tl:
        print("Imported timeline %r — media linked from the original file." % tl.GetName())
    else:
        print("Resolve refused the import; the XML is at %s" % xml_path)


if __name__ == "__main__":
    main()
