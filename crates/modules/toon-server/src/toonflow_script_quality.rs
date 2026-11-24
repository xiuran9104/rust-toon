use std::collections::BTreeSet;

use serde_json::Value;

pub(crate) const POLICY: &str = r#"
## 原著依据与修订规则（优先于模板的数量和爆款指标）
事件表用于定位章节，不替代原文。涉及人物身份、游戏/直播规则、能力边界、交易条件和关键反转时，必须读取对应原文。原著不存在的内容不得以确定事实写入；无法核实时明确标为待核实，不推测补齐。
先确定分集事件、因果、阶段收益，再提炼三幕、人物弧和钩子。允许零个重大反转，不强凑大三角、反派出场或爽点数量。选择反差、计划揭晓、计划兑现不等于人设颠覆；主角隐藏计划不等于改变核心动机。
骨架必须附“原著依据与改编边界”：关键身份、规则、反转对应原著章节和简短依据；创作台词、镜头表达标明改编表达。只能引用本轮实际读取的原文，不把事件摘要说成原文。不得把后续计划写成当前章节已取得的胜利。
审核必须读取当前工作区，不能审核委派提示中的旧副本。每项问题附问题编号、被审核原句、章节证据或具体内部矛盾、最小修订建议，并区分【事实错误】【内部矛盾】【结构风险】【可选建议】。未读原文不能断言违背原著；缺少标签不能推断情节平淡；不得以新增无依据剧情作为修复方案。禁止承诺过审或把文本审核当作成片效果结论。
在同一次最终输出前完成自查，不先提交草稿再等待系统强制二次重写。修订必须保留用户已确认的集数、时长、免费策略和原著范围，只改相关段落并同步受影响的分集、三幕、人物和反转登记；不重写无问题部分。最后检查章节分配、首次出现、伏笔早于揭晓、直播与潜伏因果、阶段成果是否自洽。
"#;

#[derive(Default)]
pub(crate) struct ReadEvidence {
    pub chapters: BTreeSet<i64>,
    pub workspace: bool,
}

impl ReadEvidence {
    pub fn record(&mut self, tool: &str, args: &Value, result: &str) {
        if result.trim().is_empty() || result.starts_with("读取失败") {
            return;
        }
        if tool == "get_novel_text" {
            if let Some(index) = args.get("chapterIndex").and_then(Value::as_i64) {
                self.chapters.insert(index);
            }
        } else if tool == "get_planData" && !result.starts_with("key '") {
            self.workspace = true;
        }
    }

    pub fn missing(&self, has_novel: bool, needs_workspace: bool) -> Option<&'static str> {
        if needs_workspace && !self.workspace {
            Some(
                "提交前必须调用 get_planData 读取本项目当前工作区；审核必须依据最新骨架或改编策略，修订必须保留已有内容。",
            )
        } else if has_novel && self.chapters.is_empty() {
            Some(
                "本轮尚未成功读取任何原文章节。请用 get_novel_text 核实关键身份、规则或反转所在章节；事件表不能替代原文。读取失败时不得伪造核实结果。",
            )
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn summaries_missing_keys_and_failed_reads_do_not_satisfy_evidence_gate() {
        let mut evidence = ReadEvidence::default();
        evidence.record("get_novel_events", &json!({}), "章节摘要");
        evidence.record("get_novel_text", &json!({"chapterIndex":1}), "");
        evidence.record(
            "get_novel_text",
            &json!({"chapterIndex":1}),
            "读取失败：数据库不可用",
        );
        evidence.record("get_planData", &json!({}), "key 'x' 不存在");
        assert!(evidence.missing(true, true).is_some());
        evidence.record("get_planData", &json!({}), "当前骨架");
        assert!(evidence.missing(true, true).is_some());
        evidence.record("get_novel_text", &json!({"chapterIndex":1}), "实际原文");
        assert!(evidence.missing(true, true).is_none());
        assert_eq!(evidence.chapters.len(), 1);
    }

    #[test]
    fn original_projects_do_not_require_nonexistent_novel() {
        assert!(ReadEvidence::default().missing(false, false).is_none());
        assert!(ReadEvidence::default().missing(false, true).is_some());
    }
}
