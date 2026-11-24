//! End-to-end model-boundary regression using a disposable database and local mocks.
use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

use crate::{
    ToonState, toonflow_agent_runtime, toonflow_agent_tools, toonflow_agents, toonflow_asset_ai,
};

#[tokio::test]
#[ignore = "run with script/test-toonflow-alignment.sh; isolated database and local mock only"]
async fn alignment_stage_skills_and_image_prompt_reach_model_boundary() {
    use rust_toon_framework_database::{DatabaseConfig, connect, migrate};
    use rust_toon_framework_security::{
        CurrentUser, DataScope, PermissionSet, SecurityConfig, TokenService,
    };
    let pool = connect(
        &DatabaseConfig::new(
            std::env::var("TEST_DATABASE_URL").expect("isolated TEST_DATABASE_URL"),
            1,
            5,
            Duration::from_secs(10),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    migrate(&pool).await.unwrap();

    let captures = Arc::new(tokio::sync::Mutex::new(Vec::<Value>::new()));
    let chat_captures = captures.clone();
    let image_captures = captures.clone();
    let app = Router::new()
        .route(
            "/chat/completions",
            post(move |Json(body): Json<Value>| {
                let captures = chat_captures.clone();
                async move {
                    captures.lock().await.push(body.clone());
                    let messages = body["messages"].as_array().unwrap();
                    if messages[0]["content"].as_str().unwrap_or_default().contains("原著依据与修订规则") {
                        let correction = messages.iter().any(|m| m["role"] == "user" && m["content"].as_str().unwrap_or_default().contains("本轮尚未成功读取任何原文章节"));
                        let read = messages.iter().any(|m| m["role"] == "tool");
                        let message = if correction && !read {
                            json!({"role":"assistant","tool_calls":[
                                {"id":"source","type":"function","function":{"name":"get_novel_text","arguments":"{\"chapterIndex\":1}"}}
                            ]})
                        } else if read {
                            json!({"role":"assistant","content":"<storySkeleton>最终骨架：拿到通行令牌，尚未完成粮道。原著依据：原著第1章。修订记录：删除已取得胜利的夸大描述。</storySkeleton>"})
                        } else {
                            json!({"role":"assistant","content":"<storySkeleton>未核实草稿：粮道已经打通。</storySkeleton>"})
                        };
                        return Json(json!({"choices":[{"message":message}]}));
                    }
                    let message = if messages.iter().any(|m| m["role"] == "tool") {
                        json!({"role":"assistant","content":"已加载当前阶段手册"})
                    } else {
                        json!({"role":"assistant","content":null,"tool_calls":[{
                            "id":"alignment-skill-call","type":"function",
                            "function":{"name":"use_skill","arguments":json!({
                                "path": messages[1]["content"].as_str().unwrap()
                            }).to_string()}
                        }]})
                    };
                    Json(json!({"choices":[{"message":message}]}))
                }
            }),
        )
        .route(
            "/images/generations",
            post(move |Json(body): Json<Value>| {
                let captures = image_captures.clone();
                async move {
                    captures.lock().await.push(body);
                    // Capture the actual provider payload without producing media or
                    // invoking a real service. Also verify tracing survives failures.
                    (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error":{"message":"alignment mock stop"}})),
                    )
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let id = chrono::Utc::now().timestamp_micros();
    sqlx::query("INSERT INTO ai.model_configs(id,name,key,platform,type,model,url,status,create_time,update_time) VALUES($1,'alignment chat',$2,'OpenAICompatible','chat','mock-chat',$3,0,$1,$1)")
        .bind(id).bind(format!("alignment-chat-{id}")).bind(&url).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO ai.model_configs(id,name,key,platform,type,model,url,status,create_time,update_time) VALUES($1,'alignment image',$2,'VolcEngine','image','mock-image',$3,0,$1,$1)")
        .bind(id+1).bind(format!("alignment-image-{id}")).bind(&url).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO toonflow.projects(id,project_type,name,type,intro,art_style,director_manual,chat_model,image_model,create_time,update_time) VALUES($1,'time_travel','alignment','穿越','现代404公寓与古代客栈共存','realpeople_ancient_chinese','Xianxia_fantasy',$1,$2,$1,$1)")
        .bind(id).bind(id+1).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO toonflow.scripts(id,project_id,name,content,create_time) VALUES($1,$1,'第一集','现代404公寓有电脑、冷白灯、冰雹和泡面桶；古代客栈保持木梁与纸窗。',$1)")
        .bind(id).execute(&pool).await.unwrap();
    let tokens = TokenService::new(
        SecurityConfig::new(
            "alignment-test-secret-at-least-32-bytes",
            "test",
            "test",
            Duration::from_secs(60),
        )
        .unwrap(),
    );
    let state = ToonState::new(pool.clone(), tokens);
    let context = toonflow_agents::production_context(&state, id, Some(id))
        .await
        .unwrap();
    for fact in [
        "现代404公寓",
        "电脑",
        "冷白灯",
        "冰雹",
        "泡面桶",
        "古代客栈",
        "穿越剧场世界观约束",
        "Xianxia_fantasy",
        "alignment image",
    ] {
        assert!(context.contains(fact), "missing context: {fact}");
    }
    assert!(
        toonflow_agents::production_context(&state, id, Some(id + 99))
            .await
            .is_err()
    );

    // Exercise the real scoped executor, native tool definitions, tool dispatch,
    // database resolver and model tool response for each affected execution stage.
    for (agent, suffix) in [
        ("directorPlanAgent", "director_planning_style"),
        ("storyboardTableAgent", "director_storyboard_table_style"),
        ("storyboardPanelAgent", "director_storyboard"),
        ("supervisionAgent", "director_storyboard_table_style"),
    ] {
        let key = format!("productionAgent:{agent}");
        let path = format!("manual/visual/realpeople_ancient_chinese/{suffix}");
        let skills = toonflow_agent_runtime::available_skills(&pool, &key, id)
            .await
            .unwrap();
        assert!(skills.iter().any(|entry| entry.0 == path));
        assert!(
            !skills
                .iter()
                .any(|entry| entry.0.contains("realpeople_modern"))
        );
        let output = toonflow_agents::run_scoped_production_agent(
            &state,
            &key,
            &context,
            &path,
            id,
            Some(id),
            &["get_flowData"],
        )
        .await
        .unwrap();
        assert!(output.contains("已加载"));
        let captured = captures.lock().await;
        let request = captured.last().unwrap();
        let tools = request["tools"].as_array().unwrap();
        assert!(tools.iter().any(|t| t["function"]["name"] == "use_skill"));
        assert!(
            !tools
                .iter()
                .any(|t| t["function"]["name"] == "generate_deriveAsset")
        );
        let result: Value = serde_json::from_str(
            request["messages"].as_array().unwrap().last().unwrap()["content"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["path"], path);
        assert_eq!(result["agentKey"], key);
        let actual = toonflow_agent_runtime::load_skill(&pool, &path)
            .await
            .unwrap();
        assert_eq!(result["content"], actual);
        assert_eq!(
            result["sha256"],
            crate::toonflow_prompt_trace::sha256(&actual)
        );
    }
    // API input cannot forge a stage, and the former main-Skill prefix bypass
    // cannot be used to load another stage's main document.
    let forged: toonflow_agent_tools::ToolRequest = serde_json::from_value(json!({
        "agentType":"productionAgent", "agentKey":"productionAgent:directorPlanAgent",
        "projectId":id, "scriptId":id, "toolName":"use_skill",
        "arguments":{"path":"production_execution_director_plan.md"}
    }))
    .unwrap();
    assert!(forged.agent_key.is_none());
    assert!(
        toonflow_agent_tools::execute_inner(&state, &forged)
            .await
            .is_err()
    );

    // Two equivalent clicks resolve to one active generation. Editing the
    // prompt cancels that stale request and detaches an old selected image.
    let derived_id = id + 20;
    let derived = toonflow_asset_ai::ImageItem {
        id: derived_id,
        type_: "role".into(),
        _name: "现代日常造型".into(),
        prompt: "灰色卫衣、黑色长裤、白色运动鞋".into(),
        base64: None,
    };
    sqlx::query("INSERT INTO toonflow.assets(id,project_id,name,type,description,prompt,parent_asset_id) VALUES($1,$2,'现代日常造型','role',$3,$3,$2)")
        .bind(derived_id).bind(id).bind(&derived.prompt).execute(&pool).await.unwrap();
    let first = toonflow_asset_ai::new_image(&pool, id, &derived, &(id + 1).to_string(), "2K")
        .await
        .unwrap();
    let duplicate = toonflow_asset_ai::new_image(&pool, id, &derived, &(id + 1).to_string(), "2K")
        .await
        .unwrap();
    assert_eq!(duplicate.id, first.id);
    assert!(duplicate.reused && !duplicate.created);
    sqlx::query("UPDATE toonflow.assets SET image_id=$2 WHERE id=$1")
        .bind(derived_id)
        .bind(first.id)
        .execute(&pool)
        .await
        .unwrap();
    let edit_user = CurrentUser {
        user_id: "00000000-0000-0000-0000-000000000001".into(),
        username: "alignment-test".into(),
        tenant_id: None,
        role_codes: vec!["super_admin".into()],
        permissions: PermissionSet::default(),
        data_scope: DataScope::All,
    };
    let _ = crate::toonflow::save_asset(
        edit_user.clone(),
        State(state.clone()),
        Json(crate::toonflow::SaveAssetRequest {
            id: Some(derived_id),
            project_id: id,
            name: "现代日常造型".into(),
            prompt: Some("黑色风衣、长裤、皮靴".into()),
            remark: None,
            r#type: Some("role".into()),
            description: Some(derived.prompt.clone()),
            script_id: Some(id),
            parent_asset_id: Some(id),
            image_id: None,
            base64: None,
        }),
    )
    .await
    .unwrap();
    let invalidated: (Option<i64>, String, Option<String>) = sqlx::query_as(
        "SELECT a.image_id,i.state,i.error_reason FROM toonflow.assets a JOIN toonflow.images i ON i.id=$2 WHERE a.id=$1",
    ).bind(derived_id).bind(first.id).fetch_one(&pool).await.unwrap();
    assert_eq!(invalidated.0, None);
    assert_eq!(invalidated.1, "已取消");
    assert_eq!(invalidated.2.as_deref(), Some("资产提示词已修改"));

    // Project fields that participate in the generation fingerprint invalidate
    // selected generated images and cancel work created with the old settings.
    let completed_id = id + 30;
    let active_id = id + 31;
    sqlx::query("INSERT INTO toonflow.images(id,type,assets_id,state,file_path,input_hash) VALUES($1,'role',$3,'已完成','/completed.png','completed-project-input'),($2,'role',$3,'生成中',NULL,'active-project-input')")
        .bind(completed_id).bind(active_id).bind(derived_id).execute(&pool).await.unwrap();
    sqlx::query("UPDATE toonflow.assets SET image_id=$2 WHERE id=$1")
        .bind(derived_id)
        .bind(completed_id)
        .execute(&pool)
        .await
        .unwrap();
    let _ = crate::toonflow_project_crud::update_project(
        edit_user.clone(),
        State(state.clone()),
        Json(crate::toonflow::SaveProjectRequest {
            id: Some(id),
            project_type: "time_travel".into(),
            chat_model: Some(id),
            image_model: Some(id + 1),
            image_quality: String::new(),
            video_model: None,
            name: "alignment".into(),
            intro: "现代404公寓与古代客栈共存".into(),
            r#type: "穿越".into(),
            art_style: "realpeople_modern".into(),
            director_manual: "Xianxia_fantasy".into(),
            mode: String::new(),
            video_ratio: String::new(),
        }),
    )
    .await
    .unwrap();
    let project_invalidated: (Option<i64>, String, Option<String>) = sqlx::query_as(
        "SELECT asset.image_id,image.state,image.error_reason FROM toonflow.assets asset JOIN toonflow.images image ON image.id=$2 WHERE asset.id=$1",
    )
    .bind(derived_id)
    .bind(active_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(project_invalidated.0, None);
    assert_eq!(project_invalidated.1, "已取消");
    assert_eq!(project_invalidated.2.as_deref(), Some("项目生成配置已修改"));

    // Deletion must ignore generated/failed image rows without a file path,
    // while still collecting and cleaning any paths that do exist.
    let _ = crate::toonflow::delete_assets(
        edit_user,
        State(state.clone()),
        Json(crate::toonflow::DeleteIdsRequest {
            ids: Some(vec![derived_id]),
            id: None,
        }),
    )
    .await
    .unwrap();
    let deleted: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM toonflow.assets WHERE id=$1),
                (SELECT count(*) FROM toonflow.images WHERE assets_id=$1)",
    )
    .bind(derived_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(deleted, (0, 0));

    // A saved manual change (water on the floor) must survive the old
    // extraction description's absence of that detail and the ancient style.
    let edited = "现代404公寓，电脑在窗边，冷白灯照明，窗外冰雹，桌上一个泡面桶，地板有积水";
    sqlx::query("INSERT INTO toonflow.assets(id,project_id,name,type,description,prompt) VALUES($1,$1,'404公寓','scene','现代404公寓，电脑与泡面桶',$2)")
        .bind(id).bind(edited).execute(&pool).await.unwrap();
    let user = CurrentUser {
        user_id: "00000000-0000-0000-0000-000000000001".into(),
        username: "alignment-test".into(),
        tenant_id: None,
        role_codes: vec!["super_admin".into()],
        permissions: PermissionSet::default(),
        data_scope: DataScope::All,
    };
    let request = serde_json::from_value(json!({
        "projectId":id,"model":id+1,"resolution":"2K","id":id,
        "type":"scene","name":"404公寓","prompt":edited
    }))
    .unwrap();
    assert!(
        toonflow_asset_ai::generate_image(user.clone(), State(state.clone()), Json(request))
            .await
            .is_err()
    );
    let captured = captures.lock().await.last().unwrap().clone();
    let task: (String, Value) = sqlx::query_as("SELECT state,input FROM toonflow.tasks WHERE project_id=$1 AND task_class='image' ORDER BY id DESC LIMIT 1")
        .bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(task.0, "failed");
    assert_eq!(task.1["prompt"], captured["prompt"]);
    assert_eq!(
        task.1["promptSha256"],
        crate::toonflow_prompt_trace::sha256(captured["prompt"].as_str().unwrap())
    );
    assert_eq!(
        task.1["promptProvenance"]["requestedPrompt"]["content"],
        edited
    );
    assert_eq!(task.1["promptProvenance"]["assetId"], id);
    for fact in ["电脑在窗边", "冷白灯", "冰雹", "一个泡面桶", "地板有积水"] {
        assert!(
            captured["prompt"].as_str().unwrap().contains(fact),
            "lost edit: {fact}"
        );
    }
    let saved: String = sqlx::query_scalar("SELECT prompt FROM toonflow.assets WHERE id=$1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(saved, edited);
    for description in [
        "古代客栈，木梁与纸窗，木桌上一个青瓷茶壶",
        "古代客栈，木梁与纸窗，木桌上的现代电脑和青瓷茶壶同框",
    ] {
        sqlx::query("UPDATE toonflow.assets SET prompt=$2 WHERE id=$1")
            .bind(id)
            .bind(description)
            .execute(&pool)
            .await
            .unwrap();
        let request = serde_json::from_value(json!({
            "projectId":id,"model":id+1,"resolution":"2K","id":id,
            "type":"scene","name":"场景回归","prompt":description
        }))
        .unwrap();
        assert!(
            toonflow_asset_ai::generate_image(user.clone(), State(state.clone()), Json(request))
                .await
                .is_err()
        );
        let captured = captures.lock().await.last().unwrap().clone();
        let prompt = captured["prompt"].as_str().unwrap();
        let visual = prompt
            .split("纯视觉描述：")
            .nth(1)
            .unwrap()
            .lines()
            .next()
            .unwrap();
        assert_eq!(
            visual, description,
            "historical and mixed-era facts must remain intact"
        );
        let input: Value = sqlx::query_scalar("SELECT input FROM toonflow.tasks WHERE project_id=$1 AND task_class='image' ORDER BY id DESC LIMIT 1")
            .bind(id).fetch_one(&pool).await.unwrap();
        assert_eq!(input["prompt"], prompt);
        assert_eq!(
            input["promptProvenance"]["requestedPrompt"]["source"],
            "assets.prompt"
        );
        assert_eq!(
            input["promptProvenance"]["requestedPrompt"]["content"],
            description
        );
    }
    sqlx::query("INSERT INTO toonflow.novels(id,project_id,chapter_index,chapter,chapter_data,create_time) VALUES($1,$1,1,'通行令牌','李晨拿到通行令牌，准备出门探路，尚未打通粮道。',$1)")
        .bind(id).execute(&pool).await.unwrap();
    let skeleton_request: toonflow_agent_tools::ToolRequest = serde_json::from_value(json!({
        "agentType":"scriptAgent", "isolationKey":format!("script-quality-{id}"),
        "projectId":id, "toolName":"run_sub_agent_storySkeleton",
        "arguments":{"prompt":"根据原著第1章生成骨架"}
    }))
    .unwrap();
    let result = toonflow_agent_tools::execute_inner(&state, &skeleton_request)
        .await
        .unwrap();
    assert_eq!(result["evidence"]["sourceChaptersRead"], json!([1]));
    assert_eq!(result["evidence"]["singlePassQualityPolicy"], true);
    let workspace: Value = sqlx::query_scalar("SELECT data FROM toonflow.agent_work_data WHERE project_id=$1 AND key='scriptAgent' AND episodes_id IS NULL")
        .bind(id).fetch_one(&pool).await.unwrap();
    assert!(
        workspace["storySkeleton"]
            .as_str()
            .unwrap()
            .contains("尚未完成粮道")
    );
    assert!(
        !workspace["storySkeleton"]
            .as_str()
            .unwrap()
            .contains("未核实草稿")
    );
    assert_eq!(
        workspace["storySkeletonEvidence"]["sourceChaptersRead"],
        json!([1])
    );
    assert_eq!(
        workspace["storySkeletonEvidence"]["singlePassQualityPolicy"],
        true
    );
    server.abort();
}
