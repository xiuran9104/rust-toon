-- 0025: 对齐方案 P2 穿越剧场物件连续性。分镜声明随身物件
-- （[{name, era, description}]），生成时沿轨道向前继承并注入提示词，
-- 保证穿越前后物件不凭空消失或换年代。可空数组为默认。
ALTER TABLE toonflow.storyboards
    ADD COLUMN IF NOT EXISTS carried_objects jsonb DEFAULT '[]'::jsonb NOT NULL;
