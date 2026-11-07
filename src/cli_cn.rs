// 中文命令前端：把连写的中文短语翻译成英文参数，交给同一个二进制里的 clap 解析器。
//
// 中文不像英文靠空格分词，因此解析时先去掉所有空白，再按中文短语切出命令名、
// 数量、时长、所属与位置参数。翻译结果与英文命令走同一条执行路径——
// 不再 fork 出第二个进程，也不需要在 PATH 里找另一个二进制。
//
// 中文命令按中文语序书写，并且**连写、不加空格**：
//
//   创建邀请码可用三次有效期七天
//   创建用户爱丽丝设为管理员
//   重置爱丽丝的通行密钥
//
// / Chinese front-end: translates concatenated Chinese phrases into English
// arguments for the very same clap parser. No second process, no sibling binary.

use std::io::{self, IsTerminal};
use std::sync::atomic::{AtomicBool, Ordering};

pub const NAME: &str = "kych-net";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

static COLOR: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------- 命令定义

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Serve,
    Build,
    Migrate,
    InviteCreate,
    InviteList,
    InviteRevoke,
    UserCreate,
    UserCreateAdmin,
    UserList,
    UserDisable,
    UserEnable,
    PasskeyReset,
}

pub struct Cmd {
    pub kind: Kind,
    pub cn: &'static str,
    pub aliases: &'static [&'static str],
    pub en: &'static str,
    pub group: &'static str,
    pub usage: &'static str,
    pub desc: &'static str,
}

/// 与 `main.rs` 里的 clap 子命令一一对应（含 README 未列出的 `user enable`）。
/// 命令名与修饰语全是连写的中文短语，不用空格、不用 `--英文选项`。
const CMDS: &[Cmd] = &[
    Cmd {
        kind: Kind::Serve,
        cn: "启动服务",
        aliases: &["运行服务", "开启服务", "把服务跑起来"],
        en: "serve",
        group: "服务与构建",
        usage: "启动服务",
        desc: "启动 HTTP API 服务",
    },
    Cmd {
        kind: Kind::Build,
        cn: "生成静态站",
        aliases: &["构建静态站", "生成站点", "构建站点"],
        en: "build",
        group: "服务与构建",
        usage: "生成静态站到<目录>",
        desc: "生成静态站点，默认到 dist",
    },
    Cmd {
        kind: Kind::Migrate,
        cn: "执行数据库迁移",
        aliases: &["迁移数据库", "数据库迁移", "执行迁移"],
        en: "db migrate",
        group: "数据库",
        usage: "执行数据库迁移",
        desc: "执行数据库迁移",
    },
    Cmd {
        kind: Kind::InviteCreate,
        cn: "创建邀请码",
        aliases: &["新建邀请码", "生成邀请码", "发一个邀请码"],
        en: "invite create",
        group: "邀请码",
        usage: "创建邀请码可用<次数>次有效期<时长>",
        desc: "创建邀请码并输出码值，默认可用 1 次、有效期 7d",
    },
    Cmd {
        kind: Kind::InviteList,
        cn: "列出邀请码",
        aliases: &["查看邀请码", "看看邀请码", "所有邀请码"],
        en: "invite list",
        group: "邀请码",
        usage: "列出邀请码",
        desc: "列出全部邀请码",
    },
    Cmd {
        kind: Kind::InviteRevoke,
        cn: "作废邀请码",
        aliases: &["撤销邀请码", "废掉邀请码"],
        en: "invite revoke",
        group: "邀请码",
        usage: "作废邀请码<邀请码>",
        desc: "作废指定邀请码",
    },
    Cmd {
        kind: Kind::UserCreate,
        cn: "创建用户",
        aliases: &["新建用户", "添加用户"],
        en: "user create",
        group: "用户",
        usage: "创建用户<用户名>设为管理员",
        desc: "创建用户并输出 TOTP 密钥（用户名支持中文）",
    },
    Cmd {
        kind: Kind::UserCreateAdmin,
        cn: "创建管理员",
        aliases: &["新建管理员", "添加管理员"],
        en: "user create",
        group: "用户",
        usage: "创建管理员<用户名>",
        desc: "创建管理员（把「管理员」前置作定语的中文写法）",
    },
    Cmd {
        kind: Kind::UserList,
        cn: "列出用户",
        aliases: &["查看用户", "所有用户"],
        en: "user list",
        group: "用户",
        usage: "列出用户",
        desc: "列出全部用户",
    },
    Cmd {
        kind: Kind::UserDisable,
        cn: "停用用户",
        aliases: &["禁用用户", "停掉用户"],
        en: "user disable",
        group: "用户",
        usage: "停用用户<用户名>",
        desc: "停用指定用户",
    },
    Cmd {
        kind: Kind::UserEnable,
        cn: "启用用户",
        aliases: &["解禁用户", "恢复用户"],
        en: "user enable",
        group: "用户",
        usage: "启用用户<用户名>",
        desc: "重新启用指定用户",
    },
    Cmd {
        kind: Kind::PasskeyReset,
        cn: "重置",
        aliases: &["清除"],
        en: "passkey reset",
        group: "通行密钥",
        usage: "重置<用户名>的通行密钥",
        desc: "清除某用户全部通行密钥",
    },
];

fn cmd_names() -> Vec<&'static str> {
    let mut v = Vec::new();
    for c in CMDS {
        v.push(c.cn);
        v.extend_from_slice(c.aliases);
    }
    v
}

/// 命令名是连写的中文短语，按最长前缀匹配。
fn match_command(line: &str) -> Option<(&'static Cmd, usize)> {
    let mut best: Option<(&'static Cmd, usize)> = None;
    for c in CMDS {
        let mut names = vec![c.cn];
        names.extend_from_slice(c.aliases);
        for name in names {
            if line.starts_with(name) && best.map_or(true, |(_, n)| name.len() > n) {
                best = Some((c, name.len()));
            }
        }
    }
    best
}

fn is_english_command(s: &str) -> bool {
    matches!(s, "serve" | "build" | "db" | "invite" | "user" | "passkey")
}

/// 参数里出现非 ASCII 字符就当作中文命令处理。
///
/// 英文命令也可能带中文用户名（`user create 爱丽丝`），这种情况交给下面的
/// 英文透传分支处理，行为与直接敲英文完全一致。
pub fn has_cjk(args: &[String]) -> bool {
    args.iter().any(|a| a.chars().any(|c| !c.is_ascii()))
}

// ---------------------------------------------------------------- 中文数量与时长

fn parse_count(s: &str) -> Option<i32> {
    if let Ok(n) = s.parse::<i32>() {
        return (n > 0).then_some(n);
    }
    Some(match s {
        "一" => 1,
        "二" | "两" => 2,
        "三" => 3,
        "四" => 4,
        "五" => 5,
        "六" => 6,
        "七" => 7,
        "八" => 8,
        "九" => 9,
        "十" => 10,
        _ => return None,
    })
}

fn normalize_expiry(s: &str) -> Result<String, String> {
    if matches!(s, "永不过期" | "永久" | "never") {
        return Ok("never".into());
    }
    if let Some(n) = s.strip_suffix("天").and_then(parse_count) {
        return Ok(format!("{}d", n));
    }
    if let Some(n) = s.strip_suffix("小时").and_then(parse_count) {
        return Ok(format!("{}h", n));
    }
    if s.is_empty() {
        return Err("「有效期」后面缺少时长，例如「七天」或「24h」。".into());
    }
    Ok(s.to_string())
}

// ---------------------------------------------------------------- 连写短语切分

fn keys_for(k: Kind) -> &'static [&'static str] {
    match k {
        Kind::Build => &["输出到", "保存到", "到"],
        Kind::InviteCreate => &["可用", "有效期"],
        Kind::UserCreate | Kind::UserCreateAdmin => &[
            "设为管理员",
            "并设为管理员",
            "作为管理员",
            "授予管理员权限",
        ],
        Kind::PasskeyReset => &["的通行密钥", "的密钥"],
        _ => &[],
    }
}

fn is_flag_key(k: &str) -> bool {
    matches!(
        k,
        "设为管理员" | "并设为管理员" | "作为管理员" | "授予管理员权限" | "的通行密钥" | "的密钥"
    )
}

/// 找出最靠前的中文短语关键词，同位置时取更长的那个。
fn cut_first<'a>(s: &'a str, keys: &[&'static str]) -> Option<(String, &'static str, &'a str)> {
    let mut best: Option<(usize, &'static str)> = None;
    for &k in keys {
        if let Some(pos) = s.find(k) {
            if best.map_or(true, |(p, bk)| pos < p || (pos == p && k.len() > bk.len())) {
                best = Some((pos, k));
            }
        }
    }
    let (pos, k) = best?;
    Some((s[..pos].to_string(), k, &s[pos + k.len()..]))
}

/// 关键词后面的值，到下一个关键词为止。
fn split_value<'a>(s: &'a str, keys: &[&'static str]) -> (&'a str, &'a str) {
    let mut end = s.len();
    for &k in keys {
        if let Some(p) = s.find(k) {
            if p < end {
                end = p;
            }
        }
    }
    (&s[..end], &s[end..])
}

// ---------------------------------------------------------------- 中文语序解析

fn parse_cn(cmd: &'static Cmd, rest: &str) -> Result<Vec<String>, String> {
    let keys = keys_for(cmd.kind);
    let mut s = rest.to_string();
    let mut positional = String::new();
    let mut options: Vec<String> = Vec::new();
    let mut uses = "1".to_string();
    let mut expires = "7d".to_string();
    let mut out = "dist".to_string();
    let mut admin = cmd.kind == Kind::UserCreateAdmin;
    let mut has_de = false;

    loop {
        let (before, key, after) = match cut_first(&s, keys) {
            Some(v) => v,
            None => {
                positional.push_str(&s);
                break;
            }
        };
        positional.push_str(&before);
        let (value, tail) = split_value(after, keys);
        let value = value.to_string();
        let next = if is_flag_key(key) { after.to_string() } else { tail.to_string() };
        match key {
            "可用" => {
                let n = parse_count(value.trim_end_matches('次')).ok_or_else(|| {
                    format!("「可用」后面要跟次数（正整数或中文数字），收到「{}」。", value)
                })?;
                uses = n.to_string();
            }
            "有效期" => expires = normalize_expiry(&value)?,
            "到" | "输出到" | "保存到" => {
                if value.is_empty() {
                    return Err("「到」后面缺少目录，例如「生成静态站到public」。".into());
                }
                out = value;
            }
            "设为管理员" | "并设为管理员" | "作为管理员" | "授予管理员权限" => admin = true,
            "的通行密钥" | "的密钥" => has_de = true,
            _ => unreachable!(),
        }
        s = next;
    }

    if matches!(cmd.kind, Kind::UserCreate | Kind::UserCreateAdmin)
        && ["名为", "叫作", "显示名"]
            .iter()
            .any(|k| positional.contains(k))
    {
        return Err(
            "用户名直接用中文即可，不需要「名为」这类显示名。\n例如：创建用户爱丽丝设为管理员"
                .into(),
        );
    }

    match cmd.kind {
        Kind::Build => options.push(format!("--out={}", out)),
        Kind::InviteCreate => {
            options.push(format!("--uses={}", uses));
            options.push(format!("--expires={}", expires));
        }
        Kind::UserCreate | Kind::UserCreateAdmin => {
            if admin {
                options.push("--admin".into());
            }
        }
        Kind::PasskeyReset if !has_de => {
            return Err(format!(
                "请按中文语序写全「的」字结构。\n用法：{}",
                cmd.usage
            ));
        }
        _ => {}
    }

    let need = match cmd.kind {
        Kind::InviteRevoke | Kind::UserCreate | Kind::UserCreateAdmin | Kind::UserDisable
        | Kind::UserEnable | Kind::PasskeyReset => 1,
        _ => 0,
    };
    if need == 1 && positional.is_empty() {
        let what = if cmd.kind == Kind::InviteRevoke { "邀请码" } else { "用户名" };
        return Err(format!("{}还缺{}。\n用法：{}", cmd.cn, what, cmd.usage));
    }
    if need == 0 && !positional.is_empty() {
        return Err(format!(
            "「{}」不接受多余的「{}」。\n用法：{}",
            cmd.cn, positional, cmd.usage
        ));
    }

    let mut argv: Vec<String> = cmd.en.split_whitespace().map(String::from).collect();
    if need == 1 {
        argv.push(positional);
    }
    argv.extend(options);
    Ok(argv)
}

fn parse_line(parts: &[String]) -> Result<(&'static Cmd, Vec<String>), String> {
    if parts.is_empty() {
        return Err("缺少命令。输入「帮助」查看全部命令。".into());
    }
    if is_english_command(&parts[0]) {
        // 英文命令原样透传，方便中英混用（静默兼容，不在帮助里宣传）。
        static EN_ALIAS: Cmd = Cmd {
            kind: Kind::Serve,
            cn: "（英文命令）",
            aliases: &[],
            en: "",
            group: "英文命令",
            usage: "<英文命令>",
            desc: "原样交给英文 CLI 执行",
        };
        return Ok((&EN_ALIAS, parts.to_vec()));
    }
    let line = parts.join("");
    let (cmd, used) = match_command(&line).ok_or_else(|| no_match_message(&line))?;
    let argv = parse_cn(cmd, &line[used..])?;
    Ok((cmd, argv))
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j + 1] + 1).min(prev[j] + 1).min(prev[j] + usize::from(ca != *cb));
        }
        prev = cur;
    }
    prev[b.len()]
}

fn no_match_message(line: &str) -> String {
    let head: String = line.chars().take(5).collect();
    let mut scored: Vec<(usize, &str)> = cmd_names()
        .iter()
        .map(|n| (levenshtein(&head, n), *n))
        .collect();
    scored.sort_by_key(|x| x.0);
    let hint = scored
        .first()
        .filter(|(d, _)| *d <= 3)
        .map(|(_, n)| format!("\n你是不是想输入「{}」？", n))
        .unwrap_or_default();
    format!(
        "「{}」不是有效的中文命令。{}中文命令连写、不加空格，例如「创建邀请码可用三次有效期七天」。\n输入「帮助」查看全部命令。",
        line, hint
    )
}

// ---------------------------------------------------------------- 输出样式

fn init_color(no_color: bool) {
    let ok = !no_color && std::env::var_os("NO_COLOR").is_none() && io::stdout().is_terminal();
    COLOR.store(ok, Ordering::Relaxed);
}

/// 初始化彩色输出。由 `main` 在任何输出之前调用一次。
pub fn setup_color(args: &[String]) {
    let (g, _) = extract_globals(args);
    init_color(g.no_color);
}

fn paint(s: &str, code: &str) -> String {
    if COLOR.load(Ordering::Relaxed) {
        format!("\x1b[{}m{}\x1b[0m", code, s)
    } else {
        s.to_string()
    }
}

fn title(s: &str) -> String {
    paint(s, "1;36")
}
fn bold(s: &str) -> String {
    paint(s, "1")
}
fn dim(s: &str) -> String {
    paint(s, "90")
}
fn red(s: &str) -> String {
    paint(s, "31")
}

fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF
        | 0xFE10..=0xFE19 | 0xFE30..=0xFE6F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1FAFF | 0x20000..=0x3FFFD)
}

fn width(s: &str) -> usize {
    s.chars().map(|c| if is_wide(c) { 2 } else { 1 }).sum()
}

fn pad(s: &str, n: usize) -> String {
    format!("{}{}", s, " ".repeat(n.saturating_sub(width(s))))
}

// ---------------------------------------------------------------- 全局选项

#[derive(Default)]
pub struct Global {
    pub no_color: bool,
    pub help: bool,
    pub version: bool,
}

fn extract_globals(tokens: &[String]) -> (Global, Vec<String>) {
    let mut g = Global::default();
    let mut rest = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        let first = i == 0;
        match t.as_str() {
            "--无颜色" | "--no-color" => g.no_color = true,
            "无颜色" if first => g.no_color = true,
            "--帮助" | "-h" | "--help" => g.help = true,
            "帮助" if first => g.help = true,
            "--版本" | "-V" | "--version" => g.version = true,
            "版本" if first => g.version = true,
            _ => rest.push(t.clone()),
        }
    }
    (g, rest)
}

// ---------------------------------------------------------------- 分派结果

/// 中文前端把原始参数处理后的结果。
pub enum Dispatch {
    /// 翻译好的英文参数，交给 clap 继续解析。
    Argv(Vec<String>),
    /// 直接打印这段文本后退出。
    Print(String),
    /// 打印完整帮助（中文部分 + 英文子命令列表）后退出。
    FullHelp,
}

/// 处理中文命令，返回给 clap 的英文参数。
///
/// 只处理带中文的调用；纯英文参数原样放行，由 clap 自己解析。
pub fn dispatch(args: &[String]) -> Result<Dispatch, String> {
    let (g, parts) = extract_globals(args);

    if g.version {
        return Ok(Dispatch::Print(format!(
            "{} {} （KychNet 站点后端与管理命令行）",
            NAME, VERSION
        )));
    }
    let line = parts.join("");
    if g.help {
        return Ok(match match_command(&line) {
            Some((cmd, _)) if !line.is_empty() => Dispatch::Print(command_help(cmd)),
            _ => Dispatch::FullHelp,
        });
    }
    if parts.is_empty() {
        return Ok(Dispatch::FullHelp);
    }

    let (_cmd, argv) = parse_line(&parts)?;
    Ok(Dispatch::Argv(argv))
}

// ---------------------------------------------------------------- 帮助

pub fn help_text() -> String {
    let mut out = String::new();
    out.push_str(&title(&format!("{} {} — KychNet 命令行", NAME, VERSION)));
    out.push('\n');
    out.push_str("中文命令连写、不加空格：动词在前、定语前置、所属用「的」、数量用中文数量词。\n");
    out.push_str("同一个二进制同时支持中文与英文命令。\n\n");
    out.push_str(&bold("用法"));
    out.push('\n');
    out.push_str(&format!("  {} <中文命令>\n", NAME));
    out.push_str(&format!("  {} <英文命令>\n\n", NAME));
    out.push_str(&format!("  例如：{} 创建邀请码可用三次有效期七天\n\n", NAME));
    out.push_str(&bold("命令一览"));
    out.push('\n');
    let mut last = "";
    for c in CMDS {
        if c.group != last {
            out.push_str(&format!("\n  {}\n", title(c.group)));
            last = c.group;
        }
        out.push_str(&format!("    {}  {}\n", pad(c.usage, 44), c.desc));
    }
    out.push_str(&format!("\n  {}\n", title("中文数量与时长")));
    out.push_str("    可用三次 / 可用3次 / 可用十次      次数写成中文数字或阿拉伯数字\n");
    out.push_str("    有效期七天 / 有效期7d / 24小时     时长可用中文或英文简写\n");
    out.push_str("    有效期永不过期                     永不过期\n");
    out.push_str("    到public / 输出到public            指定静态站目录，默认 dist\n");
    out.push_str(&format!("\n  {}\n", title("全局选项")));
    out.push_str(&format!(
        "    {}  显示帮助；也可「帮助 <命令>」看单条用法\n",
        pad("--帮助 / -h", 20)
    ));
    out.push_str(&format!("    {}  显示版本\n", pad("--版本 / -V", 20)));
    out.push_str(&format!("    {}  关闭彩色输出\n", pad("--无颜色", 20)));
    out.push_str(&format!("\n  {}\n", title("示例")));
    out.push_str(&format!("    {} 启动服务\n", NAME));
    out.push_str(&format!("    {} 生成静态站到public\n", NAME));
    out.push_str(&format!("    {} 创建邀请码可用三次有效期七天\n", NAME));
    out.push_str(&format!("    {} 作废邀请码ABCD1234\n", NAME));
    out.push_str(&format!("    {} 创建用户爱丽丝设为管理员\n", NAME));
    out.push_str(&format!("    {} 创建管理员爱丽丝\n", NAME));
    out.push_str(&format!("    {} 重置爱丽丝的通行密钥\n", NAME));
    out.push_str(&format!("\n  {}\n", title("中文别名（服务器环境变量）")));
    out.push_str(&format!(
        "    二进制名固定为 {}；在服务器的 shell 环境文件里设置别名即可：\n",
        NAME
    ));
    out.push_str(&format!(
        "      /etc/profile.d/kych-net-alias.sh → alias 跨越晨昏='{}'\n",
        NAME
    ));
    out.push_str("    alias 是 shell 命令、不是环境变量；systemd 的 Environment= 与容器 ENV 里\n");
    out.push_str("    写不了 alias，这类场景改用 PATH 目录里的同名链接（命令别名）：\n");
    out.push_str(&format!(
        "      ln -s /opt/kych-net/{} /usr/local/bin/跨越晨昏\n",
        NAME
    ));
    out
}

pub fn command_help(cmd: &Cmd) -> String {
    let mut out = String::new();
    out.push_str(&title(&format!("用法：{} {}", NAME, cmd.usage)));
    out.push('\n');
    out.push_str(cmd.desc);
    out.push('\n');
    if !cmd.aliases.is_empty() {
        out.push_str(&format!("同样说法：{}\n", cmd.aliases.join("、")));
    }
    out.push_str(&dim("中文命令连写、不加空格，不用 --英文选项。"));
    out
}

/// 中文错误信息的统一前缀，与英文侧的 clap 错误区分开。
pub fn error_text(msg: &str) -> String {
    format!("{}{}", red("错误："), msg)
}

// ---------------------------------------------------------------- 单元测试

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(v: &[&str]) -> Result<Vec<String>, String> {
        let t: Vec<String> = v.iter().map(|s| s.to_string()).collect();
        parse_line(&t).map(|(_, a)| a)
    }
    fn ok(v: &[&str]) -> Vec<String> {
        argv(v).unwrap()
    }
    fn words(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn 连写的基础命令() {
        assert_eq!(ok(&["启动服务"]), words(&["serve"]));
        assert_eq!(ok(&["运行服务"]), words(&["serve"]));
        assert_eq!(ok(&["生成静态站"]), words(&["build", "--out=dist"]));
        assert_eq!(ok(&["生成静态站到public"]), words(&["build", "--out=public"]));
        assert_eq!(ok(&["生成静态站输出到dist2"]), words(&["build", "--out=dist2"]));
        assert_eq!(ok(&["执行数据库迁移"]), words(&["db", "migrate"]));
        assert_eq!(ok(&["迁移数据库"]), words(&["db", "migrate"]));
    }

    #[test]
    fn 中文数量词与时长() {
        assert_eq!(
            ok(&["创建邀请码可用三次有效期七天"]),
            words(&["invite", "create", "--uses=3", "--expires=7d"])
        );
        assert_eq!(
            ok(&["创建邀请码可用3次有效期24h"]),
            words(&["invite", "create", "--uses=3", "--expires=24h"])
        );
        assert_eq!(
            ok(&["创建邀请码有效期永不过期"]),
            words(&["invite", "create", "--uses=1", "--expires=never"])
        );
        assert_eq!(
            ok(&["创建邀请码"]),
            words(&["invite", "create", "--uses=1", "--expires=7d"])
        );
        // 用户即便手滑加了空格，也会先拼成连写再解析
        assert_eq!(
            ok(&["创建邀请码", "可用", "三次", "有效期", "七天"]),
            words(&["invite", "create", "--uses=3", "--expires=7d"])
        );
    }

    #[test]
    fn 邀请码命令() {
        assert_eq!(ok(&["列出邀请码"]), words(&["invite", "list"]));
        assert_eq!(ok(&["作废邀请码ABCD1234"]), words(&["invite", "revoke", "ABCD1234"]));
        assert_eq!(ok(&["撤销邀请码ABCD1234"]), words(&["invite", "revoke", "ABCD1234"]));
    }

    #[test]
    fn 用户命令与定语前置() {
        assert_eq!(ok(&["创建管理员alice"]), words(&["user", "create", "alice", "--admin"]));
        assert_eq!(ok(&["创建用户alice设为管理员"]), words(&["user", "create", "alice", "--admin"]));
        assert_eq!(ok(&["创建用户爱丽丝设为管理员"]), words(&["user", "create", "爱丽丝", "--admin"]));
        assert_eq!(ok(&["创建管理员爱丽丝"]), words(&["user", "create", "爱丽丝", "--admin"]));
        assert!(argv(&["创建用户alice名为爱丽丝"]).unwrap_err().contains("不需要"));
        assert_eq!(ok(&["创建用户alice"]), words(&["user", "create", "alice"]));
        assert_eq!(ok(&["创建用户爱丽丝"]), words(&["user", "create", "爱丽丝"]));
        assert_eq!(ok(&["列出用户"]), words(&["user", "list"]));
        assert_eq!(ok(&["停用用户alice"]), words(&["user", "disable", "alice"]));
        assert_eq!(ok(&["启用用户alice"]), words(&["user", "enable", "alice"]));
    }

    #[test]
    fn 的字结构() {
        assert_eq!(ok(&["重置alice的通行密钥"]), words(&["passkey", "reset", "alice"]));
        assert_eq!(ok(&["清除alice的密钥"]), words(&["passkey", "reset", "alice"]));
        assert!(argv(&["重置alice"]).unwrap_err().contains("的」字结构"));
    }

    #[test]
    fn 英文命令透传() {
        assert_eq!(ok(&["user", "enable", "bob"]), words(&["user", "enable", "bob"]));
        assert_eq!(ok(&["invite", "list"]), words(&["invite", "list"]));
    }

    #[test]
    fn 中文报错() {
        assert!(argv(&["作废邀请码"]).unwrap_err().contains("还缺邀请码"));
        assert!(argv(&["创建用户"]).unwrap_err().contains("还缺用户名"));
        assert!(argv(&["启动服务多余"]).unwrap_err().contains("不接受多余"));
        assert!(argv(&["创建邀请码可用零次"]).unwrap_err().contains("次数"));
        assert!(argv(&["生成静态站到"]).unwrap_err().contains("缺少目录"));
        assert!(argv(&["妖妖灵"]).unwrap_err().contains("连写、不加空格"));
        assert!(argv(&["列出用乎"]).unwrap_err().contains("你是不是想输入"));
    }

    #[test]
    fn 最长匹配与全局选项() {
        assert_eq!(match_command("创建邀请码可用三次").unwrap().1, "创建邀请码".len());
        assert_eq!(match_command("执行数据库迁移").unwrap().1, "执行数据库迁移".len());
        let raw = words(&["创建邀请码可用三次", "--帮助"]);
        let (g, rest) = extract_globals(&raw);
        assert!(g.help);
        assert_eq!(rest, words(&["创建邀请码可用三次"]));
        assert_eq!(levenshtein("列出用乎", "列出用户"), 1);
    }

    #[test]
    fn 非ascii参数判定为中文路径() {
        assert!(has_cjk(&words(&["创建用户爱丽丝"])));
        assert!(!has_cjk(&words(&["user", "create", "alice"])));
    }
}
