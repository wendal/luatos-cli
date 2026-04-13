// 智能日志分析器 — 自动识别常见问题模式并给出建议
//
// 在日志流中实时检测已知错误模式（重启、OOM、脚本错误、看门狗超时等），
// 输出诊断建议帮助开发者快速定位问题。

use serde::Serialize;

use crate::{LogEntry, LogLevel};

/// 诊断结果
#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    /// 规则名称
    pub rule: String,
    /// 严重程度
    pub severity: DiagnosticSeverity,
    /// 匹配到的原始日志
    pub matched_message: String,
    /// 诊断建议
    pub suggestion: String,
}

/// 诊断严重程度
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DiagnosticSeverity {
    /// 提示信息（不影响功能）
    Hint,
    /// 警告（可能有问题）
    Warning,
    /// 错误（需要修复）
    Error,
    /// 致命（设备不工作）
    Fatal,
}

impl DiagnosticSeverity {
    pub fn icon(self) -> &'static str {
        match self {
            Self::Hint => "💡",
            Self::Warning => "⚠️",
            Self::Error => "❌",
            Self::Fatal => "🔥",
        }
    }
}

/// 诊断规则
struct DiagnosticRule {
    name: &'static str,
    severity: DiagnosticSeverity,
    suggestion: &'static str,
    /// 匹配函数：输入日志条目，返回是否匹配
    matcher: fn(&LogEntry) -> bool,
}

/// 智能日志分析器
///
/// 维护内部状态，支持流式分析。可检测连续重启、内存不足等需要上下文的模式。
pub struct SmartAnalyzer {
    rules: Vec<DiagnosticRule>,
    /// 检测到的启动次数（用于判断反复重启）
    boot_count: u32,
    /// 分析过的日志条数
    entry_count: u64,
    /// 已产生的诊断
    diagnostics: Vec<Diagnostic>,
    /// 已触发的规则名（防止同一规则重复报告）
    fired_rules: std::collections::HashSet<String>,
}

impl SmartAnalyzer {
    pub fn new() -> Self {
        Self {
            rules: Self::default_rules(),
            boot_count: 0,
            entry_count: 0,
            diagnostics: Vec::new(),
            fired_rules: std::collections::HashSet::new(),
        }
    }

    /// 分析一条日志，返回新产生的诊断（如果有）
    pub fn analyze(&mut self, entry: &LogEntry) -> Vec<Diagnostic> {
        self.entry_count += 1;
        let mut new_diagnostics = Vec::new();

        // 检测启动事件（用于重启检测）
        if is_boot_message(entry) {
            self.boot_count += 1;
            if self.boot_count >= 3 {
                let rule_name = "repeated_reboot".to_string();
                if !self.fired_rules.contains(&rule_name) {
                    self.fired_rules.insert(rule_name.clone());
                    let diag = Diagnostic {
                        rule: rule_name,
                        severity: DiagnosticSeverity::Fatal,
                        matched_message: entry.message.clone(),
                        suggestion: "设备反复重启 (≥3次)！可能原因：看门狗超时、主脚本崩溃、供电不稳。\n  建议：1) 检查 main.lua 是否有死循环或未捕获异常\n        2) 检查供电是否稳定（USB 口供电可能不足）\n        3) 尝试 `flash clear-fs` 清除文件系统后重刷".into(),
                    };
                    new_diagnostics.push(diag);
                }
            }
        }

        // 应用静态规则
        for rule in &self.rules {
            let rule_name = rule.name.to_string();
            if self.fired_rules.contains(&rule_name) {
                continue;
            }
            if (rule.matcher)(entry) {
                self.fired_rules.insert(rule_name);
                let diag = Diagnostic {
                    rule: rule.name.to_string(),
                    severity: rule.severity,
                    matched_message: entry.message.clone(),
                    suggestion: rule.suggestion.to_string(),
                };
                new_diagnostics.push(diag);
            }
        }

        self.diagnostics.extend(new_diagnostics.clone());
        new_diagnostics
    }

    /// 完成分析，返回汇总信息
    pub fn summary(&self) -> SmartSummary {
        SmartSummary {
            entries_analyzed: self.entry_count,
            boot_count: self.boot_count,
            diagnostics: self.diagnostics.clone(),
            errors: self
                .diagnostics
                .iter()
                .filter(|d| matches!(d.severity, DiagnosticSeverity::Error | DiagnosticSeverity::Fatal))
                .count(),
            warnings: self.diagnostics.iter().filter(|d| d.severity == DiagnosticSeverity::Warning).count(),
        }
    }

    fn default_rules() -> Vec<DiagnosticRule> {
        vec![
            // ── 内存相关 ──
            DiagnosticRule {
                name: "out_of_memory",
                severity: DiagnosticSeverity::Error,
                suggestion: "内存不足！建议：\n  1) 减少全局变量和大表\n  2) 使用 `collectgarbage()` 主动回收\n  3) 检查是否有内存泄漏（未关闭的定时器/回调）",
                matcher: |entry| {
                    let msg = entry.message.to_ascii_lowercase();
                    msg.contains("out of memory") || msg.contains("not enough memory") || msg.contains("memory alloc fail")
                },
            },
            DiagnosticRule {
                name: "low_memory",
                severity: DiagnosticSeverity::Warning,
                suggestion: "可用内存偏低，存在 OOM 风险。建议优化内存使用或减少并发任务。",
                matcher: |entry| {
                    let msg = &entry.message;
                    // 匹配 "free mem: xxx" 且数值很低的情况
                    if let Some(pos) = msg.find("free") {
                        let after = &msg[pos..];
                        // 提取数字
                        let num: String = after.chars().filter(|c| c.is_ascii_digit()).take(8).collect();
                        if let Ok(free_bytes) = num.parse::<u64>() {
                            return free_bytes > 0 && free_bytes < 10240; // < 10KB 为低内存
                        }
                    }
                    false
                },
            },
            // ── 脚本错误 ──
            DiagnosticRule {
                name: "lua_error",
                severity: DiagnosticSeverity::Error,
                suggestion: "Lua 脚本运行错误！检查报错位置的代码逻辑。\n  常见原因：nil 访问、类型错误、require 路径不对。\n  建议：用 `luatos-cli project deps` 检查依赖完整性。",
                matcher: |entry| {
                    entry.level == LogLevel::Error && {
                        let msg = &entry.message;
                        msg.contains(".lua:") && (msg.contains("attempt to") || msg.contains("stack traceback") || msg.contains("error"))
                    }
                },
            },
            DiagnosticRule {
                name: "lua_require_fail",
                severity: DiagnosticSeverity::Error,
                suggestion: "模块加载失败！文件可能未刷入设备。\n  建议：1) `luatos-cli project deps --unreachable` 检查遗漏文件\n        2) 确认 script_dirs 配置包含了依赖库目录",
                matcher: |entry| {
                    let msg = &entry.message;
                    msg.contains("module") && msg.contains("not found") || msg.contains("require failed")
                },
            },
            // ── 看门狗 ──
            DiagnosticRule {
                name: "watchdog_timeout",
                severity: DiagnosticSeverity::Fatal,
                suggestion: "看门狗超时触发重启！主循环可能阻塞过久。\n  建议：1) 避免在主任务中执行长时间同步操作\n        2) 确保 sys.wait() / sys.taskInit() 正确使用\n        3) 检查是否有 while true 死循环未 yield",
                matcher: |entry| {
                    let msg = entry.message.to_ascii_lowercase();
                    msg.contains("wdt") || msg.contains("watchdog") || msg.contains("wdg reset")
                },
            },
            // ── 网络相关 ──
            DiagnosticRule {
                name: "network_fail",
                severity: DiagnosticSeverity::Warning,
                suggestion: "网络连接失败。检查：\n  1) SIM 卡是否正确插入且有流量\n  2) 天线是否连接\n  3) APN 配置是否正确",
                matcher: |entry| {
                    let msg = entry.message.to_ascii_lowercase();
                    (msg.contains("pdp") && msg.contains("fail")) ||
                    (msg.contains("network") && msg.contains("error")) ||
                    (msg.contains("sim") && (msg.contains("not insert") || msg.contains("not detect"))) ||
                    msg.contains("no sim")
                },
            },
            DiagnosticRule {
                name: "dns_fail",
                severity: DiagnosticSeverity::Warning,
                suggestion: "DNS 解析失败，无法访问域名。检查网络连接和 DNS 配置。",
                matcher: |entry| {
                    let msg = entry.message.to_ascii_lowercase();
                    msg.contains("dns") && (msg.contains("fail") || msg.contains("error") || msg.contains("timeout"))
                },
            },
            // ── 硬件相关 ──
            DiagnosticRule {
                name: "power_warning",
                severity: DiagnosticSeverity::Warning,
                suggestion: "电源电压异常！可能导致设备不稳定。检查供电电压是否在模组标称范围内。",
                matcher: |entry| {
                    let msg = entry.message.to_ascii_lowercase();
                    (msg.contains("vbat") || msg.contains("power")) && (msg.contains("low") || msg.contains("warning") || msg.contains("under"))
                },
            },
            DiagnosticRule {
                name: "i2c_error",
                severity: DiagnosticSeverity::Warning,
                suggestion: "I2C 通信失败。检查：\n  1) I2C 设备接线和地址\n  2) 上拉电阻是否安装\n  3) 速率设置是否匹配设备规格",
                matcher: |entry| {
                    let msg = entry.message.to_ascii_lowercase();
                    msg.contains("i2c") && (msg.contains("fail") || msg.contains("error") || msg.contains("nak") || msg.contains("nack") || msg.contains("timeout"))
                },
            },
            DiagnosticRule {
                name: "spi_error",
                severity: DiagnosticSeverity::Warning,
                suggestion: "SPI 通信异常。检查接线 (MOSI/MISO/CLK/CS) 和设备兼容性。",
                matcher: |entry| {
                    let msg = entry.message.to_ascii_lowercase();
                    msg.contains("spi") && (msg.contains("fail") || msg.contains("error") || msg.contains("timeout"))
                },
            },
            // ── 文件系统 ──
            DiagnosticRule {
                name: "fs_error",
                severity: DiagnosticSeverity::Error,
                suggestion: "文件系统错误！建议：\n  1) `luatos-cli flash clear-fs` 清除文件系统\n  2) 重新刷入固件和脚本",
                matcher: |entry| {
                    let msg = entry.message.to_ascii_lowercase();
                    (msg.contains("lfs") || msg.contains("littlefs") || msg.contains("filesystem")) &&
                    (msg.contains("error") || msg.contains("corrupt") || msg.contains("mount fail"))
                },
            },
            // ── 启动异常 ──
            DiagnosticRule {
                name: "panic",
                severity: DiagnosticSeverity::Fatal,
                suggestion: "固件发生 panic！这通常是底层固件 bug 或严重资源不足。\n  建议：1) 升级到最新固件版本\n        2) 到 LuatOS 社区反馈并附上完整日志",
                matcher: |entry| {
                    let msg = entry.message.to_ascii_lowercase();
                    msg.contains("panic") || msg.contains("hard fault") || msg.contains("hardfault") || msg.contains("assert fail")
                },
            },
            DiagnosticRule {
                name: "stack_overflow",
                severity: DiagnosticSeverity::Fatal,
                suggestion: "栈溢出！Lua 递归过深或 C 栈空间不足。\n  建议：1) 检查是否有深层递归调用\n        2) 减少局部变量层数\n        3) 检查 sys.taskInit 的栈大小参数",
                matcher: |entry| {
                    let msg = entry.message.to_ascii_lowercase();
                    msg.contains("stack overflow") || msg.contains("stack_overflow")
                },
            },
        ]
    }
}

impl Default for SmartAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

/// 分析汇总
#[derive(Debug, Clone, Serialize)]
pub struct SmartSummary {
    pub entries_analyzed: u64,
    pub boot_count: u32,
    pub diagnostics: Vec<Diagnostic>,
    pub errors: usize,
    pub warnings: usize,
}

/// 判断是否为启动消息
fn is_boot_message(entry: &LogEntry) -> bool {
    let msg = &entry.message;
    msg.contains("LuatOS@") || msg.contains("Powered by LuatOS") || msg.contains("luatos boot") || {
        // Boot log parser 的 module 为 "luat" 或 "bk_init" 等
        entry.module.as_deref().is_some_and(|m| m == "luat" || m == "bk_init") && msg.contains("start")
    }
}

/// 格式化诊断结果为文本
pub fn format_diagnostic(diag: &Diagnostic) -> String {
    format!(
        "{} [{}] {}\n  触发: {}\n  建议: {}",
        diag.severity.icon(),
        diag.rule,
        match diag.severity {
            DiagnosticSeverity::Hint => "提示",
            DiagnosticSeverity::Warning => "警告",
            DiagnosticSeverity::Error => "错误",
            DiagnosticSeverity::Fatal => "致命",
        },
        diag.matched_message,
        diag.suggestion,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LogEntry;

    fn make_entry(level: LogLevel, message: &str) -> LogEntry {
        LogEntry {
            timestamp: "2026-04-13 12:00:00.000".into(),
            device_time: None,
            level,
            module: None,
            message: message.into(),
            raw: message.into(),
        }
    }

    #[test]
    fn detect_out_of_memory() {
        let mut analyzer = SmartAnalyzer::new();
        let entry = make_entry(LogLevel::Error, "lua: out of memory");
        let diags = analyzer.analyze(&entry);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].rule, "out_of_memory");
        assert_eq!(diags[0].severity, DiagnosticSeverity::Error);
    }

    #[test]
    fn detect_lua_error() {
        let mut analyzer = SmartAnalyzer::new();
        let entry = make_entry(LogLevel::Error, "main.lua:15: attempt to index a nil value");
        let diags = analyzer.analyze(&entry);
        assert!(diags.iter().any(|d| d.rule == "lua_error"), "应检测到 Lua 错误");
    }

    #[test]
    fn detect_watchdog() {
        let mut analyzer = SmartAnalyzer::new();
        let entry = make_entry(LogLevel::Error, "WDT reset occurred");
        let diags = analyzer.analyze(&entry);
        assert!(diags.iter().any(|d| d.rule == "watchdog_timeout"));
    }

    #[test]
    fn detect_repeated_reboot() {
        let mut analyzer = SmartAnalyzer::new();
        for i in 0..4 {
            let entry = make_entry(LogLevel::Info, &format!("LuatOS@Air8101 boot #{i}"));
            analyzer.analyze(&entry);
        }
        let summary = analyzer.summary();
        assert!(summary.boot_count >= 4);
        assert!(summary.diagnostics.iter().any(|d| d.rule == "repeated_reboot"), "应检测到反复重启");
    }

    #[test]
    fn detect_network_sim() {
        let mut analyzer = SmartAnalyzer::new();
        let entry = make_entry(LogLevel::Warn, "SIM not insert, check slot");
        let diags = analyzer.analyze(&entry);
        assert!(diags.iter().any(|d| d.rule == "network_fail"));
    }

    #[test]
    fn detect_panic() {
        let mut analyzer = SmartAnalyzer::new();
        let entry = make_entry(LogLevel::Error, "HARD FAULT at 0x00012345");
        let diags = analyzer.analyze(&entry);
        assert!(diags.iter().any(|d| d.rule == "panic"));
    }

    #[test]
    fn no_duplicate_diagnostics() {
        let mut analyzer = SmartAnalyzer::new();
        let entry = make_entry(LogLevel::Error, "lua: out of memory");
        let d1 = analyzer.analyze(&entry);
        let d2 = analyzer.analyze(&entry.clone());
        assert_eq!(d1.len(), 1, "首次应触发");
        assert_eq!(d2.len(), 0, "同一规则不应重复触发");
    }

    #[test]
    fn summary_counts_correct() {
        let mut analyzer = SmartAnalyzer::new();
        analyzer.analyze(&make_entry(LogLevel::Error, "lua: out of memory"));
        analyzer.analyze(&make_entry(LogLevel::Warn, "SIM not insert"));
        analyzer.analyze(&make_entry(LogLevel::Info, "normal log"));
        let summary = analyzer.summary();
        assert_eq!(summary.entries_analyzed, 3);
        assert_eq!(summary.errors, 1);
        assert_eq!(summary.warnings, 1);
    }

    #[test]
    fn format_diagnostic_output() {
        let diag = Diagnostic {
            rule: "test_rule".into(),
            severity: DiagnosticSeverity::Warning,
            matched_message: "test message".into(),
            suggestion: "fix it".into(),
        };
        let text = format_diagnostic(&diag);
        assert!(text.contains("警告"));
        assert!(text.contains("test_rule"));
        assert!(text.contains("fix it"));
    }
}
