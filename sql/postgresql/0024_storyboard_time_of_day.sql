-- 0024: 结构化镜头补充昼夜时间（对齐方案 P1 场景日夜状态）。
-- 可空：旧分镜不受影响；值域由应用层枚举校验（日/夜/晨/黄昏）。
ALTER TABLE toonflow.storyboards
    ADD COLUMN IF NOT EXISTS time_of_day TEXT;
