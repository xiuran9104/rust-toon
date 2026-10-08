use rust_toon_framework_database::{DatabaseConfig, connect, migrate};
use sqlx::Row;

async fn scene_state_revisions(pool: &sqlx::PgPool, ids: [i64; 4]) -> [i32; 4] {
    let revisions: (i32, i32, i32, i32) = sqlx::query_as(
        "SELECT
           (SELECT revision FROM toonflow.scene_states WHERE id=$1),
           (SELECT revision FROM toonflow.scene_states WHERE id=$2),
           (SELECT revision FROM toonflow.scene_states WHERE id=$3),
           (SELECT revision FROM toonflow.scene_states WHERE id=$4)",
    )
    .bind(ids[0])
    .bind(ids[1])
    .bind(ids[2])
    .bind(ids[3])
    .fetch_one(pool)
    .await
    .expect("inspect scene state revisions");
    [revisions.0, revisions.1, revisions.2, revisions.3]
}

#[tokio::test]
#[ignore = "run with script/test-database-migrations.sh"]
async fn applies_all_migrations_to_empty_postgres() {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let config = DatabaseConfig::new(url, 1, 5, std::time::Duration::from_secs(10))
        .expect("valid test database config");
    let pool = connect(&config).await.expect("connect test database");
    migrate(&pool).await.expect("apply complete migration set");

    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success")
        .fetch_one(&pool)
        .await
        .expect("read migration history");
    assert_eq!(applied, 26);

    sqlx::raw_sql(include_str!(
        "../../../../sql/postgresql/0022_video_export_timeline.sql"
    ))
    .execute(&pool)
    .await
    .expect("video export timeline migration is idempotent");
    let timeline_columns: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT column_name,column_default::text
         FROM information_schema.columns
         WHERE table_schema='toonflow' AND table_name='video_tracks'
           AND column_name IN ('trim_start_ms','trim_end_ms','transition_duration_ms')
         ORDER BY column_name",
    )
    .fetch_all(&pool)
    .await
    .expect("inspect video export timeline columns");
    assert_eq!(
        timeline_columns,
        vec![
            (
                "transition_duration_ms".to_string(),
                Some("600".to_string())
            ),
            ("trim_end_ms".to_string(), None),
            ("trim_start_ms".to_string(), Some("0".to_string())),
        ]
    );
    for constraint in [
        "video_tracks_trim_range_check",
        "video_tracks_transition_duration_check",
    ] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pg_constraint WHERE conname=$1)",
        )
        .bind(constraint)
        .fetch_one(&pool)
        .await
        .expect("inspect video export timeline constraint");
        assert!(exists, "missing constraint {constraint}");
    }

    let before: (String, String) = sqlx::query_as("SELECT data::text, images::text FROM toonflow.creative_manuals WHERE path='realpeople_cinematic_base' AND kind='visual'")
        .fetch_one(&pool).await.expect("read refined manual");
    sqlx::raw_sql(include_str!("../../../../sql/postgresql/0021_refine_live_action_manual_references.sql"))
        .execute(&pool).await.expect("refined manual migration is idempotent");
    let after: (String, String) = sqlx::query_as("SELECT data::text, images::text FROM toonflow.creative_manuals WHERE path='realpeople_cinematic_base' AND kind='visual'")
        .fetch_one(&pool).await.expect("read repeated refined manual");
    assert_eq!(before, after);
    assert_eq!(after.0.matches("## 执行补充 v2").count(), 12);
    assert!(after.1.contains("reference-v1.png"));

    sqlx::raw_sql(include_str!(
        "../../../../sql/postgresql/0020_merge_builtin_style_library_into_visual_manuals.sql"
    ))
    .execute(&pool)
    .await
    .expect("visual manual style-library merge migration is idempotent");

    let manual_menu: (String, String) = sqlx::query_as(
        "SELECT name, component FROM public.system_menu
         WHERE id=20002 AND path='styles'",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect merged creative manual menu");
    assert_eq!(manual_menu, ("创作手册".into(), "toonflow/manuals/index".into()));

    let duplicated_builtin_styles: i64 = sqlx::query_scalar(
        "SELECT count(*)
         FROM toonflow.art_styles AS style
         JOIN toonflow.creative_manuals AS manual
           ON manual.kind='visual'
          AND manual.name=style.name
          AND manual.path=style.label
         WHERE style.id BETWEEN 560100 AND 560110",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect retired built-in art-style copies");
    assert_eq!(duplicated_builtin_styles, 0);

    sqlx::raw_sql(include_str!(
        "../../../../sql/postgresql/0019_generic_live_action_visual_base.sql"
    ))
    .execute(&pool)
    .await
    .expect("generic live-action visual manual migration is idempotent");

    let generic_live_action_manual: (String, i64, String) = sqlx::query_as(
        "SELECT name, jsonb_array_length(data)::bigint, data::text
         FROM toonflow.creative_manuals
         WHERE kind='visual' AND path='realpeople_cinematic_base'",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect generic live-action visual manual");
    assert_eq!(generic_live_action_manual.0, "通用真人影视基底");
    assert_eq!(generic_live_action_manual.1, 12);
    assert!(generic_live_action_manual.2.contains("基础画风控制“怎么拍”"));
    assert!(generic_live_action_manual.2.contains("不得自动现代化"));
    assert!(generic_live_action_manual.2.contains("不得自动古风化"));
    assert!(!generic_live_action_manual.2.contains("禁止古风"));
    assert!(!generic_live_action_manual.2.contains("禁止现代"));

    sqlx::raw_sql(include_str!(
        "../../../../sql/postgresql/0018_asset_image_generation_idempotency.sql"
    ))
    .execute(&pool)
    .await
    .expect("asset image idempotency migration is idempotent");

    let input_hash_column: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.columns
         WHERE table_schema='toonflow' AND table_name='images' AND column_name='input_hash')",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect image input hash column");
    assert!(input_hash_column);

    sqlx::raw_sql(include_str!(
        "../../../../sql/postgresql/0012_retire_duplicate_request_traces.sql"
    ))
    .execute(&pool)
    .await
    .expect("request trace retirement migration is idempotent");

    let trace_menu: (i16, bool) =
        sqlx::query_as("SELECT deleted, visible FROM public.system_menu WHERE id=1077")
            .fetch_one(&pool)
            .await
            .expect("inspect retired request trace menu");
    assert_eq!(trace_menu, (1, false));

    let api_log_menu: (i16, bool) =
        sqlx::query_as("SELECT deleted, visible FROM public.system_menu WHERE id=1078")
            .fetch_one(&pool)
            .await
            .expect("inspect API access log menu");
    assert_eq!(api_log_menu, (0, true));

    sqlx::raw_sql(include_str!(
        "../../../../sql/postgresql/0010_volcengine_platform_label.sql"
    ))
    .execute(&pool)
    .await
    .expect("Volcengine platform label migration is idempotent");

    sqlx::raw_sql(include_str!(
        "../../../../sql/postgresql/0015_rename_doubao_platform_to_volcengine.sql"
    ))
    .execute(&pool)
    .await
    .expect("VolcEngine platform identifier migration is idempotent");

    let platform_label: String =
        sqlx::query_scalar("SELECT label FROM ai.model_platforms WHERE platform='VolcEngine'")
            .fetch_one(&pool)
            .await
            .expect("inspect canonical VolcEngine platform");
    assert_eq!(platform_label, "火山引擎");

    let dictionary_labels: Vec<String> = sqlx::query_scalar(
        "SELECT label FROM public.system_dict_data
         WHERE dict_type='ai_platform' AND value='VolcEngine'",
    )
    .fetch_all(&pool)
    .await
    .expect("inspect canonical VolcEngine dictionary labels");
    assert!(!dictionary_labels.is_empty());
    assert!(dictionary_labels.iter().all(|label| label == "火山引擎"));

    let legacy_platform_rows: i64 = sqlx::query_scalar(
        "SELECT
           (SELECT count(*) FROM ai.model_platforms WHERE platform='DouBao') +
           (SELECT count(*) FROM ai.model_catalog WHERE platform='DouBao') +
           (SELECT count(*) FROM ai.model_configs WHERE platform='DouBao') +
           (SELECT count(*) FROM public.system_dict_data
              WHERE dict_type='ai_platform' AND value='DouBao')",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect retired DouBao platform identifier");
    assert_eq!(legacy_platform_rows, 0);

    sqlx::raw_sql(include_str!(
        "../../../../sql/postgresql/0016_align_event_extraction_prompt.sql"
    ))
    .execute(&pool)
    .await
    .expect("event extraction prompt alignment migration is idempotent");

    let event_prompt: String =
        sqlx::query_scalar("SELECT data FROM toonflow.prompts WHERE source_key='eventExtraction'")
            .fetch_one(&pool)
            .await
            .expect("inspect event extraction prompt");
    assert!(event_prompt.contains("只输出纯 JSON 数组"));
    assert!(!event_prompt.contains("恰好 7 个字段"));
    sqlx::raw_sql(include_str!(
        "../../../../sql/postgresql/0017_single_core_event_per_chapter.sql"
    ))
    .execute(&pool)
    .await
    .expect("single core event migration is idempotent");
    assert!(event_prompt.contains("每章只概括一条核心事件"));
    assert!(!event_prompt.contains("每3000字提取3-5个事件"));

    sqlx::raw_sql(include_str!(
        "../../../../sql/postgresql/0002_episode_renders.sql"
    ))
    .execute(&pool)
    .await
    .expect("episode render migration is idempotent");

    sqlx::raw_sql(include_str!(
        "../../../../sql/postgresql/0003_distributed_jobs.sql"
    ))
    .execute(&pool)
    .await
    .expect("distributed job migration is idempotent");

    sqlx::raw_sql(include_str!(
        "../../../../sql/postgresql/0004_distributed_job_delivery_guards.sql"
    ))
    .execute(&pool)
    .await
    .expect("distributed job delivery guard migration is idempotent");

    sqlx::raw_sql(include_str!(
        "../../../../sql/postgresql/0005_video_id_sequence.sql"
    ))
    .execute(&pool)
    .await
    .expect("video id sequence migration is idempotent");

    sqlx::raw_sql(include_str!(
        "../../../../sql/postgresql/0006_login_lockout.sql"
    ))
    .execute(&pool)
    .await
    .expect("login lockout migration is idempotent");

    sqlx::raw_sql(include_str!(
        "../../../../sql/postgresql/0007_distributed_scheduler_and_trace_context.sql"
    ))
    .execute(&pool)
    .await
    .expect("distributed scheduler migration is idempotent");

    sqlx::raw_sql(include_str!(
        "../../../../sql/postgresql/0008_structured_video_transitions.sql"
    ))
    .execute(&pool)
    .await
    .expect("structured video transition migration is idempotent");

    sqlx::raw_sql(include_str!(
        "../../../../sql/postgresql/0009_scene_consistency.sql"
    ))
    .execute(&pool)
    .await
    .expect("scene consistency migration is idempotent");

    let storyboard_panel_skill: String = sqlx::query_scalar(
        "SELECT content FROM toonflow.skill_list
         WHERE path='production_execution_storyboard_panel.md'",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect storyboard panel scene key instructions");
    assert_eq!(storyboard_panel_skill.matches("| `sceneKey` |").count(), 1);
    assert!(
        storyboard_panel_skill.contains("add_flowData_storyboard({ sceneKey: \"scN\", videoDesc:")
    );

    let storyboard_scene_key: Option<(String, String)> = sqlx::query_as(
        "SELECT udt_name,is_nullable
         FROM information_schema.columns
         WHERE table_schema='toonflow' AND table_name='storyboards'
           AND column_name='scene_key'",
    )
    .fetch_optional(&pool)
    .await
    .expect("inspect storyboard scene key");
    assert_eq!(storyboard_scene_key, Some(("text".into(), "YES".into())));

    let track_transition_columns: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT column_name,udt_name,is_nullable,column_default
             FROM information_schema.columns
             WHERE table_schema='toonflow' AND table_name='video_tracks'
               AND column_name IN (
                 'transition_type','frame_policy','previous_track_id','transition_source'
               )
             ORDER BY column_name",
    )
    .fetch_all(&pool)
    .await
    .expect("inspect structured video track transition columns");
    assert_eq!(
        track_transition_columns,
        vec![
            (
                "frame_policy".into(),
                "text".into(),
                "NO".into(),
                Some("'own'::text".into()),
            ),
            (
                "previous_track_id".into(),
                "int8".into(),
                "YES".into(),
                None,
            ),
            (
                "transition_source".into(),
                "text".into(),
                "NO".into(),
                Some("'director'::text".into()),
            ),
            (
                "transition_type".into(),
                "text".into(),
                "NO".into(),
                Some("'cut'::text".into()),
            ),
        ]
    );

    let generation_context: Option<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT udt_name,is_nullable,column_default
         FROM information_schema.columns
         WHERE table_schema='toonflow' AND table_name='videos'
           AND column_name='generation_context'",
    )
    .fetch_optional(&pool)
    .await
    .expect("inspect video generation context");
    assert_eq!(
        generation_context,
        Some(("jsonb".into(), "NO".into(), Some("'{}'::jsonb".into())))
    );

    let scene_transition_columns: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT column_name,udt_name,is_nullable
         FROM information_schema.columns
         WHERE table_schema='toonflow' AND table_name='scene_transitions'
         ORDER BY ordinal_position",
    )
    .fetch_all(&pool)
    .await
    .expect("inspect scene transition columns");
    assert_eq!(
        scene_transition_columns,
        vec![
            ("project_id".into(), "int8".into(), "NO".into()),
            ("script_id".into(), "int8".into(), "NO".into()),
            ("from_scene_key".into(), "text".into(), "NO".into()),
            ("to_scene_key".into(), "text".into(), "NO".into()),
            ("transition_type".into(), "text".into(), "NO".into()),
            ("description".into(), "text".into(), "NO".into()),
            ("frame_policy".into(), "text".into(), "NO".into()),
            ("update_time".into(), "int8".into(), "NO".into()),
        ]
    );

    let scene_transition_primary_key: Option<String> = sqlx::query_scalar(
        "SELECT pg_get_constraintdef(oid)
         FROM pg_constraint
         WHERE conrelid='toonflow.scene_transitions'::regclass
           AND conname='scene_transitions_pkey'",
    )
    .fetch_optional(&pool)
    .await
    .expect("inspect scene transition primary key");
    assert_eq!(
        scene_transition_primary_key.as_deref(),
        Some("PRIMARY KEY (project_id, script_id, from_scene_key, to_scene_key)")
    );

    for constraint in [
        "storyboards_scene_key_canonical",
        "video_tracks_transition_type_valid",
        "video_tracks_frame_policy_valid",
        "video_tracks_transition_source_valid",
        "video_tracks_previous_track_fk",
        "video_tracks_previous_track_not_self",
        "videos_generation_context_is_object",
        "scene_transitions_script_project_fk",
        "scene_transitions_scene_keys_not_blank",
        "scene_transitions_scene_keys_canonical",
        "scene_transitions_transition_type_valid",
        "scene_transitions_frame_policy_valid",
    ] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM pg_constraint
               WHERE conname=$1
                 AND connamespace='toonflow'::regnamespace
             )",
        )
        .bind(constraint)
        .fetch_one(&pool)
        .await
        .expect("inspect structured video transition constraint");
        assert!(
            exists,
            "expected structured transition constraint {constraint}"
        );
    }

    let previous_track_delete_action: Option<String> = sqlx::query_scalar(
        "SELECT confdeltype::text
         FROM pg_constraint
         WHERE conrelid='toonflow.video_tracks'::regclass
           AND conname='video_tracks_previous_track_fk'",
    )
    .fetch_optional(&pool)
    .await
    .expect("inspect previous track delete behavior");
    assert_eq!(previous_track_delete_action.as_deref(), Some("n"));

    for index in [
        "toonflow.idx_toonflow_storyboards_scene_order",
        "toonflow.idx_toonflow_video_tracks_previous_track",
        "toonflow.idx_toonflow_scene_transitions_target",
    ] {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(index)
            .fetch_one(&pool)
            .await
            .expect("inspect structured transition index");
        assert!(exists, "expected structured transition index {index}");
    }

    sqlx::query(
        "INSERT INTO toonflow.projects(id,name,create_time,update_time)
         VALUES(-8008001,'structured-transition-migration-test',0,0)",
    )
    .execute(&pool)
    .await
    .expect("create transition migration project fixture");
    sqlx::query(
        "INSERT INTO toonflow.scripts(id,name,project_id,create_time)
         VALUES(-8008002,'structured-transition-migration-test',-8008001,0)",
    )
    .execute(&pool)
    .await
    .expect("create transition migration script fixture");
    sqlx::query(
        "INSERT INTO toonflow.video_tracks(
           id,project_id,script_id,sort_order,continuity_mode
         )
         VALUES
           (-8008011,-8008001,-8008002,0,'always'),
           (-8008012,-8008001,-8008002,0,'never'),
           (-8008013,-8008001,-8008002,0,'auto'),
           (-8008014,-8008001,-8008002,5,'auto')",
    )
    .execute(&pool)
    .await
    .expect("create transition migration track fixtures");
    sqlx::query(
        "INSERT INTO toonflow.storyboards(
           id,script_id,track_id,project_id,index,create_time
         ) VALUES
           (-8008021,-8008002,-8008011,-8008001,20,0),
           (-8008022,-8008002,-8008012,-8008001,10,0),
           (-8008023,-8008002,-8008013,-8008001,10,0)",
    )
    .execute(&pool)
    .await
    .expect("create transition migration storyboard fixtures");
    sqlx::query(
        "ALTER TABLE toonflow.video_tracks
         DROP CONSTRAINT video_tracks_frame_policy_valid",
    )
    .execute(&pool)
    .await
    .expect("simulate the first structured frame-policy installation");
    sqlx::raw_sql(include_str!(
        "../../../../sql/postgresql/0008_structured_video_transitions.sql"
    ))
    .execute(&pool)
    .await
    .expect("structured transition migration backfill reruns safely");
    let normalized_track_order: Vec<(i64, i32)> = sqlx::query_as(
        "SELECT id,sort_order FROM toonflow.video_tracks
         WHERE project_id=-8008001 AND script_id=-8008002
         ORDER BY sort_order,id",
    )
    .fetch_all(&pool)
    .await
    .expect("inspect normalized transition track order");
    assert_eq!(
        normalized_track_order,
        vec![(-8008013, 0), (-8008012, 1), (-8008011, 2), (-8008014, 3),]
    );
    let legacy_transition_settings: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT id,frame_policy,transition_source
         FROM toonflow.video_tracks
         WHERE project_id=-8008001 AND script_id=-8008002
         ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .expect("inspect migrated legacy continuity settings");
    assert_eq!(
        legacy_transition_settings,
        vec![
            (-8008014, "own".into(), "director".into()),
            (-8008013, "own".into(), "director".into()),
            (-8008012, "own".into(), "manual".into()),
            (-8008011, "previous_tail".into(), "manual".into()),
        ]
    );
    sqlx::query(
        "UPDATE toonflow.video_tracks
         SET sort_order=CASE id
           WHEN -8008011 THEN 0
           WHEN -8008013 THEN 1
           WHEN -8008012 THEN 2
           WHEN -8008014 THEN 3
         END
         WHERE project_id=-8008001 AND script_id=-8008002",
    )
    .execute(&pool)
    .await
    .expect("manually reorder transition migration track fixtures");
    sqlx::query(
        "UPDATE toonflow.video_tracks
         SET continuity_mode='always',frame_policy='own',transition_source='manual'
         WHERE id=-8008012",
    )
    .execute(&pool)
    .await
    .expect("mark a legacy continuity fixture as manually configured");
    sqlx::raw_sql(include_str!(
        "../../../../sql/postgresql/0008_structured_video_transitions.sql"
    ))
    .execute(&pool)
    .await
    .expect("structured transition migration preserves unique manual order");
    let preserved_manual_order: Vec<(i64, i32)> = sqlx::query_as(
        "SELECT id,sort_order FROM toonflow.video_tracks
         WHERE project_id=-8008001 AND script_id=-8008002
         ORDER BY sort_order,id",
    )
    .fetch_all(&pool)
    .await
    .expect("inspect preserved manual transition track order");
    assert_eq!(
        preserved_manual_order,
        vec![(-8008011, 0), (-8008013, 1), (-8008012, 2), (-8008014, 3),]
    );
    let preserved_manual_frame_policy: String =
        sqlx::query_scalar("SELECT frame_policy FROM toonflow.video_tracks WHERE id=-8008012")
            .fetch_one(&pool)
            .await
            .expect("inspect preserved manual transition policy");
    assert_eq!(preserved_manual_frame_policy, "own");
    sqlx::query("DELETE FROM toonflow.projects WHERE id=-8008001")
        .execute(&pool)
        .await
        .expect("remove structured transition migration fixtures");

    let scene_consistency_tables: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables
         WHERE table_schema='toonflow'
           AND table_name IN ('scene_masters','scene_states','scene_state_references')",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect scene consistency tables");
    assert_eq!(scene_consistency_tables, 3);

    let scene_master_columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name FROM information_schema.columns
         WHERE table_schema='toonflow' AND table_name='scene_masters'
         ORDER BY ordinal_position",
    )
    .fetch_all(&pool)
    .await
    .expect("inspect scene master columns");
    assert_eq!(
        scene_master_columns,
        vec![
            "id",
            "project_id",
            "script_id",
            "scene_key",
            "name",
            "scene_asset_id",
            "pinned_image_id",
            "spatial_prompt",
            "layout_spec",
            "status",
            "source",
            "revision",
            "create_time",
            "update_time",
        ]
    );
    let scene_state_columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name FROM information_schema.columns
         WHERE table_schema='toonflow' AND table_name='scene_states'
         ORDER BY ordinal_position",
    )
    .fetch_all(&pool)
    .await
    .expect("inspect scene state columns");
    assert_eq!(
        scene_state_columns,
        vec![
            "id",
            "scene_master_id",
            "state_key",
            "name",
            "parent_state_id",
            "sequence",
            "change_summary",
            "state_prompt",
            "object_states",
            "source",
            "revision",
            "create_time",
            "update_time",
        ]
    );
    let scene_reference_columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name FROM information_schema.columns
         WHERE table_schema='toonflow' AND table_name='scene_state_references'
         ORDER BY ordinal_position",
    )
    .fetch_all(&pool)
    .await
    .expect("inspect scene state reference columns");
    assert_eq!(
        scene_reference_columns,
        vec![
            "scene_state_id",
            "sort_order",
            "role",
            "asset_id",
            "image_id",
            "prompt_label",
        ]
    );

    let storyboard_scene_state_columns: Vec<(String, String, String, Option<String>)> =
        sqlx::query_as(
            "SELECT column_name,udt_name,is_nullable,column_default
             FROM information_schema.columns
             WHERE table_schema='toonflow' AND table_name='storyboards'
               AND column_name IN (
                 'scene_state_id','generated_scene_state_id','scene_generation_context'
               )
             ORDER BY column_name",
        )
        .fetch_all(&pool)
        .await
        .expect("inspect storyboard scene state columns");
    assert_eq!(
        storyboard_scene_state_columns,
        vec![
            (
                "generated_scene_state_id".into(),
                "int8".into(),
                "YES".into(),
                None,
            ),
            (
                "scene_generation_context".into(),
                "jsonb".into(),
                "NO".into(),
                Some("'{}'::jsonb".into()),
            ),
            ("scene_state_id".into(), "int8".into(), "YES".into(), None,),
        ]
    );

    for constraint in [
        "scene_masters_scope_unique",
        "scene_masters_script_project_fk",
        "scene_masters_pinned_image_asset_fk",
        "scene_masters_scene_key_canonical",
        "scene_masters_layout_spec_is_object",
        "scene_masters_status_valid",
        "scene_masters_reference_pair_complete",
        "scene_states_master_sequence_unique",
        "scene_states_master_key_unique",
        "scene_states_parent_same_master_fk",
        "scene_states_base_parent_contract",
        "scene_states_parent_not_self",
        "scene_states_object_states_is_object",
        "scene_state_references_pkey",
        "scene_state_references_state_image_unique",
        "scene_state_references_image_asset_fk",
        "storyboards_scene_state_fk",
        "storyboards_generated_scene_state_fk",
        "storyboards_scene_generation_context_is_object",
    ] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM pg_constraint
               WHERE connamespace='toonflow'::regnamespace AND conname=$1
             )",
        )
        .bind(constraint)
        .fetch_one(&pool)
        .await
        .expect("inspect scene consistency constraint");
        assert!(exists, "expected scene consistency constraint {constraint}");
    }

    for trigger in [
        "scene_masters_enforce_integrity",
        "scene_states_enforce_integrity",
        "scene_state_references_enforce_integrity",
        "assets_enforce_scene_reverse_scope",
        "images_enforce_scene_reverse_binding",
        "scene_state_references_bump_revision_tree",
        "images_bump_scene_revisions",
        "assets_auto_pin_scene_master_image",
        "storyboards_enforce_scene_state_scope",
        "storyboards_enforce_scene_state_timeline",
        "scene_states_enforce_storyboard_timelines",
    ] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM pg_trigger
               WHERE tgname=$1 AND NOT tgisinternal
             )",
        )
        .bind(trigger)
        .fetch_one(&pool)
        .await
        .expect("inspect scene consistency trigger");
        assert!(exists, "expected scene consistency trigger {trigger}");
    }

    for path in [
        "production_execution_director_plan.md",
        "production_execution_storyboard_table.md",
        "production_execution_storyboard_panel.md",
    ] {
        let content: String =
            sqlx::query_scalar("SELECT content FROM toonflow.skill_list WHERE path=$1")
                .bind(path)
                .fetch_one(&pool)
                .await
                .expect("inspect scene consistency skill contract");
        assert_eq!(content.matches("<!-- scene-consistency-v1 -->").count(), 1);
        assert!(content.contains("sceneStateKey"));
        assert!(content.contains("sceneStateParentKey"));
        assert!(content.contains("sceneStateDescription"));
    }

    sqlx::query(
        "INSERT INTO toonflow.projects(id,name,create_time,update_time)
         VALUES(-9009001,'scene-consistency-migration-test',0,0)",
    )
    .execute(&pool)
    .await
    .expect("create scene consistency project fixture");
    sqlx::query(
        "INSERT INTO toonflow.scripts(id,name,project_id,create_time)
         VALUES(-9009002,'scene-consistency-migration-test',-9009001,0)",
    )
    .execute(&pool)
    .await
    .expect("create scene consistency script fixture");
    sqlx::query(
        "INSERT INTO toonflow.assets(id,name,prompt,type,description,project_id)
         VALUES
           (-9009011,'唯一客厅','固定沙发与北墙木门','scene','客厅母版',-9009001),
           (-9009012,'候选仓库甲','','scene','',-9009001),
           (-9009013,'候选仓库乙','','scene','',-9009001),
           (-9009014,'未完成走廊','','scene','',-9009001)",
    )
    .execute(&pool)
    .await
    .expect("create scene consistency asset fixtures");
    sqlx::query(
        "INSERT INTO toonflow.images(id,file_path,type,assets_id,state)
         VALUES
           (-9009021,'/scene/master.png','scene',-9009011,'已完成'),
           (-9009024,'','scene',-9009014,'处理中')",
    )
    .execute(&pool)
    .await
    .expect("create scene consistency image fixture");
    sqlx::query(
        "UPDATE toonflow.assets
         SET image_id = CASE id
             WHEN -9009011 THEN -9009021
             WHEN -9009014 THEN -9009024
         END
         WHERE id IN (-9009011,-9009014)",
    )
    .execute(&pool)
    .await
    .expect("select scene consistency asset image");
    sqlx::query(
        "INSERT INTO toonflow.storyboards(
           id,script_id,project_id,scene_key,index,create_time
         ) VALUES
           (-9009031,-9009002,-9009001,'sc1',0,0),
           (-9009032,-9009002,-9009001,'sc1',1,0),
           (-9009033,-9009002,-9009001,'sc2',2,0),
           (-9009034,-9009002,-9009001,'sc2',3,0),
           (-9009035,-9009002,-9009001,'sc3',4,0),
           (-9009036,-9009002,-9009001,'sc3',5,0),
           (-9009037,-9009002,-9009001,'sc4',6,0),
           (-9009038,-9009002,-9009001,'sc4',7,0)",
    )
    .execute(&pool)
    .await
    .expect("create scene consistency storyboard fixtures");
    sqlx::query(
        "INSERT INTO toonflow.assets_storyboards(storyboard_id,asset_id,sort_order)
         VALUES
           (-9009031,-9009011,0),
           (-9009032,-9009011,0),
           (-9009033,-9009012,0),
           (-9009034,-9009013,0),
           (-9009035,-9009012,0),
           (-9009037,-9009014,0),
           (-9009038,-9009014,0)",
    )
    .execute(&pool)
    .await
    .expect("bind scene consistency asset fixtures");
    sqlx::raw_sql(include_str!(
        "../../../../sql/postgresql/0009_scene_consistency.sql"
    ))
    .execute(&pool)
    .await
    .expect("scene consistency migration backfills legacy storyboards");

    let migrated_masters: Vec<(String, Option<i64>, Option<i64>, String)> = sqlx::query_as(
        "SELECT scene_key,scene_asset_id,pinned_image_id,status
         FROM toonflow.scene_masters
         WHERE project_id=-9009001 AND script_id=-9009002
         ORDER BY scene_key",
    )
    .fetch_all(&pool)
    .await
    .expect("inspect backfilled scene masters");
    assert_eq!(
        migrated_masters,
        vec![
            ("sc1".into(), Some(-9009011), Some(-9009021), "ready".into()),
            ("sc2".into(), None, None, "needs_review".into()),
            ("sc3".into(), None, None, "needs_review".into()),
            (
                "sc4".into(),
                Some(-9009014),
                None,
                "missing_reference".into()
            ),
        ]
    );
    let migrated_states: Vec<(String, String, i32, String)> = sqlx::query_as(
        "SELECT master.scene_key,state.state_key,state.sequence,state.object_states::text
         FROM toonflow.scene_states state
         JOIN toonflow.scene_masters master ON master.id=state.scene_master_id
         WHERE master.project_id=-9009001 AND master.script_id=-9009002
         ORDER BY master.scene_key,state.sequence",
    )
    .fetch_all(&pool)
    .await
    .expect("inspect backfilled base scene states");
    assert_eq!(
        migrated_states,
        vec![
            ("sc1".into(), "base".into(), 0, "{}".into()),
            ("sc2".into(), "base".into(), 0, "{}".into()),
            ("sc3".into(), "base".into(), 0, "{}".into()),
            ("sc4".into(), "base".into(), 0, "{}".into()),
        ]
    );
    let migrated_reference_count: i64 = sqlx::query_scalar(
        "SELECT count(*)
         FROM toonflow.scene_state_references reference
         JOIN toonflow.scene_states state ON state.id=reference.scene_state_id
         JOIN toonflow.scene_masters master ON master.id=state.scene_master_id
         WHERE master.project_id=-9009001",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect backfilled state references");
    assert_eq!(migrated_reference_count, 0);
    let storyboard_state_bindings: Vec<(i64, bool, bool, String)> = sqlx::query_as(
        "SELECT id,scene_state_id IS NOT NULL,generated_scene_state_id IS NULL,
                scene_generation_context::text
         FROM toonflow.storyboards
         WHERE project_id=-9009001
         ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .expect("inspect backfilled storyboard state bindings");
    assert_eq!(storyboard_state_bindings.len(), 8);
    assert!(storyboard_state_bindings.iter().all(|row| row.1 && row.2));
    assert!(storyboard_state_bindings.iter().all(|row| row.3 == "{}"));

    let sc1_state_id: i64 = sqlx::query_scalar(
        "SELECT state.id FROM toonflow.scene_states state
         JOIN toonflow.scene_masters master ON master.id=state.scene_master_id
         WHERE master.project_id=-9009001 AND master.scene_key='sc1'
           AND state.state_key='base'",
    )
    .fetch_one(&pool)
    .await
    .expect("load scoped scene state fixture");
    assert!(
        sqlx::query("UPDATE toonflow.storyboards SET scene_state_id=$1 WHERE id=-9009033")
            .bind(sc1_state_id)
            .execute(&pool)
            .await
            .is_err(),
        "a storyboard must not bind a state from another scene"
    );

    sqlx::query(
        "UPDATE toonflow.scene_masters
         SET name='人工确认母版',status='needs_review',source='manual'
         WHERE project_id=-9009001 AND scene_key='sc1'",
    )
    .execute(&pool)
    .await
    .expect("manually revise a backfilled scene master");
    sqlx::raw_sql(include_str!(
        "../../../../sql/postgresql/0009_scene_consistency.sql"
    ))
    .execute(&pool)
    .await
    .expect("scene consistency migration preserves manual review");
    let preserved_master: (String, String, String) = sqlx::query_as(
        "SELECT name,status,source FROM toonflow.scene_masters
         WHERE project_id=-9009001 AND scene_key='sc1'",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect preserved scene master review");
    assert_eq!(
        preserved_master,
        (
            "人工确认母版".into(),
            "needs_review".into(),
            "manual".into()
        )
    );
    let scene_consistency_counts: (i64, i64, i64) = sqlx::query_as(
        "SELECT
           (SELECT count(*) FROM toonflow.scene_masters WHERE project_id=-9009001),
           (SELECT count(*) FROM toonflow.scene_states state JOIN toonflow.scene_masters master ON master.id=state.scene_master_id WHERE master.project_id=-9009001),
           (SELECT count(*) FROM toonflow.scene_state_references reference JOIN toonflow.scene_states state ON state.id=reference.scene_state_id JOIN toonflow.scene_masters master ON master.id=state.scene_master_id WHERE master.project_id=-9009001)",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect idempotent scene consistency counts");
    assert_eq!(scene_consistency_counts, (4, 4, 0));

    sqlx::query(
        "INSERT INTO toonflow.projects(id,name,create_time,update_time)
         VALUES(-9009101,'scene-consistency-other-project',0,0)",
    )
    .execute(&pool)
    .await
    .expect("create cross-project scene consistency fixture");
    sqlx::query(
        "INSERT INTO toonflow.scripts(id,name,project_id,create_time)
         VALUES(-9009102,'scene-consistency-other-script',-9009101,0)",
    )
    .execute(&pool)
    .await
    .expect("create cross-project script fixture");
    sqlx::query(
        "INSERT INTO toonflow.assets(id,name,type,project_id)
         VALUES
           (-9009015,'场内状态物件甲','prop',-9009001),
           (-9009016,'场内状态物件乙','prop',-9009001),
           (-9009111,'其他项目场景','scene',-9009101),
           (-9009112,'其他项目物件','prop',-9009101)",
    )
    .execute(&pool)
    .await
    .expect("create scene integrity asset fixtures");
    sqlx::query(
        "INSERT INTO toonflow.images(id,file_path,type,assets_id,state)
         VALUES
           (-9009022,'/scene/master-alt.png','scene',-9009011,'已完成'),
           (-9009025,'/scene/state-object.png','prop',-9009015,'已完成'),
           (-9009026,'/scene/state-object-alt.png','prop',-9009016,'已完成'),
           (-9009027,'/scene/new-master.png','scene',-9009012,'已完成'),
           (-9009121,'/scene/foreign-master.png','scene',-9009111,'已完成'),
           (-9009122,'/scene/foreign-object.png','prop',-9009112,'已完成')",
    )
    .execute(&pool)
    .await
    .expect("create scene integrity image fixtures");

    let asset_only_master_id: i64 = sqlx::query_scalar(
        "INSERT INTO toonflow.scene_masters(
           project_id,script_id,scene_key,scene_asset_id,status
         ) VALUES(-9009001,-9009002,'sc9',-9009012,'missing_reference')
         RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .expect("allow a scene master asset before its first completed image");
    let asset_only_pin: Option<i64> =
        sqlx::query_scalar("SELECT pinned_image_id FROM toonflow.scene_masters WHERE id=$1")
            .bind(asset_only_master_id)
            .fetch_one(&pool)
            .await
            .expect("inspect an asset-only scene master");
    assert_eq!(asset_only_pin, None);
    assert!(
        sqlx::query(
            "INSERT INTO toonflow.scene_masters(
               project_id,script_id,scene_key,pinned_image_id
             ) VALUES(-9009001,-9009002,'sc10',-9009021)",
        )
        .execute(&pool)
        .await
        .is_err(),
        "a pinned image still requires a selected scene asset"
    );
    assert!(
        sqlx::query(
            "INSERT INTO toonflow.scene_masters(
               project_id,script_id,scene_key,scene_asset_id,pinned_image_id
             ) VALUES(-9009001,-9009002,'sc10',-9009015,-9009025)",
        )
        .execute(&pool)
        .await
        .is_err(),
        "a scene master must bind an asset of type scene"
    );
    assert!(
        sqlx::query(
            "INSERT INTO toonflow.scene_masters(
               project_id,script_id,scene_key,scene_asset_id,pinned_image_id
             ) VALUES(-9009001,-9009002,'sc10',-9009111,-9009121)",
        )
        .execute(&pool)
        .await
        .is_err(),
        "a scene master must not bind another project's scene asset"
    );
    assert!(
        sqlx::query(
            "INSERT INTO toonflow.scene_masters(
               project_id,script_id,scene_key,scene_asset_id,pinned_image_id
             ) VALUES(-9009001,-9009002,'sc10',-9009011,-9009025)",
        )
        .execute(&pool)
        .await
        .is_err(),
        "a scene master image must belong to its selected scene asset"
    );

    let asset_only_revision: i32 =
        sqlx::query_scalar("SELECT revision FROM toonflow.scene_masters WHERE id=$1")
            .bind(asset_only_master_id)
            .fetch_one(&pool)
            .await
            .expect("inspect asset-only master revision");
    sqlx::query("UPDATE toonflow.assets SET image_id=-9009027 WHERE id=-9009012")
        .execute(&pool)
        .await
        .expect("select a completed image for an asset-only master");
    let auto_pinned_master: (Option<i64>, String, i32) = sqlx::query_as(
        "SELECT pinned_image_id,status,revision
         FROM toonflow.scene_masters WHERE id=$1",
    )
    .bind(asset_only_master_id)
    .fetch_one(&pool)
    .await
    .expect("inspect asset image auto-pinning");
    assert_eq!(
        auto_pinned_master,
        (Some(-9009027), "ready".into(), asset_only_revision + 1)
    );
    sqlx::query("DELETE FROM toonflow.scene_masters WHERE id=$1")
        .bind(asset_only_master_id)
        .execute(&pool)
        .await
        .expect("remove the asset auto-pin fixture");

    let sc4_master_before_completion: (i64, Option<i64>, String, i32) = sqlx::query_as(
        "SELECT id,pinned_image_id,status,revision
         FROM toonflow.scene_masters
         WHERE project_id=-9009001 AND script_id=-9009002 AND scene_key='sc4'",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect the incomplete unanimous scene master");
    assert_eq!(sc4_master_before_completion.1, None);
    assert_eq!(sc4_master_before_completion.2, "missing_reference");
    sqlx::query(
        "UPDATE toonflow.images
         SET file_path='/scene/corridor-ready.png',state='已完成'
         WHERE id=-9009024",
    )
    .execute(&pool)
    .await
    .expect("complete an image already selected by a scene asset");
    let sc4_master_ready: (Option<i64>, String, i32) = sqlx::query_as(
        "SELECT pinned_image_id,status,revision
         FROM toonflow.scene_masters WHERE id=$1",
    )
    .bind(sc4_master_before_completion.0)
    .fetch_one(&pool)
    .await
    .expect("inspect image-completion auto-pinning");
    assert_eq!(
        sc4_master_ready,
        (
            Some(-9009024),
            "ready".into(),
            sc4_master_before_completion.3 + 1
        )
    );
    sqlx::query("UPDATE toonflow.images SET state='处理中' WHERE id=-9009024")
        .execute(&pool)
        .await
        .expect("make a pinned master image unavailable");
    let sc4_master_missing: (String, i32) =
        sqlx::query_as("SELECT status,revision FROM toonflow.scene_masters WHERE id=$1")
            .bind(sc4_master_before_completion.0)
            .fetch_one(&pool)
            .await
            .expect("inspect unavailable pinned image status");
    assert_eq!(sc4_master_missing.0, "missing_reference");
    assert_eq!(sc4_master_missing.1, sc4_master_ready.2 + 1);
    sqlx::query("UPDATE toonflow.images SET state='已完成' WHERE id=-9009024")
        .execute(&pool)
        .await
        .expect("restore a pinned master image");
    let sc4_master_restored: (String, i32) =
        sqlx::query_as("SELECT status,revision FROM toonflow.scene_masters WHERE id=$1")
            .bind(sc4_master_before_completion.0)
            .fetch_one(&pool)
            .await
            .expect("inspect restored pinned image status");
    assert_eq!(sc4_master_restored.0, "ready");
    assert_eq!(sc4_master_restored.1, sc4_master_missing.1 + 1);

    let sc1_master_id: i64 = sqlx::query_scalar(
        "SELECT id FROM toonflow.scene_masters
         WHERE project_id=-9009001 AND script_id=-9009002 AND scene_key='sc1'",
    )
    .fetch_one(&pool)
    .await
    .expect("load sc1 master fixture");
    let sc2_master_id: i64 = sqlx::query_scalar(
        "SELECT id FROM toonflow.scene_masters
         WHERE project_id=-9009001 AND script_id=-9009002 AND scene_key='sc2'",
    )
    .fetch_one(&pool)
    .await
    .expect("load sc2 master fixture");
    let sc2_base_state_id: i64 = sqlx::query_scalar(
        "SELECT id FROM toonflow.scene_states
         WHERE scene_master_id=$1 AND state_key='base'",
    )
    .bind(sc2_master_id)
    .fetch_one(&pool)
    .await
    .expect("load sc2 base state fixture");

    assert!(
        sqlx::query(
            "INSERT INTO toonflow.storyboards(
               id,script_id,project_id,scene_key,index,create_time
             ) VALUES(-9009040,-9009002,-9009001,'sc1',8,0)",
        )
        .execute(&pool)
        .await
        .is_err(),
        "new storyboards in a managed scene must bind a state"
    );

    let state_a_id: i64 = sqlx::query_scalar(
        "INSERT INTO toonflow.scene_states(
           scene_master_id,state_key,name,parent_state_id,sequence
         ) VALUES($1,'damaged-stage','损坏阶段',$2,1)
         RETURNING id",
    )
    .bind(sc1_master_id)
    .bind(sc1_state_id)
    .fetch_one(&pool)
    .await
    .expect("create first child state");
    let state_b_id: i64 = sqlx::query_scalar(
        "INSERT INTO toonflow.scene_states(
           scene_master_id,state_key,name,parent_state_id,sequence
         ) VALUES($1,'aftermath','损坏后续',$2,2)
         RETURNING id",
    )
    .bind(sc1_master_id)
    .bind(state_a_id)
    .fetch_one(&pool)
    .await
    .expect("create descendant state");
    let sibling_state_id: i64 = sqlx::query_scalar(
        "INSERT INTO toonflow.scene_states(
           scene_master_id,state_key,name,parent_state_id,sequence
         ) VALUES($1,'alternate','平行变化',$2,3)
         RETURNING id",
    )
    .bind(sc1_master_id)
    .bind(sc1_state_id)
    .fetch_one(&pool)
    .await
    .expect("create sibling state");
    let late_parent_state_id: i64 = sqlx::query_scalar(
        "INSERT INTO toonflow.scene_states(
           scene_master_id,state_key,name,parent_state_id,sequence
         ) VALUES($1,'late-parent','较晚状态',$2,5)
         RETURNING id",
    )
    .bind(sc1_master_id)
    .bind(sc1_state_id)
    .fetch_one(&pool)
    .await
    .expect("create later parent candidate");

    assert!(
        sqlx::query(
            "INSERT INTO toonflow.scene_states(
               scene_master_id,state_key,parent_state_id,sequence
             ) VALUES($1,'foreign-parent',$2,4)",
        )
        .bind(sc1_master_id)
        .bind(sc2_base_state_id)
        .execute(&pool)
        .await
        .is_err(),
        "a state parent must remain in the same scene"
    );
    assert!(
        sqlx::query(
            "INSERT INTO toonflow.scene_states(
               id,scene_master_id,state_key,parent_state_id,sequence
             ) VALUES(-9009200,$1,'self-loop',-9009200,4)",
        )
        .bind(sc1_master_id)
        .execute(&pool)
        .await
        .is_err(),
        "a state must not parent itself"
    );
    assert!(
        sqlx::query(
            "INSERT INTO toonflow.scene_states(
               scene_master_id,state_key,parent_state_id,sequence
             ) VALUES($1,'bad-sequence',$2,4)",
        )
        .bind(sc1_master_id)
        .bind(late_parent_state_id)
        .execute(&pool)
        .await
        .is_err(),
        "a parent sequence must be lower than its child sequence"
    );
    assert!(
        sqlx::query("UPDATE toonflow.scene_states SET parent_state_id=$1 WHERE id=$2")
            .bind(state_b_id)
            .bind(state_a_id)
            .execute(&pool)
            .await
            .is_err(),
        "a parent update must not create a cycle"
    );

    sqlx::query("UPDATE toonflow.storyboards SET scene_state_id=$1 WHERE id=-9009032")
        .bind(state_b_id)
        .execute(&pool)
        .await
        .expect("advance second storyboard to a descendant state");
    sqlx::query("UPDATE toonflow.storyboards SET scene_state_id=$1 WHERE id=-9009031")
        .bind(state_a_id)
        .execute(&pool)
        .await
        .expect("advance first storyboard while preserving timeline ancestry");
    assert!(
        sqlx::query("UPDATE toonflow.scene_states SET parent_state_id=$1 WHERE id=$2")
            .bind(sc1_state_id)
            .bind(state_b_id)
            .execute(&pool)
            .await
            .is_err(),
        "changing a parent must not turn a valid storyboard timeline into sibling jumps"
    );
    assert!(
        sqlx::query("UPDATE toonflow.storyboards SET scene_state_id=$1 WHERE id=-9009032")
            .bind(sibling_state_id)
            .execute(&pool)
            .await
            .is_err(),
        "a storyboard timeline must not jump to a sibling state"
    );
    assert!(
        sqlx::query("UPDATE toonflow.storyboards SET index=-1 WHERE id=-9009032")
            .execute(&pool)
            .await
            .is_err(),
        "reordering storyboards must not create a descendant-to-ancestor timeline"
    );
    sqlx::query("UPDATE toonflow.storyboards SET scene_state_id=$1 WHERE id=-9009031")
        .bind(state_b_id)
        .execute(&pool)
        .await
        .expect("allow consecutive storyboards to hold the same state");
    assert!(
        sqlx::query("UPDATE toonflow.storyboards SET scene_state_id=$1 WHERE id=-9009032")
            .bind(sc1_state_id)
            .execute(&pool)
            .await
            .is_err(),
        "a storyboard timeline must not regress from damage to base"
    );
    sqlx::query("UPDATE toonflow.storyboards SET scene_state_id=$1 WHERE id=-9009031")
        .bind(state_a_id)
        .execute(&pool)
        .await
        .expect("restore the valid ancestor-to-descendant timeline");

    assert!(
        sqlx::query("UPDATE toonflow.scene_masters SET scene_key='sc9' WHERE id=$1")
            .bind(sc1_master_id)
            .execute(&pool)
            .await
            .is_err(),
        "reverse master scope changes must not invalidate storyboard bindings"
    );
    assert!(
        sqlx::query("UPDATE toonflow.scene_states SET scene_master_id=$1 WHERE id=$2")
            .bind(sc2_master_id)
            .bind(state_a_id)
            .execute(&pool)
            .await
            .is_err(),
        "reverse state scope changes must not invalidate children or storyboards"
    );
    assert!(
        sqlx::query("UPDATE toonflow.assets SET type='prop' WHERE id=-9009011")
            .execute(&pool)
            .await
            .is_err(),
        "reverse asset type changes must not invalidate a scene master"
    );
    assert!(
        sqlx::query("UPDATE toonflow.assets SET project_id=-9009101 WHERE id=-9009011")
            .execute(&pool)
            .await
            .is_err(),
        "reverse asset scope changes must not invalidate a scene master"
    );
    assert!(
        sqlx::query("DELETE FROM toonflow.assets WHERE id=-9009011")
            .execute(&pool)
            .await
            .is_err(),
        "a locked scene asset must be reassigned before it can be deleted"
    );
    let master_revision_before_pin: i32 =
        sqlx::query_scalar("SELECT revision FROM toonflow.scene_masters WHERE id=$1")
            .bind(sc1_master_id)
            .fetch_one(&pool)
            .await
            .expect("inspect master revision before pin change");
    sqlx::query("UPDATE toonflow.scene_masters SET pinned_image_id=-9009022 WHERE id=$1")
        .bind(sc1_master_id)
        .execute(&pool)
        .await
        .expect("change a master to another image of the same scene asset");
    let master_revision_after_pin: i32 =
        sqlx::query_scalar("SELECT revision FROM toonflow.scene_masters WHERE id=$1")
            .bind(sc1_master_id)
            .fetch_one(&pool)
            .await
            .expect("inspect master revision after pin change");
    assert_eq!(master_revision_after_pin, master_revision_before_pin + 1);
    sqlx::query(
        "UPDATE toonflow.images SET file_path='/scene/master-alt-v2.png'
         WHERE id=-9009022",
    )
    .execute(&pool)
    .await
    .expect("revise the pinned master image");
    let master_revision_after_image: i32 =
        sqlx::query_scalar("SELECT revision FROM toonflow.scene_masters WHERE id=$1")
            .bind(sc1_master_id)
            .fetch_one(&pool)
            .await
            .expect("inspect master revision after image change");
    assert_eq!(master_revision_after_image, master_revision_after_pin + 1);

    assert!(
        sqlx::query(
            "INSERT INTO toonflow.scene_state_references(
               scene_state_id,sort_order,role,asset_id,image_id
             ) VALUES($1,0,'state',-9009112,-9009122)",
        )
        .bind(state_a_id)
        .execute(&pool)
        .await
        .is_err(),
        "state references must stay in the scene project"
    );

    let tracked_state_ids = [sc1_state_id, state_a_id, state_b_id, sibling_state_id];
    let revisions_before_reference = scene_state_revisions(&pool, tracked_state_ids).await;
    sqlx::query(
        "INSERT INTO toonflow.scene_state_references(
           scene_state_id,sort_order,role,asset_id,image_id,prompt_label
         ) VALUES($1,0,'state',-9009015,-9009025,'破损门板')",
    )
    .bind(state_a_id)
    .execute(&pool)
    .await
    .expect("create a same-project state reference");
    let revisions_after_insert = scene_state_revisions(&pool, tracked_state_ids).await;
    assert_eq!(
        revisions_after_insert,
        [
            revisions_before_reference[0],
            revisions_before_reference[1] + 1,
            revisions_before_reference[2] + 1,
            revisions_before_reference[3],
        ]
    );

    sqlx::query(
        "UPDATE toonflow.scene_state_references
         SET prompt_label='倒地的破损门板'
         WHERE scene_state_id=$1 AND sort_order=0",
    )
    .bind(state_a_id)
    .execute(&pool)
    .await
    .expect("update a state reference");
    let revisions_after_reference_update = scene_state_revisions(&pool, tracked_state_ids).await;
    assert_eq!(
        revisions_after_reference_update,
        [
            revisions_after_insert[0],
            revisions_after_insert[1] + 1,
            revisions_after_insert[2] + 1,
            revisions_after_insert[3],
        ]
    );

    sqlx::query(
        "UPDATE toonflow.images SET file_path='/scene/state-object-v2.png'
         WHERE id=-9009025",
    )
    .execute(&pool)
    .await
    .expect("update a referenced image file");
    let revisions_after_file = scene_state_revisions(&pool, tracked_state_ids).await;
    assert_eq!(
        revisions_after_file,
        [
            revisions_after_reference_update[0],
            revisions_after_reference_update[1] + 1,
            revisions_after_reference_update[2] + 1,
            revisions_after_reference_update[3],
        ]
    );

    sqlx::query("UPDATE toonflow.images SET state='处理中' WHERE id=-9009025")
        .execute(&pool)
        .await
        .expect("update a referenced image state");
    let revisions_after_image_state = scene_state_revisions(&pool, tracked_state_ids).await;
    assert_eq!(
        revisions_after_image_state,
        [
            revisions_after_file[0],
            revisions_after_file[1] + 1,
            revisions_after_file[2] + 1,
            revisions_after_file[3],
        ]
    );

    sqlx::query("UPDATE toonflow.images SET assets_id=-9009016 WHERE id=-9009025")
        .execute(&pool)
        .await
        .expect("move a referenced image to another same-project asset");
    let migrated_reference_asset: i64 = sqlx::query_scalar(
        "SELECT asset_id FROM toonflow.scene_state_references
         WHERE scene_state_id=$1 AND image_id=-9009025",
    )
    .bind(state_a_id)
    .fetch_one(&pool)
    .await
    .expect("inspect cascaded state reference asset");
    assert_eq!(migrated_reference_asset, -9009016);
    let revisions_after_image_asset = scene_state_revisions(&pool, tracked_state_ids).await;
    assert_eq!(
        revisions_after_image_asset,
        [
            revisions_after_image_state[0],
            revisions_after_image_state[1] + 1,
            revisions_after_image_state[2] + 1,
            revisions_after_image_state[3],
        ]
    );
    assert!(
        sqlx::query("UPDATE toonflow.images SET assets_id=-9009112 WHERE id=-9009025")
            .execute(&pool)
            .await
            .is_err(),
        "a referenced image must not move to another project"
    );
    assert!(
        sqlx::query("UPDATE toonflow.assets SET project_id=-9009101 WHERE id=-9009016")
            .execute(&pool)
            .await
            .is_err(),
        "reverse asset project changes must preserve state reference scope"
    );

    sqlx::query(
        "DELETE FROM toonflow.scene_state_references
         WHERE scene_state_id=$1 AND sort_order=0",
    )
    .bind(state_a_id)
    .execute(&pool)
    .await
    .expect("delete a state reference");
    let revisions_after_delete = scene_state_revisions(&pool, tracked_state_ids).await;
    assert_eq!(
        revisions_after_delete,
        [
            revisions_after_image_asset[0],
            revisions_after_image_asset[1] + 1,
            revisions_after_image_asset[2] + 1,
            revisions_after_image_asset[3],
        ]
    );

    sqlx::query("DELETE FROM toonflow.projects WHERE id IN (-9009001,-9009101)")
        .execute(&pool)
        .await
        .expect("remove scene consistency migration fixtures");

    for column in ["failed_login_attempts", "locked_until"] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM information_schema.columns
               WHERE table_schema='public'
                 AND table_name='system_users'
                 AND column_name=$1
             )",
        )
        .bind(column)
        .fetch_one(&pool)
        .await
        .expect("inspect login lockout column");
        assert!(exists, "expected system_users column {column}");
    }

    let lockout_constraint: bool = sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM pg_constraint
           WHERE conrelid='public.system_users'::regclass
             AND conname='system_users_failed_login_attempts_non_negative'
         )",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect login lockout constraint");
    assert!(lockout_constraint);

    let task_id_default: Option<String> = sqlx::query_scalar(
        "SELECT column_default
         FROM information_schema.columns
         WHERE table_schema='toonflow' AND table_name='tasks' AND column_name='id'",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect distributed task id default");
    assert!(
        task_id_default
            .as_deref()
            .is_some_and(|value| value.contains("task_id_seq")),
        "toonflow task ids must come from the database sequence"
    );

    let video_id_default: Option<String> = sqlx::query_scalar(
        "SELECT column_default
         FROM information_schema.columns
         WHERE table_schema='toonflow' AND table_name='videos' AND column_name='id'",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect video id default");
    assert!(
        video_id_default
            .as_deref()
            .is_some_and(|value| value.contains("video_id_seq")),
        "toonflow video ids must come from the database sequence"
    );

    let continuity_fk: Option<(String, String)> = sqlx::query_as(
        "SELECT pg_get_constraintdef(oid),confdeltype::text
         FROM pg_constraint
         WHERE conrelid='toonflow.video_continuity_frames'::regclass
           AND conname='video_continuity_frames_video_project_fk'",
    )
    .fetch_optional(&pool)
    .await
    .expect("inspect continuity frame foreign key");
    let (continuity_definition, continuity_delete_action) =
        continuity_fk.expect("continuity frame video/project foreign key must exist");
    assert!(
        continuity_definition.contains("FOREIGN KEY (previous_video_id, project_id)")
            && continuity_definition.contains("REFERENCES toonflow.videos(id, project_id)"),
        "continuity frame ownership must match its source video"
    );
    assert_eq!(
        continuity_delete_action, "c",
        "continuity frames must cascade when their source video is deleted"
    );

    for column in [
        "message_id",
        "task_id",
        "kind",
        "trace_id",
        "trace_context",
        "payload",
        "state",
        "attempt",
        "max_attempts",
        "published_at",
        "lease_owner",
        "lease_token",
        "lease_until",
        "heartbeat_at",
        "publish_owner",
        "publish_token",
        "publish_until",
    ] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM information_schema.columns
               WHERE table_schema='toonflow'
                 AND table_name='distributed_jobs'
                 AND column_name=$1
             )",
        )
        .bind(column)
        .fetch_one(&pool)
        .await
        .expect("inspect distributed job column");
        assert!(exists, "expected distributed job column {column}");
    }

    let durable_dispatch_index: bool = sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM pg_indexes
           WHERE schemaname='toonflow' AND tablename='distributed_jobs'
             AND indexname='idx_distributed_jobs_dispatch'
             AND indexdef ILIKE '%published_at IS NULL%'
         )",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect distributed outbox index");
    assert!(durable_dispatch_index);

    for column in ["next_run_at", "last_scheduled_at"] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM information_schema.columns
               WHERE table_schema='public'
                 AND table_name='infra_job'
                 AND column_name=$1
             )",
        )
        .bind(column)
        .fetch_one(&pool)
        .await
        .expect("inspect scheduler column");
        assert!(exists, "expected infra_job column {column}");
    }

    let scheduler_index: bool = sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM pg_indexes
           WHERE schemaname='public' AND tablename='infra_job'
             AND indexname='idx_infra_job_due'
         )",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect scheduler due index");
    assert!(scheduler_index);

    let trace_constraint: bool = sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM pg_constraint
           WHERE conrelid='toonflow.distributed_jobs'::regclass
             AND conname='distributed_jobs_trace_context_is_object'
         )",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect trace context constraint");
    assert!(trace_constraint);

    for column in [
        "lease_owner",
        "lease_token",
        "lease_until",
        "next_attempt_at",
        "max_attempts",
    ] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM information_schema.columns
               WHERE table_schema='toonflow'
                 AND table_name='storage_cleanup_tasks'
                 AND column_name=$1
             )",
        )
        .bind(column)
        .fetch_one(&pool)
        .await
        .expect("inspect storage cleanup lease column");
        assert!(exists, "expected storage cleanup lease column {column}");
    }

    let cleanup_task_id_default: Option<String> = sqlx::query_scalar(
        "SELECT column_default
         FROM information_schema.columns
         WHERE table_schema='toonflow'
           AND table_name='storage_cleanup_tasks'
           AND column_name='id'",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect storage cleanup task id default");
    assert!(
        cleanup_task_id_default
            .as_deref()
            .is_some_and(|value| value.contains("storage_cleanup_task_id_seq")),
        "storage cleanup task ids must come from the database sequence"
    );

    for constraint in [
        "storage_cleanup_tasks_attempts_valid",
        "storage_cleanup_tasks_state_valid",
    ] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM pg_constraint
               WHERE conrelid='toonflow.storage_cleanup_tasks'::regclass AND conname=$1
             )",
        )
        .bind(constraint)
        .fetch_one(&pool)
        .await
        .expect("inspect storage cleanup constraint");
        assert!(exists, "expected storage cleanup constraint {constraint}");
    }

    for constraint in [
        "distributed_jobs_task_unique",
        "distributed_jobs_message_unique",
        "distributed_jobs_running_has_lease",
        "distributed_jobs_message_id_not_nil",
        "distributed_jobs_kind_wire_valid",
        "distributed_jobs_trace_wire_valid",
        "distributed_jobs_publish_claim_complete",
    ] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM pg_constraint
               WHERE conrelid='toonflow.distributed_jobs'::regclass AND conname=$1
             )",
        )
        .bind(constraint)
        .fetch_one(&pool)
        .await
        .expect("inspect distributed job constraint");
        assert!(exists, "expected distributed job constraint {constraint}");
    }

    let render_column_types: Vec<(String, String)> = sqlx::query_as(
        "SELECT column_name,udt_name
         FROM information_schema.columns
         WHERE table_schema='toonflow' AND table_name='episode_renders'
           AND column_name IN ('source_video_ids','metadata','created_by')
         ORDER BY column_name",
    )
    .fetch_all(&pool)
    .await
    .expect("inspect episode render column types");
    assert_eq!(
        render_column_types,
        vec![
            ("created_by".into(), "uuid".into()),
            ("metadata".into(), "jsonb".into()),
            ("source_video_ids".into(), "_int8".into()),
        ]
    );

    for constraint in [
        "episode_renders_script_project_fk",
        "episode_renders_project_script_version_unique",
        "episode_renders_export_task_unique",
    ] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM pg_constraint
               WHERE conrelid='toonflow.episode_renders'::regclass AND conname=$1
             )",
        )
        .bind(constraint)
        .fetch_one(&pool)
        .await
        .expect("inspect episode render constraint");
        assert!(exists, "expected episode render constraint {constraint}");
    }

    let current_render_is_unique: bool = sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM pg_indexes
           WHERE schemaname='toonflow' AND tablename='episode_renders'
             AND indexname='uq_toonflow_episode_renders_current'
             AND indexdef ILIKE '%WHERE is_current%'
         )",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect current episode render uniqueness");
    assert!(current_render_is_unique);

    let task_related_objects_is_text: bool = sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM information_schema.columns
           WHERE table_schema='toonflow' AND table_name='tasks'
             AND column_name='related_objects' AND data_type='text'
         )",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect task result metadata type");
    assert!(task_related_objects_is_text);

    for source_key in [
        "script_ai_regex",
        "script_prompt_polish",
        "eventExtraction",
        "scriptAssetExtraction",
        "videoPromptGeneration",
        "audioBindPrompt",
    ] {
        let seeded: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM toonflow.prompts WHERE source_key=$1 AND data<>'')",
        )
        .bind(source_key)
        .fetch_one(&pool)
        .await
        .expect("inspect script prompt seed");
        assert!(seeded, "expected prompt seed {source_key}");
    }

    let script_name_is_unique: bool = sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM pg_indexes
           WHERE schemaname='toonflow'
             AND tablename='scripts'
             AND indexname='uq_toonflow_scripts_project_name'
         )",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect project script name uniqueness");
    assert!(script_name_is_unique);

    let appearance_age_stage_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.columns WHERE table_schema='toonflow' AND table_name='character_appearances' AND column_name='age_stage')",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect character appearance age stage column");
    assert!(appearance_age_stage_exists);

    let project_chat_model_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM information_schema.columns
           WHERE table_schema='toonflow'
             AND table_name='projects'
             AND column_name='chat_model'
         )",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect project chat model column");
    assert!(project_chat_model_exists);

    let storyboard_asset_order_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM information_schema.columns
           WHERE table_schema='toonflow'
             AND table_name='assets_storyboards'
             AND column_name='sort_order'
         )",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect storyboard asset ordering column");
    assert!(storyboard_asset_order_exists);

    for column in [
        "progress_current",
        "progress_total",
        "retry_of_id",
        "agent_run_id",
    ] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM information_schema.columns
               WHERE table_schema='toonflow'
                 AND table_name='workflow_node_runs'
                 AND column_name=$1
             )",
        )
        .bind(column)
        .fetch_one(&pool)
        .await
        .expect("inspect workflow node run column");
        assert!(exists, "expected workflow node run column {column}");
    }

    for column in ["input", "retry_of_id", "progress_current", "progress_total"] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM information_schema.columns
               WHERE table_schema='toonflow' AND table_name='tasks' AND column_name=$1
             )",
        )
        .bind(column)
        .fetch_one(&pool)
        .await
        .expect("inspect task retry metadata column");
        assert!(exists, "expected task column {column}");
    }

    let asset_image_retry_lineage_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM information_schema.columns
           WHERE table_schema='toonflow' AND table_name='images' AND column_name='retry_of_id'
         )",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect asset image retry lineage column");
    assert!(asset_image_retry_lineage_exists);

    let model_prompt_map_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_schema='ai' AND table_name='model_prompt_maps')",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect model prompt map table");
    assert!(model_prompt_map_exists);

    let project_defaults: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT
           (SELECT column_default FROM information_schema.columns
            WHERE table_schema='toonflow' AND table_name='projects' AND column_name='video_ratio'),
           (SELECT column_default FROM information_schema.columns
            WHERE table_schema='toonflow' AND table_name='projects' AND column_name='mode')",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect project video defaults");
    assert_eq!(project_defaults.0.as_deref(), Some("'16:9'::text"));
    assert_eq!(
        project_defaults.1.as_deref(),
        Some("'startEndRequired'::text")
    );

    let asset_isolation_trigger: bool = sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM information_schema.triggers
           WHERE event_object_schema='toonflow'
             AND event_object_table='project_assets'
             AND trigger_name='project_assets_enforce_ownership'
         )",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect project asset isolation trigger");
    assert!(asset_isolation_trigger);

    for table in [
        "ai.model_configs",
        "ai.chat_roles",
        "ai.knowledge_segments",
        "ai.images",
        "ai.music",
        "toonflow.projects",
        "toonflow.project_assets",
        "toonflow.episode_renders",
        "toonflow.distributed_jobs",
        "toonflow.worker_instances",
        "toonflow.workflow_definitions",
        "toonflow.workflow_runs",
        "toonflow.workflow_node_runs",
        "system_users",
        "system_role",
        "system_menu",
        "system_oauth2_access_token",
        "infra_config",
        "infra_job",
        "infra_job_log",
        "infra_api_access_log",
        "infra_api_error_log",
        "infra_codegen_table",
        "infra_codegen_column",
        "yudao_demo01_contact",
        "yudao_demo02_category",
        "yudao_demo03_student",
        "yudao_demo03_course",
        "yudao_demo03_grade",
    ] {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(table)
            .fetch_one(&pool)
            .await
            .expect("inspect expected table");
        assert!(exists, "expected table {table}");
    }
    for sequence in [
        "toonflow.storage_cleanup_task_id_seq",
        "toonflow.task_id_seq",
        "system_dict_data_seq",
        "system_login_log_seq",
        "system_mail_log_seq",
        "system_notify_message_seq",
        "system_oauth2_access_token_seq",
        "system_oauth2_refresh_token_seq",
        "system_operate_log_seq",
        "system_sms_log_seq",
        "system_tenant_package_seq",
        "system_tenant_seq",
        "system_user_post_seq",
        "system_user_role_seq",
        "system_users_seq",
    ] {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(sequence)
            .fetch_one(&pool)
            .await
            .expect("inspect expected sequence");
        assert!(exists, "expected sequence {sequence}");
    }
    for removed in ["toonflow.vendor_configs", "toonflow.model_prompts"] {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(removed)
            .fetch_one(&pool)
            .await
            .expect("inspect removed table");
        assert!(!exists, "legacy table {removed} must be removed");
    }
    let menu = sqlx::query("SELECT component,deleted FROM system_menu WHERE id=30006")
        .fetch_one(&pool)
        .await
        .expect("AI model menu exists");
    assert_eq!(menu.get::<String, _>("component"), "ai/model/model/index");
    assert_eq!(menu.get::<i16, _>("deleted"), 1);

    let duplicate_route_names: i64 = sqlx::query_scalar(
        "SELECT count(*)
         FROM (
             SELECT CASE
                    WHEN coalesce(component_name, '') <> '' THEN component_name
                    ELSE name
                    END AS route_name
             FROM system_menu
             WHERE deleted = 0 AND status = 0 AND type <> 3
             GROUP BY route_name
             HAVING count(*) > 1
         ) duplicate_routes",
    )
    .fetch_one(&pool)
    .await
    .expect("read duplicate route names");
    assert_eq!(
        duplicate_route_names, 0,
        "active route menus must not generate duplicate frontend route names"
    );

    let active_bpm_menus: i64 = sqlx::query_scalar(
        "SELECT count(*)
         FROM system_menu
         WHERE deleted = 0
           AND (id IN (1186, 1200) OR component LIKE 'bpm/%' OR permission LIKE 'bpm:%')",
    )
    .fetch_one(&pool)
    .await
    .expect("read active BPM menu count");
    assert_eq!(
        active_bpm_menus, 0,
        "unimplemented BPM menus must stay hidden"
    );

    let bpm_dict_types: i64 =
        sqlx::query_scalar("SELECT count(*) FROM system_dict_type WHERE type LIKE 'bpm%'")
            .fetch_one(&pool)
            .await
            .expect("inspect BPM dictionary types");
    assert_eq!(bpm_dict_types, 0, "legacy BPM dictionaries must be removed");

    let restored_menu_catalog: i64 =
        sqlx::query_scalar("SELECT count(*) FROM system_menu WHERE deleted = 0")
            .fetch_one(&pool)
            .await
            .expect("read restored menu catalog");
    assert!(
        restored_menu_catalog >= 250,
        "fresh bootstrap must include the complete backend menu and permission catalog"
    );

    let restored_navigation_roots: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM system_menu
         WHERE id IN (1, 2, 2758) AND parent_id = 0
           AND type = 1 AND status = 0 AND deleted = 0",
    )
    .fetch_one(&pool)
    .await
    .expect("read restored navigation roots");
    assert_eq!(
        restored_navigation_roots, 3,
        "system, infrastructure, and AI navigation roots must be available"
    );

    let orphaned_active_menus: i64 = sqlx::query_scalar(
        "SELECT count(*)
         FROM system_menu child
         LEFT JOIN system_menu parent
           ON parent.id = child.parent_id AND parent.deleted = 0
         WHERE child.deleted = 0 AND child.parent_id <> 0 AND parent.id IS NULL",
    )
    .fetch_one(&pool)
    .await
    .expect("read orphaned active menus");
    assert_eq!(
        orphaned_active_menus, 0,
        "active menus must not disappear because their parent is missing"
    );

    for (table, column) in [
        ("system_dept", "tenant_id"),
        ("system_post", "tenant_id"),
        ("system_role", "tenant_id"),
        ("system_role", "data_scope_dept_ids"),
    ] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM information_schema.columns
               WHERE table_schema='public' AND table_name=$1 AND column_name=$2
             )",
        )
        .bind(table)
        .bind(column)
        .fetch_one(&pool)
        .await
        .expect("inspect restored management column");
        assert!(exists, "expected management column {table}.{column}");
    }

    let knowledge_status: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.columns WHERE table_schema='ai' AND table_name='chat_messages' AND column_name='knowledge_status')",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect chat knowledge status column");
    assert!(knowledge_status, "expected chat knowledge status column");

    let department_leader_type: String = sqlx::query_scalar(
        "SELECT udt_name FROM information_schema.columns
         WHERE table_schema='public' AND table_name='system_dept'
           AND column_name='leader_user_id'",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect department leader identifier type");
    assert_eq!(department_leader_type, "int8");

    let infra_config_sequence_is_synchronized: bool = sqlx::query_scalar(
        "SELECT last_value >= COALESCE((SELECT max(id) FROM infra_config), 1)
         FROM infra_config_seq",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect infrastructure configuration sequence");
    assert!(infra_config_sequence_is_synchronized);

    let active_menu_links: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM system_menu
         WHERE deleted = 0 AND visible = false AND active_menu_id IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .expect("read hidden-page business menu links");
    assert!(active_menu_links >= 10);

    let administrators: i64 = sqlx::query_scalar(
        "SELECT count(*)
         FROM system_users u
         JOIN system_user_role ur ON ur.user_id = u.id AND ur.deleted = 0
         JOIN system_role r ON r.id = ur.role_id AND r.deleted = 0
         WHERE u.deleted = 0 AND u.status = 0 AND r.status = 0
           AND r.code = 'super_admin'",
    )
    .fetch_one(&pool)
    .await
    .expect("read seeded administrators");
    assert!(administrators > 0);

    let baseline_tenants: i64 =
        sqlx::query_scalar("SELECT count(*) FROM system_tenant WHERE deleted = 0")
            .fetch_one(&pool)
            .await
            .expect("read baseline tenants");
    assert_eq!(
        baseline_tenants, 3,
        "fresh migration bootstrap must restore the current baseline tenants, not synthesize a default company"
    );

    let current_baseline_tenant_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM system_tenant
            WHERE id = 1 AND name = '芋道源码' AND deleted = 0
         )",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect current baseline tenant");
    assert!(
        current_baseline_tenant_exists,
        "fresh migration bootstrap must use the current database baseline data"
    );

    let organization_baseline: (i64, i64) = sqlx::query_as(
        "SELECT
            (SELECT count(*) FROM system_dept WHERE deleted = 0),
            (SELECT count(*) FROM system_post WHERE deleted = 0)",
    )
    .fetch_one(&pool)
    .await
    .expect("read organization baseline");
    assert!(organization_baseline.0 >= 10, "departments must be seeded");
    assert!(organization_baseline.1 >= 4, "posts must be seeded");

    let administrator_department_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1
            FROM system_users users
            JOIN system_dept dept ON dept.id = users.dept_id AND dept.deleted = 0
            WHERE users.username = 'admin' AND users.deleted = 0
         )",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect administrator department");
    assert!(administrator_department_exists);

    let dictionary_baseline: (i64, i64, i64) = sqlx::query_as(
        "SELECT
            (SELECT count(*) FROM system_dict_type WHERE deleted = 0),
            (SELECT count(DISTINCT dict_type) FROM system_dict_data WHERE deleted = 0),
            (SELECT count(*) FROM system_dict_data data
             WHERE data.deleted = 0
               AND NOT EXISTS (
                   SELECT 1 FROM system_dict_type type
                   WHERE type.type = data.dict_type AND type.deleted = 0
               ))",
    )
    .fetch_one(&pool)
    .await
    .expect("read dictionary baseline");
    assert_eq!(
        dictionary_baseline.0, 40,
        "only current product dictionaries remain active"
    );
    assert!(dictionary_baseline.0 >= dictionary_baseline.1);
    assert_eq!(
        dictionary_baseline.2, 0,
        "every dictionary must have a type"
    );

    // An upgrade also retires existing options, while an unrelated custom
    // dictionary sharing a legacy prefix must remain usable.
    sqlx::raw_sql(
        "UPDATE system_dict_type SET deleted=0 WHERE type='crm_customer_industry';
         UPDATE system_dict_data SET deleted=0 WHERE dict_type='crm_customer_industry';
         INSERT INTO system_dict_type(id,name,type,status)
         VALUES(-9001101,'Custom dictionary','crm_custom_dictionary',0);
         INSERT INTO system_dict_data(id,label,value,dict_type,status)
         VALUES(-9001102,'Custom option','custom','crm_custom_dictionary',0);",
    )
    .execute(&pool)
    .await
    .expect("prepare dictionary cleanup upgrade and custom dictionary fixtures");

    for _ in 0..2 {
        sqlx::raw_sql(include_str!(
            "../../../../sql/postgresql/0011_retire_unrelated_business_dictionaries.sql"
        ))
        .execute(&pool)
        .await
        .expect("dictionary cleanup migration upgrades and reruns safely");
    }

    let retired_dictionary: (i64, i64, i64) = sqlx::query_as(
        "SELECT
           (SELECT count(*) FROM system_dict_type WHERE type='crm_customer_industry' AND deleted=0),
           (SELECT count(*) FROM system_dict_data WHERE dict_type='crm_customer_industry' AND deleted=0),
           (SELECT count(*) FROM system_dict_data WHERE dict_type='crm_customer_industry' AND deleted=1)",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect retired dictionary and recoverable options");
    assert_eq!(retired_dictionary.0, 0);
    assert_eq!(retired_dictionary.1, 0);
    assert!(retired_dictionary.2 > 0);

    let preserved_dictionaries: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM system_dict_type WHERE deleted=0 AND type IN
           ('common_status','system_user_sex','infra_config_type','ai_platform','crm_custom_dictionary')",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect preserved system and custom dictionaries");
    assert_eq!(preserved_dictionaries, 5);

    let custom_option: i64 =
        sqlx::query_scalar("SELECT count(*) FROM system_dict_data WHERE id=-9001102 AND deleted=0")
            .fetch_one(&pool)
            .await
            .expect("inspect preserved custom option");
    assert_eq!(custom_option, 1);
    sqlx::raw_sql(
        "DELETE FROM system_dict_data WHERE id=-9001102;
         DELETE FROM system_dict_type WHERE id=-9001101;",
    )
    .execute(&pool)
    .await
    .expect("remove custom dictionary fixtures");

    let legacy_schema_exists: bool =
        sqlx::query_scalar("SELECT to_regnamespace('system') IS NOT NULL")
            .fetch_one(&pool)
            .await
            .expect("inspect legacy schema");
    assert!(!legacy_schema_exists);

    for runtime_table in [
        "system_oauth2_access_token",
        "system_oauth2_refresh_token",
        "system_login_log",
        "system_operate_log",
        "system_notify_message",
    ] {
        let rows: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {runtime_table}"))
            .fetch_one(&pool)
            .await
            .expect("read runtime table");
        assert_eq!(rows, 0, "{runtime_table} must start empty");
    }

    let codex_test_users: i64 =
        sqlx::query_scalar("SELECT count(*) FROM system_users WHERE username='codex_excel_user'")
            .fetch_one(&pool)
            .await
            .expect("read test users");
    assert_eq!(
        codex_test_users, 0,
        "transient test users must not be seeded"
    );

    let users_with_login_traces: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM system_users WHERE login_ip <> '' OR login_date IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .expect("read user login traces");
    assert_eq!(
        users_with_login_traces, 0,
        "seed users must not carry login traces"
    );

    let users_with_remote_yudao_avatar: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM system_users
         WHERE avatar LIKE 'http://test.yudao.iocoder.cn/%'
            OR avatar LIKE 'https://test.yudao.iocoder.cn/%'",
    )
    .fetch_one(&pool)
    .await
    .expect("read user avatars");
    assert_eq!(
        users_with_remote_yudao_avatar, 0,
        "seed users must not depend on remote Yudao avatar assets"
    );

    let plain_mail_passwords: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM system_mail_account
         WHERE password IS NOT NULL AND password <> '' AND password NOT LIKE 'enc:v1:%'",
    )
    .fetch_one(&pool)
    .await
    .expect("read mail secrets");
    assert_eq!(
        plain_mail_passwords, 0,
        "mail account passwords must be sealed"
    );

    let plain_sms_secrets: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM system_sms_channel
         WHERE api_key NOT LIKE 'enc:v1:%'
            OR (api_secret IS NOT NULL AND api_secret <> '' AND api_secret NOT LIKE 'enc:v1:%')",
    )
    .fetch_one(&pool)
    .await
    .expect("read sms secrets");
    assert_eq!(plain_sms_secrets, 0, "sms channel secrets must be sealed");
}
