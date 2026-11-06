//! End-to-end exercise of the POC loop through the TOOL REGISTRY — the same
//! surface the UI, the local agent, and MCP clients use. Runs on the fixture
//! adapters, fully offline.

use roughcut_core::model::ActionSource;
use roughcut_core::tools::{self, Caller};
use roughcut_core::Editor;
use serde_json::{json, Value};

async fn call(editor: &Editor, name: &str, args: Value) -> Value {
    tools::dispatch(editor, name, &args, tools::Caller::Ui)
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

    // 2. One-click rough cut: silences + fillers PROPOSED, nothing cut until
    // the user applies them.
    let rough = call(&editor, "generate_rough_cut", json!({ "project_id": pid })).await;
    let cut_count = rough["cut_count"].as_u64().unwrap();
    assert!(cut_count >= 5, "rough cut should propose several cuts, got {cut_count}");
    assert_eq!(rough["applied"], false);
    let untouched = call(&editor, "get_timeline", json!({ "project_id": pid })).await;
    assert_eq!(untouched["cut_count"], 0, "proposing must not cut");
    let applied = call(&editor, "apply_cut_proposal", json!({ "project_id": pid })).await;
    assert_eq!(applied["applied"].as_u64().unwrap(), cut_count);
    assert!(applied["cut_count"].as_u64().unwrap() >= 1);

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

    // 6. Chat instruction → offline agent finds + PROPOSES; "apply" makes
    // the cut → undoable.
    let before_chat = call(&editor, "get_timeline", json!({ "project_id": pid })).await;
    let outcome = call(
        &editor,
        "apply_instruction",
        json!({ "project_id": pid, "instruction": "cut the part where I ramble about hiking on Saturday" }),
    )
    .await;
    assert!(outcome["summary"].as_str().unwrap().contains("Proposed"), "{}", outcome["summary"]);
    assert!(outcome["actions"].as_array().unwrap().is_empty(), "the chat proposes, it doesn't cut");
    let after_chat = call(&editor, "get_timeline", json!({ "project_id": pid })).await;
    assert_eq!(before_chat["clips"], after_chat["clips"], "proposing changed the timeline");
    let waiting = call(&editor, "get_cut_proposal", json!({ "project_id": pid })).await;
    assert!(waiting["proposal"]["total"].as_u64().unwrap() >= 1);
    let outcome = call(&editor, "apply_instruction", json!({ "project_id": pid, "instruction": "Apply!" })).await;
    // The chat's edits are the assistant's, not the user's clicks — even
    // though the UI started the run.
    let chat_edits = outcome["actions"].as_array().unwrap();
    assert!(!chat_edits.is_empty(), "\"apply\" should make the proposed cut");
    let waiting = call(&editor, "get_cut_proposal", json!({ "project_id": pid })).await;
    assert!(waiting["proposal"].is_null(), "applying clears the proposal");
    for action in chat_edits {
        assert_eq!(action["source"], "local_ai", "chat edit credited to {}", action["source"]);
    }
    let history = call(&editor, "get_history", json!({ "project_id": pid })).await;
    let newest = &history["entries"][history["current_index"].as_u64().unwrap() as usize];
    assert_eq!(newest["source"], "local_ai", "History shows the chat edit as the assistant's");
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
        ("txt", "txt", " "),
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
    // No discovery file: a test server must never overwrite the real app's mcp.json.
    let info = roughcut_core::mcp::server::start_with(editor.clone(), None).await.unwrap();
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
    for meta in ["apply_instruction"] {
        assert!(!tools.iter().any(|t| t["name"] == meta), "{meta} must not be listed over MCP");
    }

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

    // Hidden is not just unlisted: meta tools are refused over the wire too.
    for (id, meta) in [(5, "apply_instruction")] {
        let resp: Value = client
            .post(&info.url)
            .bearer_auth(&info.token)
            .json(&rpc("tools/call", json!({ "name": meta, "arguments": {} }), id))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(resp["result"]["isError"], true, "{meta} must be refused over MCP");
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("not available to MCP clients"), "{meta}: {text}");
    }
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
    let rough = call(&editor, "generate_rough_cut", json!({ "project_id": pid, "apply": true })).await;
    assert_eq!(rough["applied"], true);
    let after_rough = call(&editor, "get_timeline", json!({ "project_id": pid })).await;
    let clips = after_rough["clips"].as_array().unwrap();
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
    assert!(after_rough["cut_count"].as_u64().unwrap() > 2, "AI cuts should add to manual ones");

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
        after_rough["cut_count"].as_u64().unwrap()
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
        tools::Caller::Mcp,
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
        tools::Caller::Ui,
    )
    .await;
    assert!(ok.is_ok());
}

#[tokio::test]
async fn mcp_open_project_tells_the_app_to_show_it() {
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
    let p = call(&editor, "create_project", json!({ "name": "show me", "file_path": "/demo/x.mp4" })).await;
    let pid = p["id"].as_str().unwrap().to_string();
    let opened = |rx: &mut tokio::sync::mpsc::UnboundedReceiver<(String, Value)>| {
        let mut ids = Vec::new();
        while let Ok((name, payload)) = rx.try_recv() {
            if name == "project-opened" {
                ids.push(payload["project_id"].as_str().unwrap().to_string());
            }
        }
        ids
    };
    opened(&mut rx);

    // The app's own open (a refetch of what's on screen) must not move the view.
    tools::dispatch(&editor, "open_project", &json!({ "project_id": pid }), Caller::Ui).await.unwrap();
    assert!(opened(&mut rx).is_empty(), "UI opens must not send project-opened");

    // An MCP client opening it does.
    tools::dispatch(&editor, "open_project", &json!({ "project_id": pid }), Caller::Mcp).await.unwrap();
    assert_eq!(opened(&mut rx), vec![pid]);
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
        tools::Caller::Ui,
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
        tools::Caller::Ui,
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
        tools::Caller::Ui,
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
        tools::Caller::Ui,
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
        tools::Caller::Ui,
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

#[tokio::test]
async fn history_and_jump_in_time() {
    let editor = Editor::test_instance();
    let project =
        call(&editor, "create_project", json!({ "name": "hist", "file_path": "/demo/a.mp4" })).await;
    let pid = project["id"].as_str().unwrap().to_string();
    call(&editor, "transcribe", json!({ "project_id": pid })).await;
    // Three edits.
    let a1 = call(&editor, "cut_range", json!({ "project_id": pid, "start": 1.0, "end": 2.0 }))
        .await["action"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    call(&editor, "cut_range", json!({ "project_id": pid, "start": 3.0, "end": 4.0 })).await;
    call(&editor, "cut_range", json!({ "project_id": pid, "start": 5.0, "end": 6.0 })).await;

    let h = call(&editor, "get_history", json!({ "project_id": pid })).await;
    assert_eq!(h["entries"].as_array().unwrap().len(), 3);
    assert_eq!(h["current_index"], 2);
    let cuts_now =
        call(&editor, "get_timeline", json!({ "project_id": pid })).await["cut_count"]
            .as_u64()
            .unwrap();

    // Jump back to just after the first edit → 2 later cuts become "future".
    call(&editor, "jump_to", json!({ "project_id": pid, "action_id": a1 })).await;
    let h2 = call(&editor, "get_history", json!({ "project_id": pid })).await;
    assert_eq!(h2["current_index"], 0, "{h2}");
    let applied = h2["entries"].as_array().unwrap().iter().filter(|e| e["applied"] == true).count();
    assert_eq!(applied, 1);
    let cuts_back =
        call(&editor, "get_timeline", json!({ "project_id": pid })).await["cut_count"]
            .as_u64()
            .unwrap();
    assert!(cuts_back < cuts_now, "jumping back reduces cuts");

    // Jump to the original (null) → no cuts, all 3 are future.
    call(&editor, "jump_to", json!({ "project_id": pid, "action_id": null })).await;
    let h3 = call(&editor, "get_history", json!({ "project_id": pid })).await;
    assert_eq!(h3["current_index"], -1);
    assert!(h3["entries"].as_array().unwrap().iter().all(|e| e["applied"] == false));

    // Jump forward to the newest (redo all) restores everything.
    let last = h["entries"][2]["id"].as_str().unwrap();
    call(&editor, "jump_to", json!({ "project_id": pid, "action_id": last })).await;
    assert_eq!(
        call(&editor, "get_timeline", json!({ "project_id": pid })).await["cut_count"]
            .as_u64()
            .unwrap(),
        cuts_now
    );
}

#[tokio::test]
async fn every_caller_can_call_exactly_what_it_is_shown() {
    let editor = Editor::test_instance();
    let every: Vec<String> = tools::defs(Caller::Ui).into_iter().map(|d| d.name).collect();
    for caller in [Caller::Ui, Caller::Mcp, Caller::Agent { attribute_to: ActionSource::LocalAi }] {
        let shown: std::collections::HashSet<String> =
            tools::defs(caller).into_iter().map(|d| d.name).collect();
        for name in &every {
            // Empty args: a shown tool fails on its arguments (or runs a
            // harmless read); a hidden one is refused before its handler runs.
            let refused = match tools::dispatch(&editor, name, &json!({}), caller).await {
                Err(e) => e.to_string().contains("not available to"),
                Ok(_) => false,
            };
            assert_eq!(
                refused,
                !shown.contains(name),
                "{caller:?} × {name}: listing and dispatch disagree"
            );
        }
    }

    // The agent's repair hint lists only what it can actually call.
    let agent = Caller::Agent { attribute_to: ActionSource::LocalAi };
    let err = tools::dispatch(&editor, "export", &json!({}), agent).await.unwrap_err().to_string();
    let available = err.split("available: ").nth(1).unwrap_or_default();
    assert!(available.contains("cut_range"), "got: {err}");
    for hidden in ["export", "delete_project", "set_preferences", "apply_instruction"] {
        assert!(!available.split(", ").any(|n| n == hidden), "{hidden} advertised: {err}");
    }
}

#[tokio::test]
async fn the_model_switch_is_not_a_preference_any_caller_can_set() {
    let editor = Editor::test_instance();
    let prefs = call(&editor, "get_preferences", json!({})).await;
    assert_eq!(prefs["model_mode"], "local", "Local is the default");
    assert_eq!(prefs["hosted_model"], "claude-opus-5-5");
    for caller in [Caller::Ui, Caller::Mcp] {
        for patch in [
            json!({ "model_mode": "hosted" }),
            json!({ "hosted_model": "claude-haiku-5-5" }),
            json!({ "hosted_consent_at": "2026-10-31T12:00:00Z" }),
        ] {
            let err = tools::dispatch(&editor, "set_preferences", &json!({ "preferences": patch }), caller)
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("can only be changed in the app"), "{caller:?} {patch}: {err}");
        }
    }
    // Sending the current values back (a whole-object save) is fine.
    let same = json!({ "preferences": { "model_mode": "local", "cut_aggressiveness": "natural" } });
    call(&editor, "set_preferences", same).await;
}

// ------------------------------------------------ Hosted mode, faked

mod hosted_fakes {
    use async_trait::async_trait;
    use roughcut_core::adapters::anthropic::{HostedBackend, HostedFailure};
    use roughcut_core::adapters::inference::{ChatMessage, ChatRequest, ChatResponse, InferenceClient, ServedBy};
    use roughcut_core::error::{CoreError, HostedErrorKind, Result};
    use roughcut_core::model::RouteReason;
    use std::sync::Mutex;

    fn schema_name(req: &ChatRequest) -> Option<String> {
        req.response_format.as_ref().and_then(|f| f["json_schema"]["name"].as_str()).map(str::to_string)
    }

    /// Picks for a short_picks request: runs of consecutive lines lasting
    /// 20-45 s (by the times the prompt shows), from the top of the topic,
    /// at most two, every rating `rating`.
    fn picks(req: &ChatRequest, rating: u32) -> String {
        let user = req.messages.iter().rev().find(|m| m.role == "user").and_then(|m| m.content.clone()).unwrap_or_default();
        let clock = |t: &str| -> Option<f64> {
            let (m, s) = t.split_once(':')?;
            Some(m.parse::<f64>().ok()? * 60.0 + s.parse::<f64>().ok()?)
        };
        let mut lines: Vec<(usize, f64)> = vec![];
        let mut end = 0.0;
        for l in user.lines() {
            if let Some((n, rest)) = l.split_once("| [") {
                if let (Ok(n), Some(t)) = (n.trim().parse::<usize>(), rest.split(']').next().and_then(clock)) {
                    lines.push((n, t));
                }
            } else if l.ends_with("end of topic") {
                end = l.trim_start_matches('[').split(']').next().and_then(clock).unwrap_or(0.0);
            }
        }
        let at = |i: usize| lines.get(i).map(|l| l.1).unwrap_or(end);
        let mut items: Vec<serde_json::Value> = vec![];
        let mut i = 0;
        while i < lines.len() && items.len() < 2 {
            let mut best = None;
            for j in i..lines.len() {
                let len = at(j + 1) - at(i);
                if len > 45.0 {
                    break;
                }
                if len >= 20.0 {
                    best = Some(j);
                }
            }
            match best {
                Some(j) => {
                    items.push(serde_json::json!({
                        "first": lines[i].0, "last": lines[j].0, "hook": rating, "standalone": rating,
                        "payoff": rating, "ending": rating, "title": format!("T{}", lines[i].0), "reason": "r"
                    }));
                    i = j + 1;
                }
                None => i += 1,
            }
        }
        serde_json::to_string(&items).unwrap()
    }

    /// Claude: picks shorts until `fail_from` (1-based pick request), then
    /// is unreachable; or rejects the key on every request.
    pub struct Claude {
        pub reject: bool,
        pub fail_from: usize,
        pub calls: Mutex<usize>,
    }

    #[async_trait]
    impl HostedBackend for Claude {
        async fn send(&self, req: &ChatRequest) -> std::result::Result<ChatResponse, HostedFailure> {
            if self.reject {
                return Err(HostedFailure::Fatal(CoreError::hosted(
                    HostedErrorKind::KeyRejected,
                    "Anthropic rejected the API key (401). Update it in Setup.",
                )));
            }
            let text = if schema_name(req).as_deref() == Some("short_picks") {
                let mut n = self.calls.lock().unwrap();
                *n += 1;
                if *n >= self.fail_from {
                    return Err(HostedFailure::Unreachable { reason: RouteReason::Offline, detail: "no network".into() });
                }
                picks(req, 4)
            } else {
                "not json".into() // the outline falls back to pause-based beats
            };
            Ok(ChatResponse {
                message: ChatMessage::assistant(&text),
                served_by: Some(ServedBy { model: req.model.clone(), hosted: true }),
                usage: None,
            })
        }
    }

    /// The local model: picks shorts too.
    pub struct Local;

    #[async_trait]
    impl InferenceClient for Local {
        async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
            let text = if schema_name(&req).as_deref() == Some("short_picks") { picks(&req, 5) } else { "not json".into() };
            Ok(ChatResponse::new(ChatMessage::assistant(&text)))
        }
        async fn healthy(&self) -> bool {
            true
        }
    }
}

async fn hosted_project(claude: hosted_fakes::Claude) -> (Editor, String) {
    let editor = Editor::test_instance();
    editor.set_inference_for_tests(std::sync::Arc::new(hosted_fakes::Local), std::sync::Arc::new(claude));
    editor.set_hosted_key_for_tests(Some("sk-test")).unwrap();
    editor.set_model_mode(roughcut_core::model::ModelMode::Hosted, true).unwrap();
    let p = call(&editor, "create_project", json!({ "name": "hosted", "file_path": "/demo/footage.mp4" })).await;
    let pid = p["id"].as_str().unwrap().to_string();
    call(&editor, "transcribe", json!({ "project_id": pid })).await;
    (editor, pid)
}

#[tokio::test]
async fn a_rejected_key_stops_a_scan_and_a_chat_and_changes_nothing() {
    let claude = hosted_fakes::Claude { reject: true, fail_from: usize::MAX, calls: Default::default() };
    let (editor, pid) = hosted_project(claude).await;
    let before = call(&editor, "get_clip_candidates", json!({ "project_id": pid })).await;

    let err = tools::dispatch(&editor, "find_clips", &json!({ "project_id": pid, "scan": true }), Caller::Ui)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "hosted_key_rejected");
    assert!(err.to_string().contains("Update it in Setup"), "{err}");
    let after = call(&editor, "get_clip_candidates", json!({ "project_id": pid })).await;
    assert_eq!(before, after, "the stored candidates are untouched");

    // The chat agent doesn't fall back to offline keyword cuts.
    let timeline = call(&editor, "get_timeline", json!({ "project_id": pid })).await;
    let err = tools::dispatch(
        &editor,
        "apply_instruction",
        &json!({ "project_id": pid, "instruction": "cut the tangent about hiking" }),
        Caller::Ui,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), "hosted_key_rejected");
    let unchanged = call(&editor, "get_timeline", json!({ "project_id": pid })).await;
    assert_eq!(timeline["cut_count"], unchanged["cut_count"]);
    assert_eq!(timeline["clips"].as_array().unwrap().len(), unchanged["clips"].as_array().unwrap().len());

    // A story edit plans before it changes anything: no dead-air cuts land
    // ahead of the rejected request.
    let err = tools::dispatch(&editor, "story_edit", &json!({ "project_id": pid, "instruction": "tighten it" }), Caller::Ui)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "hosted_key_rejected");
    let still = call(&editor, "get_timeline", json!({ "project_id": pid })).await;
    assert_eq!(timeline["cut_count"], still["cut_count"]);
    let status = editor.model_status().unwrap();
    assert_eq!(status.reason, Some(roughcut_core::model::RouteReason::KeyRejected));
}

#[tokio::test]
async fn a_scan_that_loses_the_network_says_which_model_scored_what() {
    let claude = hosted_fakes::Claude { reject: false, fail_from: 2, calls: Default::default() };
    let (editor, pid) = hosted_project(claude).await;
    let found = call(&editor, "find_clips", json!({ "project_id": pid, "count": 20 })).await;
    assert_eq!(found["method"], "llm");
    let scorers: std::collections::BTreeSet<String> = found["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["scorer"].as_str().map(str::to_string))
        .collect();
    let local_model = editor.get_preferences().unwrap().inference_model;
    assert!(scorers.contains("Claude Opus 5.5"), "{scorers:?}");
    assert!(scorers.contains(&local_model), "{scorers:?}");
    let status = editor.model_status().unwrap();
    assert_eq!(status.effective, roughcut_core::model::ModelMode::Local);
    assert_eq!(status.reason, Some(roughcut_core::model::RouteReason::Offline));
}

#[tokio::test]
async fn set_preferences_patches_and_guards_egress() {
    let editor = Editor::test_instance();
    let before = call(&editor, "get_preferences", json!({})).await;

    // The Inspector sends ONE key; every other preference must survive.
    let after = call(
        &editor,
        "set_preferences",
        json!({ "preferences": { "cut_aggressiveness": "aggressive" } }),
    )
    .await;
    assert_eq!(after["cut_aggressiveness"], "aggressive");
    for (k, v) in before.as_object().unwrap() {
        if k != "cut_aggressiveness" {
            assert_eq!(&after[k], v, "{k} changed");
        }
    }
    // Nested objects merge rather than replace.
    let after = call(
        &editor,
        "set_preferences",
        json!({ "preferences": { "default_padding": { "end_s": 0.4 } } }),
    )
    .await;
    assert_eq!(after["default_padding"]["end_s"], 0.4);
    assert_eq!(after["default_padding"]["start_s"], before["default_padding"]["start_s"]);
    // Persisted, not just echoed.
    let stored = call(&editor, "get_preferences", json!({})).await;
    assert_eq!(stored["cut_aggressiveness"], "aggressive");

    // A typo is rejected, not silently dropped.
    let err = tools::dispatch(
        &editor,
        "set_preferences",
        &json!({ "preferences": { "cut_agressiveness": "natural" } }),
        Caller::Ui,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("unknown preference(s): cut_agressiveness"), "got: {err}");

    // Zero egress: an external client can't point inference off-machine…
    let remote = json!({ "preferences": { "inference_endpoint": "https://llm.example.com/v1" } });
    let err = tools::dispatch(&editor, "set_preferences", &remote, Caller::Mcp)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("not on this machine"), "got: {err}");
    // …and the agent loop isn't even shown the tool.
    let agent = Caller::Agent { attribute_to: ActionSource::LocalAi };
    let err = tools::dispatch(&editor, "set_preferences", &remote, agent).await.unwrap_err().to_string();
    assert!(err.contains("not available to the agent loop"), "got: {err}");
    let stored = call(&editor, "get_preferences", json!({})).await;
    assert_eq!(stored["inference_endpoint"], before["inference_endpoint"]);

    // Another LOCAL server is fine from an external client.
    let local = json!({ "preferences": { "inference_endpoint": "http://127.0.0.1:47861/v1" } });
    let out = tools::dispatch(&editor, "set_preferences", &local, Caller::Mcp).await.unwrap();
    assert_eq!(out["inference_endpoint"], "http://127.0.0.1:47861/v1");

    // The user can choose a remote endpoint deliberately, and external
    // clients can still change OTHER preferences afterwards.
    call(&editor, "set_preferences", remote.clone()).await;
    let out = tools::dispatch(
        &editor,
        "set_preferences",
        &json!({ "preferences": { "silence_min_duration_s": 1.2 } }),
        Caller::Mcp,
    )
    .await
    .unwrap();
    assert_eq!(out["inference_endpoint"], "https://llm.example.com/v1");
    assert_eq!(out["silence_min_duration_s"], 1.2);
}

#[tokio::test]
async fn clips_find_save_export() {
    let editor = Editor::test_instance();
    let p = call(&editor, "create_project", json!({ "name": "long-form", "file_path": "/demo/footage.mp4" })).await;
    let pid = p["id"].as_str().unwrap().to_string();

    // Before a transcript there is nothing to find.
    let err = tools::dispatch(&editor, "find_clips", &json!({ "project_id": pid }), Caller::Ui)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("transcribe first"), "got: {err}");

    call(&editor, "transcribe", json!({ "project_id": pid })).await;
    call(&editor, "generate_rough_cut", json!({ "project_id": pid })).await;

    // 1. Find: ranked, in bounds, distinct moments, persisted.
    let found = call(
        &editor,
        "find_clips",
        json!({ "project_id": pid, "count": 4, "min_s": 15, "max_s": 45 }),
    )
    .await;
    assert_eq!(found["method"], "heuristic", "tests have no model server");
    let candidates = found["candidates"].as_array().unwrap().clone();
    assert!(!candidates.is_empty() && candidates.len() <= 4, "got {}", candidates.len());
    let scores: Vec<f64> = candidates.iter().map(|c| c["score"].as_f64().unwrap()).collect();
    assert!(scores.windows(2).all(|w| w[0] >= w[1]), "best first: {scores:?}");
    for c in &candidates {
        let d = c["duration"].as_f64().unwrap();
        assert!((15.0..=45.0).contains(&d), "duration {d}");
        assert!(!c["title"].as_str().unwrap().is_empty());
        assert!(!c["hook"].as_str().unwrap().is_empty());
        assert!(!c["reason"].as_str().unwrap().is_empty());
    }
    let stored = call(&editor, "get_clip_candidates", json!({ "project_id": pid })).await;
    assert_eq!(stored["candidates"].as_array().unwrap().len(), candidates.len());

    // 2. Create from a candidate: the clip plays exactly the candidate's
    // stretch of the current cut, and the candidate leaves the list.
    let best = &candidates[0];
    let created = call(
        &editor,
        "create_clip",
        json!({ "project_id": pid, "candidate_id": best["id"] }),
    )
    .await;
    let clip = &created["clip"];
    let sid = clip["id"].as_str().unwrap().to_string();
    assert_eq!(clip["title"], best["title"]);
    assert!((clip["duration"].as_f64().unwrap() - best["duration"].as_f64().unwrap()).abs() < 0.1);
    let stored = call(&editor, "get_clip_candidates", json!({ "project_id": pid })).await;
    assert_eq!(stored["candidates"].as_array().unwrap().len(), candidates.len() - 1);

    // A hand-picked range works too; a bad one names the problem.
    let ranged = call(
        &editor,
        "create_clip",
        json!({ "project_id": pid, "start": 10.0, "end": 40.0, "title": "My pick" }),
    )
    .await;
    assert_eq!(ranged["clip"]["title"], "My pick");
    let err = tools::dispatch(
        &editor,
        "create_clip",
        &json!({ "project_id": pid, "start": 40.0, "end": 10.0 }),
        Caller::Ui,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("invalid"), "got: {err}");

    // 3. Saved clips are independent of the main cut: undoing the rough cut moves
    // the main timeline, not the clip.
    let before = call(&editor, "list_clips", json!({ "project_id": pid })).await["clips"][0]["duration"]
        .as_f64()
        .unwrap();
    call(&editor, "undo", json!({ "project_id": pid })).await;
    let after = call(&editor, "list_clips", json!({ "project_id": pid })).await["clips"][0]["duration"]
        .as_f64()
        .unwrap();
    assert!((before - after).abs() < 1e-9);

    // 4. Update: rename + re-range.
    let start = clip["start"].as_f64().unwrap();
    let updated = call(
        &editor,
        "update_clip",
        json!({ "project_id": pid, "clip_id": sid, "title": "Renamed", "start": start, "end": start + 20.0 }),
    )
    .await;
    assert_eq!(updated["clip"]["title"], "Renamed");
    assert!(updated["clip"]["duration"].as_f64().unwrap() <= 20.0 + 1e-6);

    // 5. Export: the same writers over the clip's timeline. Captions start
    // at the clip's own 00:00:00, not the source time.
    let dir = std::env::temp_dir().join("roughcut-e2e-clips");
    for (target, needle) in [("edl", "TITLE:"), ("srt", "00:00:00,"), ("txt", " ")] {
        let out = dir.join(format!("clip.{target}"));
        let r = call(
            &editor,
            "export_clip",
            json!({ "project_id": pid, "clip_id": sid, "target": target, "out_path": out.to_string_lossy() }),
        )
        .await;
        let content = std::fs::read_to_string(r["path"].as_str().unwrap()).unwrap();
        assert!(content.contains(needle), "{target} export missing {needle}: {content}");
        if target == "edl" {
            assert!(content.contains("Renamed"), "EDL titled after the clip: {content}");
        }
    }

    // 6. Persisted with the project; delete removes it.
    let project = call(&editor, "open_project", json!({ "project_id": pid })).await;
    assert_eq!(project["saved_clips"].as_array().unwrap().len(), 2);
    call(&editor, "delete_clip", json!({ "project_id": pid, "clip_id": sid })).await;
    let listed = call(&editor, "list_clips", json!({ "project_id": pid })).await;
    assert_eq!(listed["clips"].as_array().unwrap().len(), 1);

    // The local agent can find and save clips but not export them.
    let agent = Caller::Agent { attribute_to: ActionSource::LocalAi };
    let names: Vec<String> = tools::defs(agent).into_iter().map(|d| d.name).collect();
    assert!(names.contains(&"find_clips".to_string()));
    assert!(!names.contains(&"export_clip".to_string()));
}

#[tokio::test]
async fn bulk_shorts_scan_make_export() {
    let editor = Editor::test_instance();
    let p = call(&editor, "create_project", json!({ "name": "long-form", "file_path": "/demo/footage.mp4" })).await;
    let pid = p["id"].as_str().unwrap().to_string();
    call(&editor, "transcribe", json!({ "project_id": pid })).await;

    // Without the new arguments: today's shape, plus one piece per candidate.
    let plain = call(&editor, "find_clips", json!({ "project_id": pid })).await;
    let plain = plain["candidates"].as_array().unwrap().clone();
    assert!(!plain.is_empty() && plain.len() <= 8, "got {}", plain.len());
    for c in &plain {
        let pieces = c["pieces"].as_array().unwrap();
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0]["start"], c["start"]);
        assert_eq!(pieces[0]["end"], c["end"]);
    }

    // A scan: more of the video, still distinct moments.
    let scan = call(&editor, "find_clips", json!({ "project_id": pid, "scan": true, "count": 30, "min_score": 40 })).await;
    assert_eq!(scan["method"], "heuristic", "tests have no model server");
    let candidates = scan["candidates"].as_array().unwrap().clone();
    assert!(candidates.len() >= 3 && candidates.len() <= 30, "got {}", candidates.len());
    assert!(candidates.len() >= plain.len(), "a scan offers at least as many");

    // One scan per project at a time.
    let held = editor.begin_clip_scan(pid.parse().unwrap()).unwrap();
    let err = tools::dispatch(&editor, "find_clips", &json!({ "project_id": pid, "scan": true }), Caller::Ui)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("already running"), "got: {err}");
    drop(held);

    // Make three, one of them made stale by cutting its whole stretch from
    // the main edit after the scan.
    let ids: Vec<Value> = candidates[..3].iter().map(|c| c["id"].clone()).collect();
    let stale = &candidates[2];
    call(&editor, "cut_range", json!({ "project_id": pid, "start": stale["start"], "end": stale["end"] })).await;
    let made = call(&editor, "create_clips", json!({ "project_id": pid, "candidate_ids": ids })).await;
    let clips = made["clips"].as_array().unwrap();
    let errors = made["errors"].as_array().unwrap();
    assert_eq!(clips.len(), 2, "{made}");
    assert_eq!(errors.len(), 1, "{made}");
    assert_eq!(errors[0]["candidate_id"], stale["id"]);
    for c in clips {
        assert_eq!(c["format"], "vertical");
        assert_eq!(c["crop_x"], 0.5, "no frames to look at in tests: the crop stays centred");
    }
    let listed = call(&editor, "get_clip_candidates", json!({ "project_id": pid })).await;
    let listed: Vec<&Value> = listed["candidates"].as_array().unwrap().iter().collect();
    assert!(listed.iter().any(|c| c["id"] == stale["id"]), "the stale one stays listed");
    assert!(!listed.iter().any(|c| c["id"] == ids[0] || c["id"] == ids[1]), "made ones leave the list");

    // An empty batch names the problem.
    let err = tools::dispatch(&editor, "create_clips", &json!({ "project_id": pid, "candidate_ids": [] }), Caller::Ui)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("empty"), "got: {err}");

    // Re-scanning doesn't offer the moments just saved.
    let again = call(&editor, "find_clips", json!({ "project_id": pid, "scan": true, "count": 30 })).await;
    let overlap = |a: (f64, f64), b: (f64, f64)| {
        let shared = (a.1.min(b.1) - a.0.max(b.0)).max(0.0);
        shared / (a.1 - a.0).min(b.1 - b.0)
    };
    for c in again["candidates"].as_array().unwrap() {
        let cand = (c["start"].as_f64().unwrap(), c["end"].as_f64().unwrap());
        for s in clips {
            let saved = (s["start"].as_f64().unwrap(), s["end"].as_f64().unwrap());
            assert!(overlap(cand, saved) <= 0.3, "{cand:?} is the saved moment {saved:?}");
        }
    }

    // Export the batch. Captions work offline; the MP4 needs ffmpeg, so in
    // tests every MP4 fails — and a failure doesn't stop the rest.
    let dir = std::env::temp_dir().join(format!("roughcut-e2e-batch-{}", uuid::Uuid::new_v4()));
    let clip_ids: Vec<Value> = clips.iter().map(|c| c["id"].clone()).collect();
    let mut with_bogus = clip_ids.clone();
    with_bogus.insert(1, json!(uuid::Uuid::new_v4().to_string()));
    let out = call(
        &editor,
        "export_clips",
        json!({ "project_id": pid, "clip_ids": with_bogus, "out_dir": dir.to_string_lossy(), "target": "srt" }),
    )
    .await;
    let results = out["results"].as_array().unwrap();
    assert_eq!(results.len(), 3);
    assert!(results[1]["error"].is_string() && results[1]["path"].is_null());
    let file_name = |path: &str| std::path::Path::new(path).file_name().unwrap().to_string_lossy().into_owned();
    let first = results[0]["path"].as_str().unwrap();
    assert!(first.ends_with(".srt") && file_name(first).starts_with("01-"), "{first}");
    assert!(file_name(results[2]["path"].as_str().unwrap()).starts_with("03-"));
    assert!(std::path::Path::new(first).exists());

    // Again into the same folder: nothing is overwritten.
    let again = call(
        &editor,
        "export_clips",
        json!({ "project_id": pid, "clip_ids": [clip_ids[0]], "out_dir": dir.to_string_lossy(), "target": "srt" }),
    )
    .await;
    let second = again["results"][0]["path"].as_str().unwrap();
    assert!(second.ends_with("-2.srt"), "{second}");

    let mp4 = call(
        &editor,
        "export_clips",
        json!({ "project_id": pid, "clip_ids": clip_ids, "out_dir": dir.to_string_lossy() }),
    )
    .await;
    assert!(mp4["results"].as_array().unwrap().iter().all(|r| r["error"].is_string()));
    let _ = std::fs::remove_dir_all(&dir);

    // The local agent can make shorts in a batch but not export them.
    let agent = Caller::Agent { attribute_to: ActionSource::LocalAi };
    let names: Vec<String> = tools::defs(agent).into_iter().map(|d| d.name).collect();
    assert!(names.contains(&"create_clips".to_string()));
    assert!(!names.contains(&"export_clips".to_string()));
}

#[tokio::test]
async fn clip_face_framing_without_frames_or_on_a_composite() {
    let editor = Editor::test_instance();
    let p = call(&editor, "create_project", json!({ "name": "faces", "file_path": "/demo/footage.mp4" })).await;
    let pid = p["id"].as_str().unwrap().to_string();

    // A new 9:16 clip kicks off a background face pass. Demo mode has no
    // frames to grab, so it fails quietly and the crop stays centred.
    let created = call(
        &editor,
        "create_clip",
        json!({ "project_id": pid, "start": 10.0, "end": 40.0, "format": "vertical" }),
    )
    .await;
    let cid = created["clip"]["id"].as_str().unwrap().to_string();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let listed = call(&editor, "list_clips", json!({ "project_id": pid })).await;
    assert_eq!(listed["clips"][0]["crop_x"], 0.5);

    // Asked directly, it says why.
    let err = tools::dispatch(&editor, "center_clip_on_face", &json!({ "project_id": pid, "clip_id": cid }), Caller::Ui)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("ffmpeg"), "got: {err}");

    // A screen + camera project crops the composite: no auto framing.
    let id = uuid::Uuid::parse_str(&pid).unwrap();
    editor.attach_screen_media(id, "/demo/screen.mp4").await.unwrap();
    let err = tools::dispatch(&editor, "center_clip_on_face", &json!({ "project_id": pid, "clip_id": cid }), Caller::Ui)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("screen recording"), "got: {err}");
}

#[tokio::test]
async fn clips_by_theme_and_from_chat() {
    let editor = Editor::test_instance();
    let p = call(&editor, "create_project", json!({ "name": "themed", "file_path": "/demo/footage.mp4" })).await;
    let pid = p["id"].as_str().unwrap().to_string();
    let transcript = call(&editor, "transcribe", json!({ "project_id": pid })).await;
    let hiking = transcript["segments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["text"].as_str().unwrap_or("").contains("hiking"))
        .expect("fixture has the hiking tangent")["id"]
        .clone();

    // A theme keeps only moments about it, and is reported back.
    let found = call(&editor, "find_clips", json!({ "project_id": pid, "theme": "hiking" })).await;
    assert_eq!(found["theme"], "hiking");
    let candidates = found["candidates"].as_array().unwrap();
    assert!(!candidates.is_empty());
    for c in candidates {
        assert!(c["segment_ids"].as_array().unwrap().contains(&hiking), "off-theme candidate: {c}");
        assert_eq!(c["theme"], "hiking");
        assert!(c["reason"].as_str().unwrap().contains("On theme") || c["reason"].as_str().unwrap().contains("on theme"));
    }

    // A theme nothing matches says so instead of returning random clips.
    let err = tools::dispatch(
        &editor,
        "find_clips",
        &json!({ "project_id": pid, "theme": "quantum chromodynamics" }),
        Caller::Ui,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("matches the theme"), "got: {err}");

    // Create by rank (what the chat agent uses — positions, not uuids).
    let created = call(&editor, "create_clip", json!({ "project_id": pid, "rank": 1 })).await;
    assert_eq!(created["clip"]["title"], candidates[0]["title"]);
    let err = tools::dispatch(&editor, "create_clip", &json!({ "project_id": pid, "rank": 99 }), Caller::Ui)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("out of range"), "got: {err}");

    // Chat with no model running: a shorts request containing "cut" finds clips
    // and never touches the main video.
    let before = call(&editor, "get_timeline", json!({ "project_id": pid })).await["cut_count"].clone();
    let out = call(
        &editor,
        "apply_instruction",
        json!({ "project_id": pid, "instruction": "cut me a couple of shorts about hiking" }),
    )
    .await;
    assert!(out["actions"].as_array().unwrap().is_empty(), "no edits: {out}");
    assert!(out["summary"].as_str().unwrap().contains("Clips tab"), "{out}");
    let after = call(&editor, "get_timeline", json!({ "project_id": pid })).await["cut_count"].clone();
    assert_eq!(before, after, "the main video is untouched");
    let stored = call(&editor, "get_clip_candidates", json!({ "project_id": pid })).await;
    assert_eq!(stored["candidates"][0]["theme"], "hiking");
}

#[tokio::test]
async fn clips_from_several_pieces_in_either_format() {
    let editor = Editor::test_instance();
    let p = call(&editor, "create_project", json!({ "name": "pieces", "file_path": "/demo/footage.mp4" })).await;
    let pid = p["id"].as_str().unwrap().to_string();
    let transcript = call(&editor, "transcribe", json!({ "project_id": pid })).await;
    let speech: Vec<&Value> = transcript["segments"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| !s["is_silence"].as_bool().unwrap() && !s["is_filler"].as_bool().unwrap())
        .collect();
    let (a, b) = (speech[1], speech[speech.len() - 2]);
    let len = |s: &Value| s["end"].as_f64().unwrap() - s["start"].as_f64().unwrap();

    // Two sentences from opposite ends of the video, picked by id: one clip,
    // played back to back, 16:9 by default.
    let saved = call(
        &editor,
        "create_clip",
        json!({ "project_id": pid, "segment_ids": [b["id"], a["id"]], "title": "Bookends" }),
    )
    .await;
    let clip = &saved["clip"];
    assert_eq!(clip["format"], "landscape");
    assert_eq!(clip["ranges"].as_array().unwrap().len(), 2, "two pieces, in source order");
    assert!(clip["ranges"][0]["start"].as_f64().unwrap() < clip["ranges"][1]["start"].as_f64().unwrap());
    assert!((clip["duration"].as_f64().unwrap() - (len(a) + len(b))).abs() < 0.1);
    let cid = clip["id"].as_str().unwrap().to_string();

    // Word-level ranges work too; touching pieces merge.
    let ranged = call(
        &editor,
        "create_clip",
        json!({ "project_id": pid, "format": "vertical", "ranges": [
            { "start": 10.0, "end": 20.0 }, { "start": 20.0, "end": 25.0 }, { "start": 100.0, "end": 110.0 }
        ] }),
    )
    .await;
    assert_eq!(ranged["clip"]["format"], "vertical");
    assert_eq!(ranged["clip"]["ranges"].as_array().unwrap().len(), 2);

    // Switch format and crop; bad values say what's expected.
    let updated = call(
        &editor,
        "update_clip",
        json!({ "project_id": pid, "clip_id": cid, "format": "vertical", "crop_x": 0.3 }),
    )
    .await;
    assert_eq!(updated["clip"]["format"], "vertical");
    assert_eq!(updated["clip"]["crop_x"], 0.3);
    let err = tools::dispatch(
        &editor,
        "update_clip",
        &json!({ "project_id": pid, "clip_id": cid, "format": "square" }),
        Caller::Ui,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("landscape"), "got: {err}");

    // The EDL plays the two pieces back to back.
    let out = std::env::temp_dir().join("roughcut-e2e-clips").join("bookends.edl");
    let r = call(
        &editor,
        "export_clip",
        json!({ "project_id": pid, "clip_id": cid, "target": "edl", "out_path": out.to_string_lossy() }),
    )
    .await;
    let edl = std::fs::read_to_string(r["path"].as_str().unwrap()).unwrap();
    let events = edl.lines().filter(|l| l.starts_with("00")).count();
    assert!(events >= 2, "one event per piece: {edl}");

    // Nothing here touched the main video.
    let tl = call(&editor, "get_timeline", json!({ "project_id": pid })).await;
    assert_eq!(tl["cut_count"], 0);
}

#[tokio::test]
async fn import_into_a_project_relinks_the_same_video_and_keeps_the_edit() {
    let editor = Editor::test_instance();
    let p = call(&editor, "create_project", json!({ "name": "moved", "file_path": "/demo/old/footage.mp4" })).await;
    let pid = p["id"].as_str().unwrap().to_string();
    call(&editor, "cut_range", json!({ "project_id": pid, "start": 10.0, "end": 20.0 })).await;

    // The fixture probe reports the same video for any path: a relink.
    let media = call(
        &editor,
        "import_media",
        json!({ "project_id": pid, "file_path": "/demo/new/footage.mp4" }),
    )
    .await;
    assert_eq!(media["file_path"], "/demo/new/footage.mp4");
    assert_eq!(media["id"], p["media"]["id"], "relink keeps the media id");
    let timeline = call(&editor, "get_timeline", json!({ "project_id": pid })).await;
    assert_eq!(timeline["cut_count"], 1, "the cut survives the relink");
    let undone = call(&editor, "undo", json!({ "project_id": pid })).await;
    assert!(undone["action"].is_object(), "and so does the undo history");
}

#[tokio::test]
async fn cut_passage_cuts_the_whole_span_with_clean_edges() {
    let editor = Editor::test_instance();
    let project =
        call(&editor, "create_project", json!({ "name": "passage", "file_path": "/demo/a.mp4" })).await;
    let pid = project["id"].as_str().unwrap().to_string();
    let t = call(&editor, "transcribe", json!({ "project_id": pid })).await;
    let speech: Vec<&Value> = t["segments"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["is_silence"] != true)
        .collect();
    let (first, last) = (speech[3], speech[6]);
    let before: f64 = speech.iter().map(|s| s["end"].as_f64().unwrap() - s["start"].as_f64().unwrap()).sum();
    let r = call(
        &editor,
        "cut_passage",
        json!({ "project_id": pid, "from_segment_id": first["id"], "to_segment_id": last["id"] }),
    )
    .await;
    let (cs, ce) = (r["cut"]["start"].as_f64().unwrap(), r["cut"]["end"].as_f64().unwrap());
    let (fs, le) = (first["start"].as_f64().unwrap(), last["end"].as_f64().unwrap());
    // The cut covers the passage, give or take the edge snap.
    assert!((cs - fs).abs() <= 10.0 && (ce - le).abs() <= 10.0, "edges moved too far: {r}");
    assert!(ce - cs >= (le - fs) * 0.5, "most of the passage is cut: {r}");
    assert!(r["cut"]["starts_with"].as_str().unwrap().len() > 0, "{r}");
    assert_eq!(r["timeline"]["cut_count"], 1, "one cut, not one per segment: {r}");
    let _ = before;

    // A whole-video "passage" is refused: that's a length request.
    let refused = tools::dispatch(
        &editor,
        "cut_passage",
        &json!({ "project_id": pid, "start": 0.0, "end": 4000.0 }),
        Caller::Ui,
    )
    .await;
    assert!(refused.is_err() || refused.unwrap()["cut"]["seconds"].as_f64().unwrap() < 1800.0);
}

#[tokio::test]
async fn the_rough_cut_never_leaves_a_sliver_cut() {
    // Padding used to be applied after the rough cut, shrinking every cut by
    // start + end padding: a 0.3 s "um" became a 0.01 s jump.
    let editor = Editor::test_instance();
    let p = call(&editor, "create_project", json!({ "name": "slivers", "file_path": "/demo/footage.mp4" })).await;
    let pid = p["id"].as_str().unwrap().to_string();
    call(&editor, "transcribe", json!({ "project_id": pid })).await;
    call(&editor, "generate_rough_cut", json!({ "project_id": pid })).await;
    call(&editor, "apply_cut_proposal", json!({ "project_id": pid })).await;
    let tl = call(&editor, "get_timeline", json!({ "project_id": pid })).await;
    let cuts: Vec<f64> = tl["clips"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["included"] == false)
        .map(|c| c["source_out"].as_f64().unwrap() - c["source_in"].as_f64().unwrap())
        .collect();
    assert!(!cuts.is_empty(), "the demo transcript has things to cut");
    assert!(cuts.iter().all(|&d| d >= roughcut_core::detect::MIN_CUT_S - 1e-6), "a sliver cut: {cuts:?}");
}

#[tokio::test]
async fn a_big_story_edit_is_proposed_and_only_the_confirmed_beats_are_cut() {
    use async_trait::async_trait;
    use roughcut_core::adapters::inference::{ChatMessage, ChatRequest, ChatResponse, InferenceClient};

    /// Plans to cut the first half of the beats (a run, so no seam guard
    /// drops it); anything else isn't JSON, so the outline falls back to
    /// pause-based beats and the flow check runs without a model.
    struct Planner;
    #[async_trait]
    impl InferenceClient for Planner {
        async fn chat(&self, req: ChatRequest) -> roughcut_core::error::Result<ChatResponse> {
            let user = req.messages.iter().rev().find(|m| m.role == "user").and_then(|m| m.content.clone()).unwrap_or_default();
            let text = match user.split_once("Beats:") {
                Some((_, beats)) => {
                    let n = beats.lines().filter(|l| l.split('|').next().is_some_and(|k| k.trim().parse::<usize>().is_ok())).count();
                    let cut: Vec<usize> = (1..=(n / 2).max(2)).filter(|&k| k + 1 < n).collect();
                    json!({ "cut_beats": cut, "rationale": "tangents" }).to_string()
                }
                None => "not json".into(),
            };
            Ok(ChatResponse::new(ChatMessage::assistant(&text)))
        }
        async fn healthy(&self) -> bool {
            true
        }
    }

    let editor = Editor::test_instance();
    editor.set_inference_for_tests(std::sync::Arc::new(Planner), std::sync::Arc::new(hosted_fakes::Claude {
        reject: false,
        fail_from: usize::MAX,
        calls: Default::default(),
    }));
    let p = call(&editor, "create_project", json!({ "name": "story", "file_path": "/demo/footage.mp4" })).await;
    let pid = p["id"].as_str().unwrap().to_string();
    call(&editor, "transcribe", json!({ "project_id": pid })).await;
    let kept = |tl: &Value| -> f64 {
        tl["clips"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["included"] == true)
            .map(|c| c["source_out"].as_f64().unwrap() - c["source_in"].as_f64().unwrap())
            .sum()
    };
    let before = call(&editor, "get_timeline", json!({ "project_id": pid })).await;

    // No length asked for: a big plan comes back as a proposal; nothing changes.
    let out = call(&editor, "story_edit", json!({ "project_id": pid, "instruction": "tighten it" })).await;
    let proposal = &out["proposal"];
    assert!(proposal.is_object(), "expected a proposal: {out}");
    assert!(out["actions"].as_array().unwrap().is_empty(), "nothing cut: {out}");
    assert!(out["summary"].as_str().unwrap().starts_with("Nothing is cut yet"), "{out}");
    let same = call(&editor, "get_timeline", json!({ "project_id": pid })).await;
    assert!((kept(&before) - kept(&same)).abs() < 1e-6, "the timeline is unchanged");
    assert_eq!(before["cut_count"], same["cut_count"]);

    // The user confirms: exactly those beats are cut, with no new plan.
    let beats = proposal["cut_beats"].clone();
    let out = call(&editor, "story_edit", json!({ "project_id": pid, "instruction": "tighten it", "cut_beats": beats })).await;
    assert!(out["proposal"].is_null(), "{out}");
    assert!(!out["actions"].as_array().unwrap().is_empty(), "{out}");
    let after = call(&editor, "get_timeline", json!({ "project_id": pid })).await;
    let cut_s = proposal["cut_s"].as_f64().unwrap();
    let lost = kept(&before) - kept(&after);
    assert!(lost >= cut_s * 0.9, "the confirmed beats were cut: lost {lost:.1}s of {cut_s:.1}s");

    // A named length is the user's yes: no proposal.
    let fresh = call(&editor, "create_project", json!({ "name": "story2", "file_path": "/demo/footage.mp4" })).await;
    let pid2 = fresh["id"].as_str().unwrap().to_string();
    call(&editor, "transcribe", json!({ "project_id": pid2 })).await;
    let out = call(&editor, "story_edit", json!({ "project_id": pid2, "instruction": "tighten it", "target_duration_s": 120 })).await;
    assert!(out["proposal"].is_null(), "{out}");
}

#[tokio::test]
async fn retime_words_needs_the_audio_and_changes_nothing_without_it() {
    let editor = Editor::test_instance();
    let p = call(&editor, "create_project", json!({ "name": "retime", "file_path": "/demo/footage.mp4" })).await;
    let pid = p["id"].as_str().unwrap().to_string();
    call(&editor, "transcribe", json!({ "project_id": pid })).await;
    let before = call(&editor, "get_transcript", json!({ "project_id": pid })).await;
    assert_eq!(before["words_trimmed"], false, "the demo transcript isn't trimmed");
    // The fixture video has no audio to read: a clear error, nothing changed.
    let err = tools::dispatch(&editor, "retime_words", &json!({ "project_id": pid }), Caller::Mcp).await.unwrap_err();
    assert_eq!(err.code(), "unavailable", "{err}");
    let after = call(&editor, "get_transcript", json!({ "project_id": pid })).await;
    assert_eq!(before, after);
    // The in-app agent isn't offered it.
    let agent = Caller::Agent { attribute_to: ActionSource::LocalAi };
    assert!(!tools::defs(agent).iter().any(|d| d.name == "retime_words"));
}

#[tokio::test]
async fn proposed_cuts_change_nothing_until_applied_and_apply_as_one_undo() {
    let editor = Editor::test_instance();
    let p = call(&editor, "create_project", json!({ "name": "review", "file_path": "/demo/footage.mp4" })).await;
    let pid = p["id"].as_str().unwrap().to_string();
    call(&editor, "transcribe", json!({ "project_id": pid })).await;
    let rough = call(&editor, "generate_rough_cut", json!({ "project_id": pid })).await;
    let total = rough["cut_count"].as_u64().unwrap();
    assert!(total >= 3 && rough["pauses"].as_u64().unwrap() >= 1, "{rough}");
    let history = call(&editor, "get_history", json!({ "project_id": pid })).await;
    assert_eq!(history["entries"].as_array().unwrap().len(), 0, "proposing is not an edit");

    // The proposal is on the project, in source order, numbered.
    let proj = call(&editor, "open_project", json!({ "project_id": pid })).await;
    let cuts = proj["cut_proposal"]["cuts"].as_array().unwrap().clone();
    assert_eq!(cuts.len() as u64, total);
    assert!(cuts.windows(2).all(|w| w[0]["start"].as_f64() <= w[1]["start"].as_f64()));

    // Keep the pauses: skipped cuts aren't made.
    let skipped = call(&editor, "skip_proposed_cuts", json!({ "project_id": pid, "kind": "pause" })).await;
    let pauses = rough["pauses"].as_u64().unwrap();
    assert_eq!(skipped["changed"].as_u64().unwrap(), pauses);
    let applied = call(&editor, "apply_cut_proposal", json!({ "project_id": pid })).await;
    assert_eq!(applied["applied"].as_u64().unwrap(), total - pauses);
    assert_eq!(applied["skipped"].as_u64().unwrap(), pauses);
    let tl = call(&editor, "get_timeline", json!({ "project_id": pid })).await;
    for c in cuts.iter().filter(|c| c["kind"] == "pause") {
        let mid = (c["start"].as_f64().unwrap() + c["end"].as_f64().unwrap()) / 2.0;
        let kept = tl["clips"].as_array().unwrap().iter().any(|k| {
            k["included"] == true && k["source_in"].as_f64().unwrap() <= mid && k["source_out"].as_f64().unwrap() >= mid
        });
        assert!(kept, "skipped pause at {mid} was cut");
    }
    // One undo takes the whole proposal back.
    call(&editor, "undo", json!({ "project_id": pid })).await;
    let tl = call(&editor, "get_timeline", json!({ "project_id": pid })).await;
    assert_eq!(tl["cut_count"], 0);

    // Discard drops a proposal without cutting.
    call(&editor, "generate_rough_cut", json!({ "project_id": pid })).await;
    let d = call(&editor, "discard_cut_proposal", json!({ "project_id": pid })).await;
    assert_eq!(d["discarded"], true);
    let waiting = call(&editor, "get_cut_proposal", json!({ "project_id": pid })).await;
    assert!(waiting["proposal"].is_null());
    let tl = call(&editor, "get_timeline", json!({ "project_id": pid })).await;
    assert_eq!(tl["cut_count"], 0);
}

#[tokio::test]
async fn the_chat_proposes_its_cuts_and_a_restore_takes_them_back() {
    let editor = Editor::test_instance();
    let p = call(&editor, "create_project", json!({ "name": "chat-review", "file_path": "/demo/footage.mp4" })).await;
    let pid = p["id"].as_str().unwrap().to_string();
    call(&editor, "transcribe", json!({ "project_id": pid })).await;
    let chat = Caller::Agent { attribute_to: ActionSource::LocalAi };
    let as_chat = |name: &'static str, args: Value| {
        let editor = editor.clone();
        async move { tools::dispatch(&editor, name, &args, chat).await.unwrap_or_else(|e| panic!("{name}: {e}")) }
    };
    let r = as_chat("cut_range", json!({ "project_id": pid, "start": 10.0, "end": 15.0 })).await;
    assert_eq!(r["proposed"], 1, "{r}");
    let r = as_chat("apply_edits", json!({ "project_id": pid, "edits": [
        { "type": "cut_range", "start": 20.0, "end": 22.0 },
        { "type": "cut_range", "start": 12.0, "end": 14.0 }
    ] })).await;
    assert_eq!(r["proposed"], 1, "only what isn't already proposed is added: {r}");
    assert_eq!(r["pending_cuts"], 2);
    let tl = call(&editor, "get_timeline", json!({ "project_id": pid })).await;
    assert_eq!(tl["cut_count"], 0, "the chat proposed, it didn't cut");

    // "Keep that part" drops the proposed cut, and journals nothing.
    let r = as_chat("restore_range", json!({ "project_id": pid, "start": 19.0, "end": 23.0 })).await;
    assert_eq!(r["proposed_cuts_dropped"], 1);
    let history = call(&editor, "get_history", json!({ "project_id": pid })).await;
    assert_eq!(history["entries"].as_array().unwrap().len(), 0);

    // The UI and MCP clients still cut at once.
    let r = call(&editor, "cut_range", json!({ "project_id": pid, "start": 40.0, "end": 41.0 })).await;
    assert!(r["action"].is_object());
    let applied = as_chat("apply_cut_proposal", json!({ "project_id": pid })).await;
    assert_eq!(applied["applied"], 1);
    let history = call(&editor, "get_history", json!({ "project_id": pid })).await;
    let newest = &history["entries"][history["current_index"].as_u64().unwrap() as usize];
    assert_eq!(newest["source"], "local_ai");
    assert!(newest["description"].as_str().unwrap().starts_with("Chat: 1 cut"), "{newest}");
}
