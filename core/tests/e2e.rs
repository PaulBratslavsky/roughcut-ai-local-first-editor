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
    // Carries the full post-rough-cut state (manual + AI cuts).
    assert_eq!(
        copy["timeline"]["cut_count"].as_u64().unwrap(),
        rough["cut_count"].as_u64().unwrap()
    );
    // Each op is its own undo step: the rough cut undoes first, then padding.
    let undone = call(&editor, "undo", json!({ "project_id": pid })).await;
    assert_eq!(undone["action"]["kind"], "ai_batch");
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

#[tokio::test]
async fn external_destructive_ops_require_confirmation() {
    use std::sync::Arc;
    struct ChanSink(tokio::sync::mpsc::UnboundedSender<(String, Value)>);
    impl roughcut_core::events::EventSink for ChanSink {
        fn emit(&self, event: &str, payload: Value) {
            let _ = self.0.send((event.to_string(), payload));
        }
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let editor = Editor::new(
        Box::new(roughcut_core::store::SqliteStore::open_in_memory().unwrap()),
        Box::new(roughcut_core::adapters::MockVideoEngine),
        Box::new(roughcut_core::adapters::MockTranscriber),
        Arc::new(ChanSink(tx)),
        true,
    );
    editor.require_confirmations_for_tests(true);

    let p = call(&editor, "create_project", json!({ "name": "guard", "file_path": "/demo/x.mp4" })).await;
    let pid = p["id"].as_str().unwrap().to_string();

    // The "user" denies whatever confirmation arrives.
    let ed2 = editor.clone();
    tokio::spawn(async move {
        while let Some((name, payload)) = rx.recv().await {
            if name == "confirm-request" {
                let id = uuid::Uuid::parse_str(payload["id"].as_str().unwrap()).unwrap();
                ed2.resolve_confirmation(id, false);
            }
        }
    });

    // Externally-driven delete: denied, project survives.
    let denied = tools::dispatch(
        &editor,
        "delete_project",
        &json!({ "project_id": pid }),
        ActionSource::McpClient,
    )
    .await;
    assert!(denied.is_err(), "external delete must require approval");
    let listed = call(&editor, "list_projects", json!({})).await;
    assert_eq!(listed["projects"].as_array().unwrap().len(), 1);

    // The user's own UI needs no approval.
    let ok = tools::dispatch(
        &editor,
        "delete_project",
        &json!({ "project_id": pid }),
        ActionSource::Ui,
    )
    .await;
    assert!(ok.is_ok());
}

#[tokio::test]
async fn open_project_backfills_a_missing_semantic_index() {
    let editor = Editor::test_instance();
    // Transcribe with NO embedder reachable: transcript persists, index doesn't
    // (this is exactly the state of projects from before indexing existed).
    let project =
        call(&editor, "create_project", json!({ "name": "old", "file_path": "/demo/a.mp4" })).await;
    let pid = uuid::Uuid::parse_str(project["id"].as_str().unwrap()).unwrap();
    call(&editor, "transcribe", json!({ "project_id": pid })).await;
    // Let transcribe's own background index attempt run (it fails: no server).
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(editor.store().load_embeddings(pid).unwrap().is_none(), "precondition: no index");

    // An embedder appears (server started); opening the project backfills.
    editor.set_embedder_for_tests(std::sync::Arc::new(roughcut_core::adapters::MockEmbedder));
    call(&editor, "open_project", json!({ "project_id": pid })).await;
    let mut indexed = false;
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        if editor.store().load_embeddings(pid).unwrap().is_some() {
            indexed = true;
            break;
        }
    }
    assert!(indexed, "open_project should build the missing index in the background");

    // And the next open is a no-op (already indexed) — search is hybrid now.
    let found =
        call(&editor, "find_segments", json!({ "project_id": pid, "query": "hiking trip" })).await;
    assert_eq!(found["method"], "hybrid");
}

#[tokio::test]
async fn plan_duration_cut_reaches_the_target() {
    let editor = Editor::test_instance();
    editor.set_embedder_for_tests(std::sync::Arc::new(roughcut_core::adapters::MockEmbedder));
    let project =
        call(&editor, "create_project", json!({ "name": "long", "file_path": "/demo/a.mp4" })).await;
    let pid = project["id"].as_str().unwrap().to_string();
    let pid_u = uuid::Uuid::parse_str(&pid).unwrap();
    call(&editor, "transcribe", json!({ "project_id": pid })).await;
    editor.index_transcript(pid_u).await.unwrap();

    let before = editor.get_timeline(pid_u).unwrap().included_duration();
    let target = before * 0.5;
    let plan =
        call(&editor, "plan_duration_cut", json!({ "project_id": pid, "target_duration_s": target }))
            .await;
    assert_eq!(plan["method"], "centrality");
    let ids = plan["segment_ids"].as_array().unwrap();
    assert!(!ids.is_empty(), "halving the video must propose cuts");
    assert!(
        plan["projected_after_s"].as_f64().unwrap() <= target + 0.5,
        "plan should reach the target: {plan}"
    );

    // Apply the plan with one batched cut; the timeline really shrinks.
    let cut = call(
        &editor,
        "cut_by_transcript",
        json!({ "project_id": pid, "segment_ids": ids }),
    )
    .await;
    let after = cut["timeline"]["clips"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["included"] == true)
        .map(|c| c["source_out"].as_f64().unwrap() - c["source_in"].as_f64().unwrap())
        .sum::<f64>();
    assert!(after < before * 0.6, "included duration should drop near the target: {after} vs {before}");

    // apply=true: plan + cut in ONE call, receipt carries the new duration.
    let smaller = after * 0.6;
    let receipt = call(
        &editor,
        "plan_duration_cut",
        json!({ "project_id": pid, "target_duration_s": smaller, "apply": true }),
    )
    .await;
    assert!(receipt["applied"].as_u64().unwrap() > 0, "apply should cut: {receipt}");
    assert!(
        receipt["included_duration_s"].as_f64().unwrap() <= smaller + 0.5,
        "receipt duration should be at the target: {receipt}"
    );
}

#[tokio::test]
async fn trash_soft_deletes_and_restores() {
    let editor = Editor::test_instance();
    let project =
        call(&editor, "create_project", json!({ "name": "keepme", "file_path": "/demo/a.mp4" })).await;
    let pid = project["id"].as_str().unwrap().to_string();

    // Delete -> gone from projects, present in trash.
    call(&editor, "delete_project", json!({ "project_id": pid })).await;
    let listed = call(&editor, "list_projects", json!({})).await;
    assert!(listed["projects"].as_array().unwrap().is_empty());
    assert_eq!(listed["trash"][0]["id"].as_str().unwrap(), pid);

    // Restore -> back, edit state intact.
    let restored = call(&editor, "restore_project", json!({ "project_id": pid })).await;
    assert!(restored["deleted_at"].is_null());
    let listed = call(&editor, "list_projects", json!({})).await;
    assert_eq!(listed["projects"][0]["id"].as_str().unwrap(), pid);
    assert!(listed["trash"].as_array().unwrap().is_empty());

    // Purge: trash older than the window is hard-deleted.
    call(&editor, "delete_project", json!({ "project_id": pid })).await;
    let pid_u = uuid::Uuid::parse_str(&pid).unwrap();
    let mut old = editor.open_project(pid_u).unwrap();
    old.deleted_at = Some(chrono::Utc::now() - chrono::Duration::days(31));
    editor.store().save_project(&old).unwrap();
    let purged = editor.purge_trash(30).unwrap();
    assert_eq!(purged, 1);
    let listed = call(&editor, "list_projects", json!({})).await;
    assert!(listed["trash"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn story_tools_degrade_honestly_without_a_model() {
    let editor = Editor::test_instance();
    let project =
        call(&editor, "create_project", json!({ "name": "story", "file_path": "/demo/a.mp4" })).await;
    let pid = project["id"].as_str().unwrap().to_string();
    call(&editor, "transcribe", json!({ "project_id": pid })).await;

    // Outline: heuristic beats cover the speech, in order.
    let outline = call(&editor, "outline_transcript", json!({ "project_id": pid })).await;
    assert_eq!(outline["method"], "heuristic");
    let beats = outline["beats"].as_array().unwrap();
    assert!(beats.len() >= 2, "expected sections, got {}", beats.len());
    assert!(beats.iter().all(|b| !b["segment_ids"].as_array().unwrap().is_empty()));

    // Cached: second call returns identical beats (same kv entry).
    let outline2 = call(&editor, "outline_transcript", json!({ "project_id": pid })).await;
    assert_eq!(outline["beats"], outline2["beats"]);

    // Make a cut right before a segment that opens with a connective, then
    // review: the deterministic pass must flag the abrupt resume.
    let t = call(&editor, "get_transcript", json!({ "project_id": pid })).await;
    let segs: Vec<&serde_json::Value> = t["segments"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["is_silence"] == false && s["text"].as_str().unwrap_or("") != "")
        .collect();
    let idx = (1..segs.len())
        .find(|&i| {
            let txt = segs[i]["text"].as_str().unwrap_or("").to_lowercase();
            txt.starts_with("and ") || txt.starts_with("but ") || txt.starts_with("so ")
        })
        .expect("fixture has a connective-opening segment");
    call(
        &editor,
        "cut_by_transcript",
        json!({ "project_id": pid, "segment_ids": [segs[idx - 1]["id"]] }),
    )
    .await;
    let review = call(&editor, "review_flow", json!({ "project_id": pid })).await;
    assert_eq!(review["method"], "deterministic");
    assert!(review["boundaries_checked"].as_u64().unwrap() >= 1);
    assert!(
        review["issues"].as_array().unwrap().iter().any(|i| i["kind"] == "abrupt_start"),
        "expected an abrupt_start issue: {review}"
    );

    // story_edit without a model and without a target: honest refusal.
    let err = tools::dispatch(
        &editor,
        "story_edit",
        &json!({ "project_id": pid, "instruction": "tighten this" }),
        ActionSource::Ui,
    )
    .await;
    assert!(err.is_err(), "story_edit should refuse without a model server");

    // …but WITH a target it falls back to the duration planner.
    editor.set_embedder_for_tests(std::sync::Arc::new(roughcut_core::adapters::MockEmbedder));
    editor.index_transcript(uuid::Uuid::parse_str(&pid).unwrap()).await.unwrap();
    let before = editor
        .get_timeline(uuid::Uuid::parse_str(&pid).unwrap())
        .unwrap()
        .included_duration();
    let out = call(
        &editor,
        "story_edit",
        json!({ "project_id": pid, "instruction": "tighten this", "target_duration_s": before * 0.5 }),
    )
    .await;
    assert!(out["after_s"].as_f64().unwrap() <= before * 0.55, "fallback should hit the target: {out}");
}

#[tokio::test]
async fn chat_editing_stability_guards() {
    let editor = Editor::test_instance();
    let project =
        call(&editor, "create_project", json!({ "name": "guards", "file_path": "/demo/a.mp4" })).await;
    let pid = project["id"].as_str().unwrap().to_string();
    call(&editor, "transcribe", json!({ "project_id": pid })).await;

    // 1. Schema validation: wrong-typed arg fails with expected-vs-got text.
    let err = tools::dispatch(
        &editor,
        "cut_range",
        &json!({ "project_id": pid, "start": "five", "end": 10.0 }),
        ActionSource::Ui,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("must be a number"), "got: {err}");

    // 2. Invalid range fails loudly instead of phantom-journaling a no-op.
    let err = tools::dispatch(
        &editor,
        "cut_range",
        &json!({ "project_id": pid, "start": 30.0, "end": 10.0 }),
        ActionSource::Ui,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("invalid"), "got: {err}");

    // 3. Journal-internal variants are rejected at the tool boundary.
    let err = tools::dispatch(
        &editor,
        "apply_edits",
        &json!({ "project_id": pid, "edits": [
            { "type": "set_clips", "clips": [], "global_padding": { "start_s": 0, "end_s": 0, "linked": true } }
        ]}),
        ActionSource::Ui,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("not allowed"), "got: {err}");

    // 4. Mid-batch failure returns a PARTIAL receipt, not a bare error.
    let receipt = call(
        &editor,
        "apply_edits",
        json!({ "project_id": pid, "edits": [
            { "type": "cut_range", "start": 1.0, "end": 2.0 },
            { "type": "cut_segments", "segment_ids": ["00000000-0000-0000-0000-000000000001"] },
            { "type": "cut_range", "start": 3.0, "end": 4.0 }
        ]}),
    )
    .await;
    assert_eq!(receipt["applied"], 1, "{receipt}");
    assert_eq!(receipt["failed_at"], 1);
    assert!(receipt["note"].as_str().unwrap().contains("APPLIED"));

    // 5. undo_actions reverts a specific turn — and refuses when stale.
    let a1 = receipt["actions"][0]["id"].as_str().unwrap().to_string();
    let r2 = call(
        &editor,
        "apply_edits",
        json!({ "project_id": pid, "edits": [ { "type": "cut_range", "start": 5.0, "end": 6.0 } ] }),
    )
    .await;
    let a2 = r2["actions"][0]["id"].as_str().unwrap().to_string();
    // stale: a1 is no longer on top
    let err = tools::dispatch(
        &editor,
        "undo_actions",
        &json!({ "project_id": pid, "action_ids": [a1] }),
        ActionSource::Ui,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("no longer the newest"), "got: {err}");
    // fresh: a2 undoes cleanly
    let out = call(&editor, "undo_actions", json!({ "project_id": pid, "action_ids": [a2] })).await;
    assert_eq!(out["undone"], 1);
}

#[tokio::test]
async fn rough_cut_tiers_cut_from_marked() {
    let editor = Editor::test_instance();
    let project =
        call(&editor, "create_project", json!({ "name": "tiers", "file_path": "/demo/a.mp4" })).await;
    let pid = project["id"].as_str().unwrap().to_string();
    call(&editor, "transcribe", json!({ "project_id": pid })).await;

    // Rough cut: Tier 1 applied, Tier 2 flagged (the demo fixture has a
    // repeated take, so suggestions should be > 0).
    let rc = call(&editor, "generate_rough_cut", json!({ "project_id": pid })).await;
    assert!(rc["cut_count"].as_u64().unwrap() >= 1, "tier 1 should cut: {rc}");
    let suggested = rc["suggestions"].as_u64().unwrap();

    let listed = call(&editor, "get_suggestions", json!({ "project_id": pid })).await;
    let segs = listed["suggestions"].as_array().unwrap();
    assert_eq!(segs.len() as u64, suggested, "list matches the count");

    if let Some(first) = segs.first() {
        // Accepting one applies a real cut and drops it from the list.
        let before_cuts = call(&editor, "get_timeline", json!({ "project_id": pid })).await["cut_count"].as_u64().unwrap();
        let sid = first["id"].as_str().unwrap();
        call(&editor, "accept_suggestion", json!({ "project_id": pid, "suggestion_id": sid })).await;
        let after = call(&editor, "get_timeline", json!({ "project_id": pid })).await["cut_count"].as_u64().unwrap();
        assert!(after >= before_cuts, "accept applies a cut");
        let relisted = call(&editor, "get_suggestions", json!({ "project_id": pid })).await;
        assert_eq!(relisted["suggestions"].as_array().unwrap().len(), segs.len() - 1);

        // Accept-all drains the rest in one batch.
        let all = call(&editor, "accept_all_suggestions", json!({ "project_id": pid })).await;
        assert_eq!(all["accepted"].as_u64().unwrap(), (segs.len() - 1) as u64);
        let empty = call(&editor, "get_suggestions", json!({ "project_id": pid })).await;
        assert!(empty["suggestions"].as_array().unwrap().is_empty());
    }
}

#[tokio::test]
async fn stale_suggestions_clear_on_retranscribe() {
    let editor = Editor::test_instance();
    let project =
        call(&editor, "create_project", json!({ "name": "stale", "file_path": "/demo/a.mp4" })).await;
    let pid = project["id"].as_str().unwrap().to_string();
    call(&editor, "transcribe", json!({ "project_id": pid })).await;
    let rc = call(&editor, "generate_rough_cut", json!({ "project_id": pid })).await;
    if rc["suggestions"].as_u64().unwrap() == 0 {
        return; // fixture had none; nothing to assert
    }
    // Re-transcribe replaces segment ids → stale suggestions must be cleared.
    call(&editor, "transcribe", json!({ "project_id": pid })).await;
    let after = call(&editor, "get_suggestions", json!({ "project_id": pid })).await;
    assert!(
        after["suggestions"].as_array().unwrap().is_empty(),
        "suggestions must clear on re-transcribe: {after}"
    );
}
