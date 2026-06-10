//! End-to-end exercise of the POC loop through the TOOL REGISTRY — the same
//! surface the UI, the local agent, and MCP clients use. Runs on the fixture
//! adapters, fully offline.

use roughcut_core::model::ActionSource;
use roughcut_core::tools;
use roughcut_core::Editor;
use serde_json::{json, Value};

async fn call(editor: &Editor, name: &str, args: Value) -> Value {
    tools::dispatch(editor, name, &args, ActionSource::Ui)
        .await
        .unwrap_or_else(|e| panic!("tool {name} failed: {e}"))
}

#[tokio::test]
async fn poc_loop_through_the_tool_registry() {
    let editor = Editor::test_instance();

    // 1. Import a video (demo probe) + transcribe on-device, no network.
    let project = call(
        &editor,
        "create_project",
        json!({ "name": "i-quit-rough-draft", "file_path": "/demo/footage.mp4" }),
    )
    .await;
    let pid = project["id"].as_str().unwrap().to_string();
    assert_eq!(project["timeline"]["cut_count"], 0);
    let transcript = call(&editor, "transcribe", json!({ "project_id": pid })).await;
    let segments = transcript["segments"].as_array().unwrap();
    assert!(segments.len() > 10, "expected a real transcript");

    // 2. One-click rough cut: silences + fillers + takes removed, cut count shown.
    let rough = call(&editor, "generate_rough_cut", json!({ "project_id": pid })).await;
    let cut_count = rough["cut_count"].as_u64().unwrap();
    assert!(cut_count >= 5, "rough cut should make several cuts, got {cut_count}");

    // 3. Edit by transcript: delete a sentence → video cuts to match.
    let victim = segments
        .iter()
        .find(|s| s["text"].as_str().unwrap_or("").contains("hiking"))
        .expect("fixture has the weekend tangent");
    let before = call(&editor, "get_timeline", json!({ "project_id": pid })).await;
    let cut = call(
        &editor,
        "cut_by_transcript",
        json!({ "project_id": pid, "segment_ids": [victim["id"]] }),
    )
    .await;
    assert!(cut["timeline"]["cut_count"].as_u64().unwrap() > before["cut_count"].as_u64().unwrap_or(0));

    // 4. Drag a clip boundary (trim), frame-accurately.
    let timeline = call(&editor, "get_timeline", json!({ "project_id": pid })).await;
    let clip = timeline["clips"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["included"] == true && c["source_out"].as_f64().unwrap() - c["source_in"].as_f64().unwrap() > 2.0)
        .unwrap()
        .clone();
    let new_in = clip["source_in"].as_f64().unwrap() + 0.5;
    let trimmed = call(
        &editor,
        "trim_clip",
        json!({ "project_id": pid, "clip_id": clip["id"], "new_source_in": new_in, "new_source_out": clip["source_out"] }),
    )
    .await;
    let moved = trimmed["timeline"]["clips"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == clip["id"])
        .unwrap()["source_in"]
        .as_f64()
        .unwrap();
    // Snapped to a 30fps frame boundary near the requested point.
    assert!((moved - new_in).abs() < 1.0 / 30.0 + 1e-9);
    let frames = moved * 30.0;
    assert!((frames - frames.round()).abs() < 1e-6, "trim not frame-exact: {moved}");

    // 5. Global padding applied to all talking clips.
    let padded = call(
        &editor,
        "set_global_padding",
        json!({ "project_id": pid, "start_s": 0.15, "end_s": 0.15, "linked": true }),
    )
    .await;
    assert_eq!(padded["timeline"]["global_padding"]["start_s"].as_f64().unwrap(), 0.15);

    // 6. Chat instruction → offline agent finds + cuts → undoable.
    let outcome = call(
        &editor,
        "apply_instruction",
        json!({ "project_id": pid, "instruction": "cut the part where I ramble about hiking on Saturday" }),
    )
    .await;
    assert!(outcome["summary"].as_str().unwrap().len() > 5);
    let undone = call(&editor, "undo", json!({ "project_id": pid })).await;
    assert!(undone["action"].is_object() || undone["action"].is_null());

    // 7. Export NLE interchange + captions; verify shape.
    let dir = std::env::temp_dir().join("roughcut-e2e");
    for (target, ext, needle) in [
        ("premiere_xml", "xml", "<xmeml"),
        ("fcp_xml", "fcpxml", "<fcpxml"),
        ("edl", "edl", "TITLE:"),
        ("otio", "otio", "OTIO_SCHEMA"),
        ("srt", "srt", "-->"),
    ] {
        let out = dir.join(format!("export.{ext}"));
        let r = call(
            &editor,
            "export",
            json!({ "project_id": pid, "target": target, "out_path": out.to_string_lossy() }),
        )
        .await;
        let content = std::fs::read_to_string(r["path"].as_str().unwrap()).unwrap();
        assert!(content.contains(needle), "{target} export missing {needle}");
    }

    // 8. Metadata: chapters from the transcript.
    let chapters = call(&editor, "generate_chapters", json!({ "project_id": pid })).await;
    assert!(chapters["chapters"].as_array().unwrap().len() >= 2);

    // 9. Projects persist and reload.
    call(&editor, "save_project", json!({ "project_id": pid })).await;
    let listed = call(&editor, "list_projects", json!({})).await;
    assert_eq!(listed["projects"].as_array().unwrap().len(), 1);

    // 10. Delete removes the edit state (never the media file).
    let deleted = call(&editor, "delete_project", json!({ "project_id": pid })).await;
    assert_eq!(deleted["deleted"], true);
    let listed = call(&editor, "list_projects", json!({})).await;
    assert_eq!(listed["projects"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn mcp_server_drives_an_edit_over_http() {
    let editor = Editor::test_instance();
    let info = roughcut_core::mcp::server::start(editor.clone()).await.unwrap();
    let client = reqwest::Client::new();

    let rpc = |method: &str, params: Value, id: i64| {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    };

    // Unauthorized without the per-install token.
    let resp = client
        .post(&info.url)
        .json(&rpc("tools/list", json!({}), 1))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // initialize → tools/list → tools/call, like Claude Desktop via the shim.
    let resp: Value = client
        .post(&info.url)
        .bearer_auth(&info.token)
        .json(&rpc("initialize", json!({ "protocolVersion": "2024-11-05" }), 1))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resp["result"]["serverInfo"]["name"], "roughcut");

    let resp: Value = client
        .post(&info.url)
        .bearer_auth(&info.token)
        .json(&rpc("tools/list", json!({}), 2))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "cut_by_transcript"));
    assert!(tools.iter().any(|t| t["name"] == "export"));

    let resp: Value = client
        .post(&info.url)
        .bearer_auth(&info.token)
        .json(&rpc(
            "tools/call",
            json!({ "name": "create_project", "arguments": { "name": "via-mcp", "file_path": "/demo/clip.mp4" } }),
            3,
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resp["result"]["isError"], false);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let project: Value = serde_json::from_str(text).unwrap();
    let pid = project["id"].as_str().unwrap();

    let resp: Value = client
        .post(&info.url)
        .bearer_auth(&info.token)
        .json(&rpc(
            "tools/call",
            json!({ "name": "generate_rough_cut", "arguments": { "project_id": pid } }),
            4,
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resp["result"]["isError"], false);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let rough: Value = serde_json::from_str(text).unwrap();
    assert!(rough["cut_count"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn semantic_search_and_paged_reads() {
    let editor = Editor::test_instance();
    editor.set_embedder_for_tests(std::sync::Arc::new(roughcut_core::adapters::MockEmbedder));

    let project = call(
        &editor,
        "create_project",
        json!({ "name": "semantic", "file_path": "/demo/footage.mp4" }),
    )
    .await;
    let pid = project["id"].as_str().unwrap().to_string();
    call(&editor, "transcribe", json!({ "project_id": pid })).await;

    // Index, then paged lean reads.
    let pid_u = uuid::Uuid::parse_str(&pid).unwrap();
    let indexed = editor.index_transcript(pid_u).await.unwrap();
    assert!(indexed > 10, "expected speech segments indexed, got {indexed}");

    let page = call(&editor, "read_transcript", json!({ "project_id": pid, "limit": 5 })).await;
    assert_eq!(page["returned"], 5);
    assert!(page["total_segments"].as_u64().unwrap() > 20);
    assert!(page["segments"][0].get("words").is_none(), "lean page must not carry word arrays");
    let page2 = call(
        &editor,
        "read_transcript",
        json!({ "project_id": pid, "offset": 5, "limit": 5 }),
    )
    .await;
    assert_ne!(page["segments"][0]["id"], page2["segments"][0]["id"]);

    // Semantic search beats keyword overlap: the hiking tangent wins.
    let found = call(
        &editor,
        "find_segments",
        json!({ "project_id": pid, "query": "hiking trip on Saturday" }),
    )
    .await;
    assert_eq!(found["method"], "hybrid");
    let top = found["segments"][0]["text"].as_str().unwrap();
    assert!(top.contains("hiking"), "semantic top hit should be the tangent, got: {top}");
}

#[tokio::test]
async fn apply_edits_batches_ops_in_one_call() {
    let editor = Editor::test_instance();
    let project = call(
        &editor,
        "create_project",
        json!({ "name": "batch", "file_path": "/demo/footage.mp4" }),
    )
    .await;
    let pid = project["id"].as_str().unwrap().to_string();
    call(&editor, "transcribe", json!({ "project_id": pid })).await;

    let result = call(
        &editor,
        "apply_edits",
        json!({ "project_id": pid, "edits": [
            { "type": "cut_range", "start": 10.0, "end": 15.0 },
            { "type": "cut_range", "start": 40.0, "end": 44.0 },
            { "type": "set_global_padding", "start_s": 0.15, "end_s": 0.15, "linked": true }
        ]}),
    )
    .await;
    assert_eq!(result["applied"], 3);
    assert_eq!(result["cut_count"].as_u64().unwrap(), 2);
    assert!(result.get("timeline").is_none(), "batch receipt must stay lean");
    assert!(result["actions"][0].get("inverse").is_none(), "journal stays internal");

    // A rough-cut pass COMPOSES: the manual cuts above survive it.
    let rough = call(&editor, "generate_rough_cut", json!({ "project_id": pid })).await;
    let clips = rough["timeline"]["clips"].as_array().unwrap();
    // (padding breathes gap edges, so check the midpoints, not the bounds)
    let still_cut = |mid: f64, min_len: f64| {
        clips.iter().any(|c| {
            c["included"] == false
                && c["source_in"].as_f64().unwrap() <= mid
                && c["source_out"].as_f64().unwrap() >= mid
                && c["source_out"].as_f64().unwrap() - c["source_in"].as_f64().unwrap() >= min_len
        })
    };
    assert!(still_cut(12.5, 4.0), "manual cut 10-15 wiped by rough cut");
    assert!(still_cut(42.0, 3.0), "manual cut 40-44 wiped by rough cut");
    assert!(rough["cut_count"].as_u64().unwrap() > 2, "AI cuts should add to manual ones");

    // Save-as: duplicate carries the cut state, original untouched.
    let copy = call(
        &editor,
        "duplicate_project",
        json!({ "project_id": pid, "name": "batch-lean-cut" }),
    )
    .await;
    assert_ne!(copy["id"], json!(pid));
    assert_eq!(copy["timeline"]["cut_count"].as_u64().unwrap(), 2);
    // Each op is its own undo step.
    let undone = call(&editor, "undo", json!({ "project_id": pid })).await;
    assert_eq!(undone["action"]["kind"], "pad");
}

#[tokio::test]
async fn setup_status_reports_capabilities() {
    let editor = Editor::test_instance();
    let status = roughcut_core::setup::status(&editor).await;
    assert!(status.demo, "test instance forces demo");
    assert!(status.demo_reason.is_some());
    assert_eq!(status.tiers.len(), 2);
    assert_eq!(status.tiers.iter().filter(|t| t.recommended).count(), 1);
    // Unreachable endpoint in tests → chat row reports false fast.
    assert!(!status.inference_reachable);
}
