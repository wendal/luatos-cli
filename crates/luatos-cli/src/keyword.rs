//! 关键字判定的共享纯函数。
//!
//! 两条链路要做同构的「pass 关键字须全部命中、fail 关键字一命中即快速失败」判定，
//! 但抓取到的数据源不同：
//!
//! - `flash test`（`cmd_flash`）抓的是裸文本行 `Vec<String>`，命中方式为
//!   `all_lines.iter().any(|line| line.contains(kw))`；
//! - `trun`（`cmd_trun`）抓的是结构化 `LogEntry`，命中方式由 `MatchField` 决定，
//!   走 `match_keyword(&all_entries, kw, match_field)`。
//!
//! 本模块不接触串口、日志与任何 IO：调用方用闭包 `hit: Fn(&str) -> bool` 决定「怎么算
//! 命中」，本模块只按输入顺序逐一询问并汇总结果，从而把判定规则与数据源解耦。
//! 两个命令拿到同一份 `KeywordOutcome` 后，再各自组装成自己的输出结构。

/// 一次关键字判定的完整结果。
///
/// 所有 `Vec` 的元素顺序都严格跟随对应的输入关键字顺序。`all_passed` 是最终判定：
/// 所有 pass 关键字命中 **且** 未 `fast_failed`；空 pass 列表视为全通过。
/// `pass_results` / `missing_keywords` 只反映 pass 关键字自身的命中情况，与 fail 关键字无关。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeywordOutcome {
    /// pass 关键字逐条 `(关键字, 是否命中)`，顺序与 `pass_keywords` 一致。
    pub pass_results: Vec<(String, bool)>,
    /// 未命中的 pass 关键字，顺序与 `pass_keywords` 一致。
    pub missing_keywords: Vec<String>,
    /// fail 关键字逐条 `(关键字, 是否命中)`，顺序与 `fail_keywords` 一致。
    pub fail_results: Vec<(String, bool)>,
    /// 命中的 fail 关键字，顺序与 `fail_keywords` 一致。
    pub matched_fail_keywords: Vec<String>,
    /// 是否有任一 fail 关键字命中。
    pub fast_failed: bool,
    /// 所有 pass 关键字命中且未 `fast_failed`（空 pass 列表视为全通过）。
    pub all_passed: bool,
}

/// 按输入顺序评估 pass / fail 关键字，汇总为 `KeywordOutcome`。
///
/// `hit` 由调用方提供：传入关键字 `&str`，返回它在当前数据源中是否命中。每个 pass
/// 关键字调用一次、每个 fail 关键字调用一次，调用顺序与切片顺序一致，因此结果里的
/// 各个 `Vec` 也严格保持输入顺序。
///
/// - `fast_failed` = `matched_fail_keywords` 非空；
/// - `all_passed` = `missing_keywords` 为空且未 `fast_failed`；空 `pass_keywords`
///   时 `missing_keywords` 必为空，故视为全通过。
pub fn evaluate_outcome<F>(pass_keywords: &[String], fail_keywords: &[String], hit: F) -> KeywordOutcome
where
    F: Fn(&str) -> bool,
{
    let mut pass_results = Vec::with_capacity(pass_keywords.len());
    let mut missing_keywords = Vec::new();
    for kw in pass_keywords {
        let found = hit(kw.as_str());
        pass_results.push((kw.clone(), found));
        if !found {
            missing_keywords.push(kw.clone());
        }
    }

    let mut fail_results = Vec::with_capacity(fail_keywords.len());
    let mut matched_fail_keywords = Vec::new();
    for kw in fail_keywords {
        let found = hit(kw.as_str());
        fail_results.push((kw.clone(), found));
        if found {
            matched_fail_keywords.push(kw.clone());
        }
    }

    let fast_failed = !matched_fail_keywords.is_empty();
    let all_passed = missing_keywords.is_empty() && !fast_failed;
    KeywordOutcome {
        pass_results,
        missing_keywords,
        fail_results,
        matched_fail_keywords,
        fast_failed,
        all_passed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_pass_keywords_hit_passes() {
        let lines = ["Boot OK", "LuatOS@ READY"];
        let pass = vec!["LuatOS@".to_string(), "READY".to_string()];

        let outcome = evaluate_outcome(&pass, &[], |kw| lines.iter().any(|line| line.contains(kw)));
        assert!(outcome.all_passed);
        assert!(!outcome.fast_failed);
        assert!(outcome.missing_keywords.is_empty());
        assert!(outcome.matched_fail_keywords.is_empty());
        assert_eq!(outcome.pass_results, vec![("LuatOS@".to_string(), true), ("READY".to_string(), true)]);
        assert!(outcome.fail_results.is_empty());
    }

    #[test]
    fn missing_pass_keyword_fails() {
        let lines = ["Boot OK", "READY token"];
        let pass = vec!["READY".to_string(), "LuatOS@".to_string()];

        let outcome = evaluate_outcome(&pass, &[], |kw| lines.iter().any(|line| line.contains(kw)));
        assert!(!outcome.all_passed);
        assert!(!outcome.fast_failed);
        assert_eq!(outcome.missing_keywords, vec!["LuatOS@".to_string()]);
        assert_eq!(outcome.pass_results, vec![("READY".to_string(), true), ("LuatOS@".to_string(), false)]);
    }

    #[test]
    fn matched_fail_keyword_fast_fails() {
        let lines = ["panic: assert failed", "LuatOS@ ready"];
        let pass = vec!["LuatOS@".to_string()];
        let fail = vec!["panic".to_string(), "hardfault".to_string()];

        let outcome = evaluate_outcome(&pass, &fail, |kw| lines.iter().any(|line| line.contains(kw)));
        assert!(outcome.fast_failed);
        assert!(!outcome.all_passed);
        assert_eq!(outcome.matched_fail_keywords, vec!["panic".to_string()]);
        assert_eq!(outcome.fail_results, vec![("panic".to_string(), true), ("hardfault".to_string(), false)]);
        // pass 关键字即便全部命中，也不能抵消一次 fail 命中
        assert!(outcome.pass_results.iter().all(|(_, found)| *found));
        assert!(outcome.missing_keywords.is_empty());
    }

    #[test]
    fn empty_keyword_lists_pass_by_default() {
        let lines = ["anything"];

        let outcome = evaluate_outcome(&[], &[], |kw| lines.iter().any(|line| line.contains(kw)));
        assert!(outcome.all_passed);
        assert!(!outcome.fast_failed);
        assert!(outcome.pass_results.is_empty());
        assert!(outcome.missing_keywords.is_empty());
        assert!(outcome.fail_results.is_empty());
        assert!(outcome.matched_fail_keywords.is_empty());

        // pass 为空但 fail 命中：仍须快速失败，空 pass 不能把结果抬成通过
        let fail = ["anything".to_string()];
        let outcome = evaluate_outcome(&[], &fail, |kw| lines.iter().any(|line| line.contains(kw)));
        assert!(outcome.fast_failed);
        assert!(!outcome.all_passed);
    }

    #[test]
    fn result_order_follows_input_keyword_order() {
        let lines = ["alpha beta gamma"];
        let pass = vec!["gamma".to_string(), "alpha".to_string(), "missing".to_string(), "beta".to_string()];
        let fail = vec!["beta".to_string(), "nope".to_string(), "gamma".to_string()];

        let outcome = evaluate_outcome(&pass, &fail, |kw| lines.iter().any(|line| line.contains(kw)));
        assert_eq!(
            outcome.pass_results,
            vec![
                ("gamma".to_string(), true),
                ("alpha".to_string(), true),
                ("missing".to_string(), false),
                ("beta".to_string(), true)
            ]
        );
        assert_eq!(outcome.missing_keywords, vec!["missing".to_string()]);
        assert_eq!(
            outcome.fail_results,
            vec![("beta".to_string(), true), ("nope".to_string(), false), ("gamma".to_string(), true)]
        );
        assert_eq!(outcome.matched_fail_keywords, vec!["beta".to_string(), "gamma".to_string()]);
    }
}
