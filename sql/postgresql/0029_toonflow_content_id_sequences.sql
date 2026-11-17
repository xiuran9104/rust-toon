-- Allocate Toonflow content identifiers in PostgreSQL.
--
-- These identifiers used to be minted in Rust as `now_ms() + offset`, where the
-- offset only distinguished rows inside a single request. Two requests that
-- started within the same millisecond therefore produced overlapping identifier
-- ranges. The frontend imports a novel file in 20-chapter batches, so the
-- second batch collided with the first one and the request failed with
-- `duplicate key value violates unique constraint "novels_pkey"`.
--
-- Every table listed below now draws its identifier from a dedicated sequence,
-- so concurrent requests, batched imports and workflow runs can never collide
-- regardless of wall-clock timing. This follows the pattern already used by
-- `toonflow.task_id_seq`, `toonflow.video_id_seq` and
-- `toonflow.storage_cleanup_task_id_seq`.

DO $$
DECLARE
    target text;
BEGIN
    FOREACH target IN ARRAY ARRAY[
        'agent_memories',
        'agent_runs',
        'agent_tool_calls',
        'art_styles',
        'assets',
        'character_appearances',
        'events',
        'image_flows',
        'images',
        'novels',
        'projects',
        'scripts',
        'storyboards',
        'video_tracks'
    ]
    LOOP
        EXECUTE format('CREATE SEQUENCE IF NOT EXISTS toonflow.%I_id_seq AS bigint', target);
        EXECUTE format(
            'ALTER SEQUENCE toonflow.%I_id_seq OWNED BY toonflow.%I.id',
            target,
            target
        );
        -- Never hand out an identifier that already exists, and never move the
        -- sequence backwards when this migration is replayed.
        EXECUTE format(
            'SELECT setval(
                 ''toonflow.%I_id_seq''::regclass,
                 GREATEST(
                     COALESCE((SELECT max(id) + 1 FROM toonflow.%I), 1),
                     (SELECT CASE WHEN is_called THEN last_value + 1 ELSE last_value END
                        FROM toonflow.%I_id_seq)
                 ),
                 false
             )',
            target,
            target,
            target
        );
        EXECUTE format(
            'ALTER TABLE toonflow.%I ALTER COLUMN id SET DEFAULT nextval(''toonflow.%I_id_seq''::regclass)',
            target,
            target
        );
    END LOOP;
END
$$;
