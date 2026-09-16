//! printf 格式串的**规格解析**与**渲染**（供各日志解码器共用）。
//!
//! C 侧固件把日志以「格式串 + 原始参数字节」的形式发出来，格式串本身不含任何
//! 已格式化的值，因此解码器必须自己实现 printf 的格式化语义。本模块负责其中
//! 与「参数在流中的布局」无关的部分：
//!
//! * [`parse`] —— 解析 `%[flags][width][.prec][length]conv`
//! * [`narrow`] —— 按变参提升规则把原始槽值收窄到 `length` 指定的宽度
//! * [`render_int`] / [`render_float`] / [`render_str`] / [`render_char`] / [`render_ptr`]
//!   —— 按规格渲染出最终文本
//!
//! 参数布局（各链路不同：NUL 结尾串 / 长度前缀串、4 或 8 字节整数槽、`*` 的取参
//! 顺序等）由调用方处理。调用方解析出 [`FmtSpec`] 后，先从参数流取出 `*` 对应的
//! 宽度/精度并写回 [`FmtSpec::width`] / [`FmtSpec::prec`]，再取出主参数交给渲染函数。
//!
//! 已实现的 C 语义：
//!
//! | 特性 | 说明 |
//! |------|------|
//! | flags | `-` 左对齐、`+`/` ` 符号、`#` 进制前缀、`0` 零填充 |
//! | width | 数字，或 `*`（调用方从参数取值；负值等价 `-` + 绝对值） |
//! | precision | `.数字` 或 `.*`；整数为最小位数（`.0` + 值 0 ⇒ 空串），浮点/串为位数/字符数 |
//! | length | `hh` `h` `l` `ll` `z` `j` `t` `L`（32 位 ARM：`long`/`size_t` 均为 4 字节） |
//! | int | `d` `i` `u` `o` `x` `X`，含 `#` 前缀与精度补零 |
//! | float | `f` `F` `e` `E` `g` `G` `a` `A`，默认精度 6（`a` 默认 13 位十六进制） |
//! | 其他 | `c` `s` `p` `%`；`n` 无输出 |

use std::iter::Peekable;
use std::str::Chars;

/// `length` 修饰符。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Length {
    #[default]
    None,
    /// `hh` —— 按 `char`/`signed char` 解释（8 位）
    Hh,
    /// `h` —— 按 `short` 解释（16 位）
    H,
    /// `l` —— 32 位 ARM 上 `long` 为 4 字节
    L,
    /// `ll` —— `long long`，8 字节
    Ll,
    /// `z` —— `size_t`，4 字节
    Z,
    /// `j` —— `intmax_t`，4 字节
    J,
    /// `t` —— `ptrdiff_t`，4 字节
    T,
    /// `L` —— `long double`（本平台与 `double` 同为 8 字节）
    BigL,
}

/// 一个完整的转换规格：`%` 之后、转换字符（含）为止。
#[derive(Debug, Clone, Default)]
pub(crate) struct FmtSpec {
    /// `-` 左对齐
    pub minus: bool,
    /// `+` 总是打印符号
    pub plus: bool,
    /// ` ` 非负值前打印空格
    pub space: bool,
    /// `#` 备用形式（`0x`/`0X` 前缀、`o` 补前导 0、浮点保留小数点）
    pub hash: bool,
    /// `0` 零填充
    pub zero: bool,
    /// 字段宽度（`*` 由调用方取值后写回此处；负值已在写回时规范化为 `minus` + 绝对值）
    pub width: Option<usize>,
    /// 宽度是否来自 `*`
    pub width_star: bool,
    /// 精度（`.数字`）；`.*` 由调用方取值后写回此处
    pub prec: Option<usize>,
    /// 精度是否来自 `*`
    pub prec_star: bool,
    /// 长度修饰符
    pub length: Length,
    /// 转换字符
    pub conv: char,
}

/// [`parse`] 的结果。
#[derive(Debug, Clone)]
pub(crate) enum Parsed {
    /// `%%` —— 输出一个字面 `%`
    Percent,
    /// 完整规格
    Spec(FmtSpec),
}

/// 解析 `%` 之后的格式规格（调用方已消费 `%`）。
///
/// 返回 `None` 表示这不是一个可识别的转换（格式串可能是普通文本，或使用了本模块
/// 未支持的扩展），调用方应自行决定降级策略（如原样输出）。
pub(crate) fn parse(chars: &mut Peekable<Chars<'_>>) -> Option<Parsed> {
    let mut sp = FmtSpec::default();

    // flags —— 可重复、顺序任意；`0` 属于 flags（而非宽度数字）由 C 标准规定
    loop {
        match chars.peek().copied() {
            Some('-') => sp.minus = true,
            Some('+') => sp.plus = true,
            Some(' ') => sp.space = true,
            Some('#') => sp.hash = true,
            Some('0') => sp.zero = true,
            _ => break,
        }
        chars.next();
    }

    // width —— 数字 或 `*`
    if chars.peek() == Some(&'*') {
        chars.next();
        sp.width_star = true;
    } else {
        let mut w: Option<usize> = None;
        while let Some(&c) = chars.peek() {
            if !c.is_ascii_digit() {
                break;
            }
            chars.next();
            w = Some(
                w.unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(c as usize - '0' as usize),
            );
        }
        sp.width = w;
    }

    // precision —— `.` 后跟数字 或 `*`；单独一个 `.` 表示精度 0
    if chars.peek() == Some(&'.') {
        chars.next();
        if chars.peek() == Some(&'*') {
            chars.next();
            sp.prec_star = true;
        } else {
            let mut p = 0usize;
            while let Some(&c) = chars.peek() {
                if !c.is_ascii_digit() {
                    break;
                }
                chars.next();
                p = p.saturating_mul(10).saturating_add(c as usize - '0' as usize);
            }
            sp.prec = Some(p);
        }
    }

    // length —— 最长匹配
    sp.length = match chars.peek().copied() {
        Some('h') => {
            chars.next();
            if chars.peek() == Some(&'h') {
                chars.next();
                Length::Hh
            } else {
                Length::H
            }
        }
        Some('l') => {
            chars.next();
            if chars.peek() == Some(&'l') {
                chars.next();
                Length::Ll
            } else {
                Length::L
            }
        }
        Some('z') => {
            chars.next();
            Length::Z
        }
        Some('j') => {
            chars.next();
            Length::J
        }
        Some('t') => {
            chars.next();
            Length::T
        }
        Some('L') => {
            chars.next();
            Length::BigL
        }
        _ => Length::None,
    };

    let conv = chars.next()?;
    match conv {
        'd' | 'i' | 'u' | 'o' | 'x' | 'X' | 'f' | 'F' | 'e' | 'E' | 'g' | 'G' | 'a' | 'A' | 'c'
        | 's' | 'S' | 'p' | 'n' => {
            sp.conv = conv;
            Some(Parsed::Spec(sp))
        }
        '%' => Some(Parsed::Percent),
        _ => None,
    }
}

/// 该 `length` 是否要求 8 字节参数槽。
///
/// 32 位 ARM 上 `long`/`size_t`/`intmax_t`/`ptrdiff_t` 均为 4 字节，
/// 只有 `long long` 与 `long double` 占 8 字节。
pub(crate) fn needs_wide_slot(length: Length) -> bool {
    matches!(length, Length::Ll | Length::BigL)
}

/// 无符号/有符号整数值。
#[derive(Debug, Clone, Copy)]
pub(crate) enum IntVal {
    Signed(i64),
    Unsigned(u64),
}

impl IntVal {
    fn as_u64(self) -> u64 {
        match self {
            IntVal::Signed(v) => v as u64,
            IntVal::Unsigned(v) => v,
        }
    }

    fn is_zero(self) -> bool {
        self.as_u64() == 0
    }
}

/// 按 C 的变参提升规则，把原始槽值收窄到 `length` 对应的宽度。
///
/// `raw` 为参数流中读出的原始槽值（`needs_wide_slot` 为真时是完整的 8 字节，
/// 否则只有低 4 字节有效）。
pub(crate) fn narrow(raw: u64, length: Length, signed: bool) -> IntVal {
    if signed {
        let v = match length {
            Length::Hh => raw as u8 as i8 as i64,
            Length::H => raw as u16 as i16 as i64,
            Length::Ll => raw as i64,
            Length::None | Length::L | Length::Z | Length::J | Length::T | Length::BigL => {
                raw as u32 as i32 as i64
            }
        };
        IntVal::Signed(v)
    } else {
        let v = match length {
            Length::Hh => raw as u8 as u64,
            Length::H => raw as u16 as u64,
            Length::Ll => raw,
            Length::None | Length::L | Length::Z | Length::J | Length::T | Length::BigL => {
                raw as u32 as u64
            }
        };
        IntVal::Unsigned(v)
    }
}

/// 按 `spec` 渲染一个整数（`d i u o x X`）。
pub(crate) fn render_int(spec: &FmtSpec, v: IntVal) -> String {
    let negative = matches!(v, IntVal::Signed(x) if x < 0);
    let mag = match v {
        IntVal::Signed(x) => x.unsigned_abs(),
        IntVal::Unsigned(x) => x,
    };

    let (mut digits, mut prefix) = match spec.conv {
        'd' | 'i' | 'u' => (mag.to_string(), String::new()),
        'x' => (format!("{mag:x}"), String::new()),
        'X' => (format!("{mag:X}"), String::new()),
        'o' => (format!("{mag:o}"), String::new()),
        _ => (mag.to_string(), String::new()),
    };

    if spec.hash {
        match spec.conv {
            'x' if mag != 0 => prefix = "0x".to_string(),
            'X' if mag != 0 => prefix = "0X".to_string(),
            'o' if !digits.starts_with('0') => prefix = "0".to_string(),
            _ => {}
        }
    }

    // 精度：整数表示最小位数；`.0` + 值 0 ⇒ 空串
    if let Some(p) = spec.prec {
        if p == 0 && v.is_zero() {
            digits.clear();
        } else if digits.len() < p {
            digits = format!("{}{digits}", "0".repeat(p - digits.len()));
        }
    }

    let sign = if negative {
        "-"
    } else if spec.plus {
        "+"
    } else if spec.space {
        " "
    } else {
        ""
    };

    let head = sign.len() + prefix.len();
    let body = format!("{sign}{prefix}{digits}");
    pad_with(spec, body, head, ZeroMode::IntLike)
}

/// 按 `spec` 渲染一个浮点数（`f F e E g G a A`）。
pub(crate) fn render_float(spec: &FmtSpec, v: f64) -> String {
    let upper = spec.conv.is_ascii_uppercase();
    let prec = spec.prec;

    let body = if v.is_nan() {
        if upper { "NAN".to_string() } else { "nan".to_string() }
    } else if v.is_infinite() {
        if upper { "INF".to_string() } else { "inf".to_string() }
    } else {
        let a = v.abs();
        let s = match spec.conv.to_ascii_lowercase() {
            'f' => fmt_fixed(a, prec.unwrap_or(6), spec.hash),
            'e' => fmt_exp(a, prec.unwrap_or(6), spec.hash),
            'g' => fmt_general(a, prec.unwrap_or(6), spec.hash),
            'a' => fmt_hex_float(a, prec, spec.hash),
            _ => fmt_fixed(a, prec.unwrap_or(6), spec.hash),
        };
        if upper { s.to_uppercase() } else { s }
    };

    // NaN 不带符号（除非显式要求）；其余按符号位 + `+`/` ` 标志
    let sign = if v.is_sign_negative() && !v.is_nan() {
        "-"
    } else if spec.plus {
        "+"
    } else if spec.space {
        " "
    } else {
        ""
    };

    let body = format!("{sign}{body}");
    pad_with(spec, body, sign.len(), ZeroMode::FloatLike)
}

/// 按 `spec` 渲染一个字符串（`s`）—— 精度限制最大字符数，零填充标志对 `%s` 无效。
pub(crate) fn render_str(spec: &FmtSpec, s: &str) -> String {
    let text: String = match spec.prec {
        Some(p) => s.chars().take(p).collect(),
        None => s.to_string(),
    };
    pad_with(spec, text, 0, ZeroMode::Off)
}

/// 按 `spec` 渲染一个字符（`c`）。
pub(crate) fn render_char(spec: &FmtSpec, c: char) -> String {
    pad_with(spec, c.to_string(), 0, ZeroMode::Off)
}

/// 按 `spec` 渲染一个指针（`p`）—— `0x` + 小写十六进制，不补前导零。
pub(crate) fn render_ptr(spec: &FmtSpec, raw: u64) -> String {
    pad_with(spec, format!("0x{raw:x}"), 0, ZeroMode::Off)
}

/// 零填充策略 —— C 对不同类型的 `0` 标志语义不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ZeroMode {
    /// 不使用零填充（`s` `c` `p`）
    Off,
    /// 整数：指定精度时忽略 `0` 标志（C11 7.21.6.1p6）
    IntLike,
    /// 浮点：`0` 标志始终有效（精度不是"最小位数"的概念）
    FloatLike,
}

impl ZeroMode {
    fn fills(self, spec: &FmtSpec) -> bool {
        match self {
            ZeroMode::Off => false,
            ZeroMode::IntLike => spec.zero && spec.prec.is_none(),
            ZeroMode::FloatLike => spec.zero,
        }
    }
}

/// 通用字段宽度填充。
///
/// `zero_at` 是零填充的插入位置（字节偏移，位于符号/进制前缀之后）；
/// `zero_mode` 决定是否允许零填充 —— C 规定 `0` 标志对整数在指定精度时被忽略，
/// 但对浮点始终有效，而 `%s`/`%c`/`%p` 根本不用零填充。
fn pad_with(spec: &FmtSpec, body: String, zero_at: usize, zero_mode: ZeroMode) -> String {
    let width = spec.width.unwrap_or(0);
    let len = body.chars().count();
    if len >= width {
        return body;
    }
    let fill = width - len;

    if spec.minus {
        let mut s = String::with_capacity(body.len() + fill);
        s.push_str(&body);
        s.push_str(&" ".repeat(fill));
        s
    } else if zero_mode.fills(spec) {
        let mut s = String::with_capacity(body.len() + fill);
        s.push_str(&body[..zero_at.min(body.len())]);
        s.push_str(&"0".repeat(fill));
        s.push_str(&body[zero_at.min(body.len())..]);
        s
    } else {
        let mut s = String::with_capacity(body.len() + fill);
        s.push_str(&" ".repeat(fill));
        s.push_str(&body);
        s
    }
}

/// `%f` —— 定点表示。
fn fmt_fixed(v: f64, prec: usize, hash: bool) -> String {
    let mut s = format!("{v:.prec$}");
    if hash && !s.contains('.') {
        s.push('.');
    }
    s
}

/// `%e` —— 科学计数法，指数至少两位并带符号（与 C 一致）。
fn fmt_exp(v: f64, prec: usize, hash: bool) -> String {
    let raw = format!("{v:.prec$e}");
    let (mant, exp) = match raw.split_once('e') {
        Some((m, e)) => (m.to_string(), e.parse::<i32>().unwrap_or(0)),
        None => (raw, 0),
    };
    let mut mant = mant;
    if hash && !mant.contains('.') {
        mant.push('.');
    }
    format!("{mant}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
}

/// `%g` —— 按指数大小在 `%e` / `%f` 间选择，并去掉尾随零（除 `#`）。
fn fmt_general(v: f64, prec: usize, hash: bool) -> String {
    let p = prec.max(1);
    let exp10 = if v == 0.0 { 0 } else { decimal_exponent(v) };

    if exp10 < -4 || exp10 >= p as i32 {
        let mut s = fmt_exp(v, p - 1, hash);
        if !hash {
            trim_zeros(&mut s, true);
        }
        s
    } else {
        let frac = (p as i32 - 1 - exp10).max(0) as usize;
        let mut s = fmt_fixed(v, frac, hash);
        if !hash {
            trim_zeros(&mut s, false);
        }
        s
    }
}

/// `%a` —— 十六进制浮点，形如 `0x1.921fb54442d18p+1`。
///
/// 不指定精度时输出全部有效十六进制位并去掉尾随零；本平台 `long double` 与
/// `double` 同为 8 字节，因此按 IEEE-754 双精度拆分。
fn fmt_hex_float(v: f64, prec: Option<usize>, hash: bool) -> String {
    if v == 0.0 {
        let mut s = String::from("0x0");
        match prec {
            Some(p) if p > 0 => {
                s.push('.');
                s.push_str(&"0".repeat(p));
            }
            Some(_) => {}
            None if hash => s.push('.'),
            None => {}
        }
        s.push_str("p+0");
        return s;
    }

    let bits = v.to_bits();
    let raw_exp = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & 0x000f_ffff_ffff_ffff;

    // 规格化数前导位为 1、真实指数 = raw_exp - 1023；非规格化数为 0、指数固定 -1022
    let (lead, exp) = if raw_exp == 0 {
        (0, -1022)
    } else {
        (1, raw_exp - 1023)
    };

    let mut digits = format!("{frac:013x}");
    match prec {
        Some(p) => {
            digits.truncate(p);
            while digits.len() < p {
                digits.push('0');
            }
        }
        None => {
            while digits.ends_with('0') {
                digits.pop();
            }
        }
    }

    let mut s = format!("0x{lead}");
    if !digits.is_empty() || hash {
        s.push('.');
        s.push_str(&digits);
    }
    s.push_str(&format!("p{}{}", if exp < 0 { '-' } else { '+' }, exp.abs()));
    s
}

/// 用最短表示求出 `v`（正数）的十进制指数，避免 `log10()` 的边界误差。
fn decimal_exponent(v: f64) -> i32 {
    let s = format!("{v:e}");
    s.split_once('e')
        .and_then(|(_, e)| e.parse::<i32>().ok())
        .unwrap_or(0)
}

/// 去掉尾随零，必要时连小数点一起去掉。`in_exp` 表示 `s` 形如 `mant e±XX`。
fn trim_zeros(s: &mut String, in_exp: bool) {
    if in_exp {
        let Some((mant, exp)) = s.split_once('e') else {
            return;
        };
        let mut m = mant.to_string();
        if m.contains('.') {
            while m.ends_with('0') {
                m.pop();
            }
            if m.ends_with('.') {
                m.pop();
            }
        }
        *s = format!("{m}e{exp}");
    } else if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
}
