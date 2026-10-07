-- 0023: 结构化镜头补充景别与运镜（稳定性计划 P0.2/P0.5）。
-- 两列均可空：旧分镜与既有导出不受影响；值域由应用层枚举校验。
ALTER TABLE toonflow.storyboards
    ADD COLUMN IF NOT EXISTS shot_size TEXT,
    ADD COLUMN IF NOT EXISTS camera_move TEXT;
