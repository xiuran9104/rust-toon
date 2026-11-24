//! 制作画布的变更版本号。
//!
//! 画布刷新一轮要读三个端点，外加每个工作流节点一次查询。制作 Agent 运行期间
//! 每 1.8 秒一轮，全部是全量载荷。这里把画布实际读取的行集压成一个版本号，
//! 前端版本没变就跳过刷新。
//!
//! 每张表取行集的 `xmin`（行版本所属的事务号）做摘要：插入、更新、删除都会
//! 改变结果。storyboards、videos、video_tracks、images 都没有 update_time，
//! 按时间戳判不了变更；逐列哈希又会漏掉以后新加的列。`xmin` 两个问题都没有，
//! 误判方向只会是多刷新一次，不会漏刷新。

use sqlx::PgPool;

/// 覆盖 `get_flow_data`、`get_storyboards`、`load_generate_data` 与
/// `latest_node_run` 读取的全部表。新增读取路径时必须同步补进来，
/// 否则那部分数据的变化不会触发画布刷新（见 `database_tests`）。
const FLOW_VERSION_SQL: &str = r#"
WITH scope_assets AS (
    SELECT a.id, a.image_id
    FROM toonflow.assets a
    WHERE a.project_id = $1
      AND (a.id IN (SELECT asset_id FROM toonflow.script_assets WHERE script_id = $2)
        OR a.parent_asset_id IN (SELECT asset_id FROM toonflow.script_assets WHERE script_id = $2))
)
SELECT md5(concat_ws('|',
    (SELECT md5(coalesce(string_agg(t.xmin::text, ',' ORDER BY t.ctid), '-'))
       FROM toonflow.agent_work_data t
      WHERE t.project_id = $1 AND t.episodes_id = $2 AND t.key = 'productionAgent'),
    (SELECT md5(coalesce(string_agg(t.xmin::text, ',' ORDER BY t.ctid), '-'))
       FROM toonflow.scripts t WHERE t.id = $2 AND t.project_id = $1),
    (SELECT md5(coalesce(string_agg(t.xmin::text, ',' ORDER BY t.ctid), '-'))
       FROM toonflow.script_assets t WHERE t.script_id = $2),
    (SELECT md5(coalesce(string_agg(t.xmin::text, ',' ORDER BY t.ctid), '-'))
       FROM toonflow.assets t
      WHERE t.project_id = $1
        AND (t.id IN (SELECT id FROM scope_assets)
          OR t.parent_asset_id IN (SELECT id FROM scope_assets))),
    (SELECT md5(coalesce(string_agg(t.xmin::text, ',' ORDER BY t.ctid), '-'))
       FROM toonflow.images t
      WHERE t.id IN (SELECT image_id FROM scope_assets WHERE image_id IS NOT NULL)
         OR t.assets_id IN (SELECT id FROM scope_assets)
         OR t.id IN (SELECT m.pinned_image_id FROM toonflow.scene_masters m
                      WHERE m.project_id = $1 AND m.script_id = $2)),
    (SELECT md5(coalesce(string_agg(t.xmin::text, ',' ORDER BY t.ctid), '-'))
       FROM toonflow.character_appearances t
      WHERE t.project_id = $1 AND t.script_id = $2),
    (SELECT md5(coalesce(string_agg(t.xmin::text, ',' ORDER BY t.ctid), '-'))
       FROM toonflow.storyboards t WHERE t.project_id = $1 AND t.script_id = $2),
    (SELECT md5(coalesce(string_agg(t.xmin::text, ',' ORDER BY t.ctid), '-'))
       FROM toonflow.assets_storyboards t
      WHERE t.storyboard_id IN (SELECT id FROM toonflow.storyboards
                                 WHERE project_id = $1 AND script_id = $2)),
    (SELECT md5(coalesce(string_agg(t.xmin::text, ',' ORDER BY t.ctid), '-'))
       FROM toonflow.scene_masters t WHERE t.project_id = $1 AND t.script_id = $2),
    (SELECT md5(coalesce(string_agg(t.xmin::text, ',' ORDER BY t.ctid), '-'))
       FROM toonflow.scene_states t
      WHERE t.scene_master_id IN (SELECT id FROM toonflow.scene_masters
                                   WHERE project_id = $1 AND script_id = $2)),
    (SELECT md5(coalesce(string_agg(t.xmin::text, ',' ORDER BY t.ctid), '-'))
       FROM toonflow.asset_audio_bindings t
      WHERE t.asset_role_id IN (SELECT id FROM scope_assets)),
    (SELECT md5(coalesce(string_agg(t.xmin::text, ',' ORDER BY t.ctid), '-'))
       FROM toonflow.video_tracks t WHERE t.project_id = $1 AND t.script_id = $2),
    (SELECT md5(coalesce(string_agg(t.xmin::text, ',' ORDER BY t.ctid), '-'))
       FROM toonflow.videos t WHERE t.project_id = $1 AND t.script_id = $2),
    (SELECT md5(coalesce(string_agg(t.xmin::text, ',' ORDER BY t.ctid), '-'))
       FROM toonflow.scene_transitions t WHERE t.project_id = $1 AND t.script_id = $2),
    (SELECT md5(coalesce(string_agg(t.xmin::text, ',' ORDER BY t.ctid), '-'))
       FROM toonflow.workflow_definitions t WHERE t.project_id = $1 AND t.script_id = $2),
    (SELECT md5(coalesce(string_agg(t.xmin::text, ',' ORDER BY t.ctid), '-'))
       FROM toonflow.workflow_runs t WHERE t.project_id = $1 AND t.script_id = $2),
    (SELECT md5(coalesce(string_agg(t.xmin::text, ',' ORDER BY t.ctid), '-'))
       FROM toonflow.workflow_node_runs t
      WHERE t.workflow_run_id IN (SELECT id FROM toonflow.workflow_runs
                                   WHERE project_id = $1 AND script_id = $2))
))"#;

pub(crate) async fn production_flow_version(
    pool: &PgPool,
    project_id: i64,
    script_id: i64,
) -> Result<String, sqlx::Error> {
    sqlx::query_scalar::<_, String>(FLOW_VERSION_SQL)
        .bind(project_id)
        .bind(script_id)
        .fetch_one(pool)
        .await
}

#[cfg(test)]
mod database_tests {
    use std::time::Duration;

    use rust_toon_framework_database::{DatabaseConfig, connect, migrate};
    use sqlx::PgPool;

    use super::production_flow_version;

    const PROJECT_ID: i64 = 9_500_000;
    const SCRIPT_ID: i64 = 9_500_010;
    /// 与本剧本的各类派生 ID（剧本ID + 100..=800）不重叠。
    const OTHER_SCRIPT_ID: i64 = 9_510_010;
    /// 两个剧本各六个分镜，ID 为 剧本ID + 偏移。
    const STORYBOARD_OFFSETS: [i64; 6] = [200, 201, 202, 203, 204, 205];

    fn storyboard_ids() -> Vec<i64> {
        [SCRIPT_ID, OTHER_SCRIPT_ID]
            .into_iter()
            .flat_map(|script| {
                STORYBOARD_OFFSETS
                    .into_iter()
                    .map(move |offset| script + offset)
            })
            .collect()
    }

    /// 清掉本测试的全部数据。storyboards 对项目没有外键，不会被级联删除，
    /// 而它又被 assets_storyboards 引用，所以必须先于项目删除。
    async fn cleanup(pool: &PgPool) {
        sqlx::query("DELETE FROM toonflow.assets_storyboards WHERE storyboard_id = ANY($1)")
            .bind(storyboard_ids())
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM toonflow.videos WHERE id=$1")
            .bind(SCRIPT_ID + 800)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "DELETE FROM toonflow.video_tracks WHERE project_id=$1 AND script_id = ANY($2)",
        )
        .bind(PROJECT_ID)
        .bind(vec![SCRIPT_ID, OTHER_SCRIPT_ID])
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("DELETE FROM toonflow.storyboards WHERE id = ANY($1)")
            .bind(storyboard_ids())
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM toonflow.projects WHERE id=$1")
            .bind(PROJECT_ID)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn seed(pool: &PgPool) {
        cleanup(pool).await;
        sqlx::query("INSERT INTO toonflow.projects(id,name,create_time,update_time) VALUES($1,'flow version test',0,0)")
            .bind(PROJECT_ID).execute(pool).await.unwrap();
        for (script_id, script_name) in [(SCRIPT_ID, "剧本一"), (OTHER_SCRIPT_ID, "剧本二")] {
            sqlx::query("INSERT INTO toonflow.scripts(id,name,content,project_id,create_time) VALUES($1,$2,'',$3,0)")
                .bind(script_id).bind(script_name).bind(PROJECT_ID).execute(pool).await.unwrap();
            sqlx::query("INSERT INTO toonflow.agent_work_data(id,project_id,episodes_id,key,data,create_time,update_time) VALUES($1,$2,$3,'productionAgent','{\"scriptPlan\":\"\"}',0,0)")
                .bind(script_id + 100).bind(PROJECT_ID).bind(script_id).execute(pool).await.unwrap();
            for offset in STORYBOARD_OFFSETS {
                sqlx::query("INSERT INTO toonflow.storyboards(id,script_id,prompt,project_id,create_time) VALUES($1,$2,'',$3,0)")
                    .bind(script_id + offset).bind(script_id).bind(PROJECT_ID).execute(pool).await.unwrap();
            }
        }

        // 本剧本的制作数据：角色 + 衍生 + 配音绑定 + 造型 + 场景 + 工作流。
        let role = SCRIPT_ID + 300;
        for (asset_id, parent) in [
            (role, None),
            (SCRIPT_ID + 301, Some(role)),
            (SCRIPT_ID + 302, None),
        ] {
            sqlx::query("INSERT INTO toonflow.assets(id,name,prompt,type,description,project_id,parent_asset_id) VALUES($1,'角色','提示词','role','说明',$2,$3)")
                .bind(asset_id).bind(PROJECT_ID).bind(parent).execute(pool).await.unwrap();
        }
        sqlx::query("INSERT INTO toonflow.script_assets(script_id,asset_id) VALUES($1,$2)")
            .bind(SCRIPT_ID)
            .bind(role)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO toonflow.images(id,file_path,assets_id,state) VALUES($1,'/a.png',$2,'已完成')")
            .bind(SCRIPT_ID + 400).bind(role).execute(pool).await.unwrap();
        sqlx::query("UPDATE toonflow.assets SET image_id=$2 WHERE id=$1")
            .bind(role)
            .bind(SCRIPT_ID + 400)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO toonflow.character_appearances(id,project_id,script_id,role_asset_id,name,scenes,costume_prompt,description,create_time,update_time,age_stage) VALUES($1,$2,$3,$4,'造型','[]','服装','说明',0,0,'')")
            .bind(SCRIPT_ID + 500).bind(PROJECT_ID).bind(SCRIPT_ID).bind(role).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO toonflow.assets_storyboards(storyboard_id,asset_id,sort_order) VALUES($1,$2,0)")
            .bind(SCRIPT_ID + 200).bind(role).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO toonflow.asset_audio_bindings(asset_role_id,asset_audio_id,create_time) VALUES($1,$2,0)")
            .bind(role).bind(SCRIPT_ID + 301).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO toonflow.scene_masters(id,project_id,script_id,scene_key,name) VALUES($1,$2,$3,'sc1','客厅')")
            .bind(SCRIPT_ID + 600).bind(PROJECT_ID).bind(SCRIPT_ID).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO toonflow.scene_states(id,scene_master_id,state_key,sequence) VALUES($1,$2,'base',0)")
            .bind(SCRIPT_ID + 601).bind(SCRIPT_ID + 600).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO toonflow.scene_transitions(project_id,script_id,from_scene_key,to_scene_key) VALUES($1,$2,'sc1','sc2')")
            .bind(PROJECT_ID).bind(SCRIPT_ID).execute(pool).await.unwrap();
        let definition_id: i64 = sqlx::query_scalar(
            "INSERT INTO toonflow.workflow_definitions(project_id,script_id,version,create_time,update_time) VALUES($1,$2,1,0,0) RETURNING id",
        )
        .bind(PROJECT_ID)
        .bind(SCRIPT_ID)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO toonflow.workflow_runs(id,workflow_definition_id,project_id,script_id,definition_version,create_time) VALUES($1,$2,$3,$4,1,0)")
            .bind(SCRIPT_ID + 700).bind(definition_id).bind(PROJECT_ID).bind(SCRIPT_ID).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO toonflow.workflow_node_runs(id,workflow_run_id,node_id,node_type,create_time) VALUES($1,$2,'storyboard','storyboard',0)")
            .bind(SCRIPT_ID + 701).bind(SCRIPT_ID + 700).execute(pool).await.unwrap();
    }

    async fn version(pool: &PgPool, script_id: i64) -> String {
        production_flow_version(pool, PROJECT_ID, script_id)
            .await
            .unwrap()
    }

    async fn run(pool: &PgPool, statement: &str, binds: &[i64]) {
        let mut query = sqlx::query(statement);
        for value in binds {
            query = query.bind(*value);
        }
        query.execute(pool).await.unwrap();
    }

    /// 每张表一次代表性变更都必须翻转版本号；别的剧本的变更不能影响本剧本。
    #[tokio::test]
    #[ignore = "run with script/test-database-migrations.sh"]
    async fn every_canvas_source_table_flips_the_version() {
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
        let config = DatabaseConfig::new(url, 1, 5, Duration::from_secs(10)).unwrap();
        let pool = connect(&config).await.unwrap();
        migrate(&pool).await.unwrap();
        seed(&pool).await;

        // 没有写入时版本号稳定，且两个剧本互不相同。
        let mut current = version(&pool, SCRIPT_ID).await;
        assert_eq!(current, version(&pool, SCRIPT_ID).await);
        let other_before = version(&pool, OTHER_SCRIPT_ID).await;
        assert_ne!(current, other_before);

        let mutations: Vec<(&str, &str, Vec<i64>)> = vec![
            (
                "agent_work_data",
                "UPDATE toonflow.agent_work_data SET data='{\"scriptPlan\":\"x\"}'::jsonb, update_time=1 WHERE project_id=$1 AND episodes_id=$2",
                vec![PROJECT_ID, SCRIPT_ID],
            ),
            (
                "scripts",
                "UPDATE toonflow.scripts SET content='新正文' WHERE id=$1",
                vec![SCRIPT_ID],
            ),
            (
                "script_assets",
                "INSERT INTO toonflow.script_assets(script_id,asset_id) VALUES($1,$2)",
                vec![SCRIPT_ID, SCRIPT_ID + 302],
            ),
            (
                "assets",
                "UPDATE toonflow.assets SET name='新名字' WHERE id=$1",
                vec![SCRIPT_ID + 300],
            ),
            (
                "images",
                "UPDATE toonflow.images SET state='生成失败' WHERE id=$1",
                vec![SCRIPT_ID + 400],
            ),
            (
                "character_appearances",
                "UPDATE toonflow.character_appearances SET name='新造型' WHERE id=$1",
                vec![SCRIPT_ID + 500],
            ),
            (
                "storyboards",
                "UPDATE toonflow.storyboards SET state='生成中' WHERE id=$1",
                vec![SCRIPT_ID + 200],
            ),
            (
                "assets_storyboards",
                "UPDATE toonflow.assets_storyboards SET sort_order=3 WHERE storyboard_id=$1",
                vec![SCRIPT_ID + 200],
            ),
            (
                "scene_masters",
                "UPDATE toonflow.scene_masters SET name='卧室' WHERE id=$1",
                vec![SCRIPT_ID + 600],
            ),
            (
                "scene_states",
                "UPDATE toonflow.scene_states SET name='受损' WHERE id=$1",
                vec![SCRIPT_ID + 601],
            ),
            (
                "asset_audio_bindings",
                "UPDATE toonflow.asset_audio_bindings SET create_time=7 WHERE asset_role_id=$1",
                vec![SCRIPT_ID + 300],
            ),
            (
                "video_tracks",
                "INSERT INTO toonflow.video_tracks(project_id,script_id,state,sort_order,continuity_mode,transition_type,frame_policy,transition_source,trim_start_ms,transition_duration_ms) VALUES($1,$2,'未生成',0,'auto','cut','own','manual',0,0)",
                vec![PROJECT_ID, SCRIPT_ID],
            ),
            (
                "videos",
                "INSERT INTO toonflow.videos(id,project_id,script_id,state,generation_context) VALUES($1,$2,$3,'生成中','{}')",
                vec![SCRIPT_ID + 800, PROJECT_ID, SCRIPT_ID],
            ),
            (
                "scene_transitions",
                "UPDATE toonflow.scene_transitions SET description='硬切' WHERE project_id=$1 AND script_id=$2",
                vec![PROJECT_ID, SCRIPT_ID],
            ),
            (
                "workflow_definitions",
                "UPDATE toonflow.workflow_definitions SET version=version+1, update_time=1 WHERE project_id=$1 AND script_id=$2",
                vec![PROJECT_ID, SCRIPT_ID],
            ),
            (
                "workflow_runs",
                "UPDATE toonflow.workflow_runs SET state='running' WHERE id=$1",
                vec![SCRIPT_ID + 700],
            ),
            (
                "workflow_node_runs",
                "UPDATE toonflow.workflow_node_runs SET state='running', progress_total=3 WHERE id=$1",
                vec![SCRIPT_ID + 701],
            ),
        ];
        for (table, statement, binds) in &mutations {
            run(&pool, statement, binds).await;
            let next = version(&pool, SCRIPT_ID).await;
            assert_ne!(next, current, "变更 {table} 后版本号必须变化");
            current = next;
        }

        // 行集增长：新增衍生资产、新增一张资产的生成记录。
        run(
            &pool,
            "INSERT INTO toonflow.assets(id,name,prompt,type,description,project_id,parent_asset_id) VALUES($1,'新造型','提示词','role','说明',$2,$3)",
            &[SCRIPT_ID + 303, PROJECT_ID, SCRIPT_ID + 300],
        )
        .await;
        let next = version(&pool, SCRIPT_ID).await;
        assert_ne!(next, current, "新增衍生资产后版本号必须变化");
        current = next;
        run(
            &pool,
            "INSERT INTO toonflow.images(id,file_path,assets_id,state) VALUES($1,'/b.png',$2,'生成中')",
            &[SCRIPT_ID + 401, SCRIPT_ID + 300],
        )
        .await;
        let next = version(&pool, SCRIPT_ID).await;
        assert_ne!(next, current, "新增生成记录后版本号必须变化");
        current = next;

        // 别的剧本的变化不影响本剧本。
        run(
            &pool,
            "UPDATE toonflow.storyboards SET state='生成失败' WHERE id=$1",
            &[OTHER_SCRIPT_ID + 200],
        )
        .await;
        run(
            &pool,
            "UPDATE toonflow.agent_work_data SET data='{\"scriptPlan\":\"别的剧本\"}'::jsonb WHERE episodes_id=$1",
            &[OTHER_SCRIPT_ID],
        )
        .await;
        assert_eq!(
            version(&pool, SCRIPT_ID).await,
            current,
            "其他剧本的变化不应影响本剧本"
        );
        assert_ne!(version(&pool, OTHER_SCRIPT_ID).await, other_before);

        // 删除同样要翻转版本号。
        run(
            &pool,
            "DELETE FROM toonflow.storyboards WHERE id=$1",
            &[SCRIPT_ID + 205],
        )
        .await;
        assert_ne!(
            version(&pool, SCRIPT_ID).await,
            current,
            "删除分镜后版本号必须变化"
        );

        cleanup(&pool).await;
    }
}
