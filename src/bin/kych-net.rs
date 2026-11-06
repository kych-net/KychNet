//! kych-net — KychNet 中文命令行
//!
//! KychNet 已有一套英文 CLI `kychnet`，这套中文入口的二进制叫 `kych-net`。
//! 中文命令名「跨越晨昏」由服务器环境变量/初始化文件里的别名提供，
//! 不改动二进制与源码名：shell 用 alias，服务托管场景用 PATH 目录里的同名链接。
//! 中文命令按中文语序书写，并且**连写、不加空格**：
//!
//!   创建邀请码可用三次有效期七天
//!   创建管理员alice
//!   重置alice的通行密钥
//!
//! 中文不像英文靠空格分词，因此解析时先去掉所有空白，再按中文短语切出命令名、
//! 数量、时长、所属与位置参数。解析结果直接交给同一个 `kychnet` 执行，与英文 CLI
//! 共用 service 层，不改动后端代码。
//!
//! 放进 KychNet 仓库：把本文件复制为 `src/bin/kych-net.rs`，Cargo 会自动发现它，
//! `cargo build --bin kych-net` 即可。本目录的 Cargo.toml 是零依赖的独立工程，
//! 仅用于在没有 PostgreSQL 的环境里编译与自测。
//!
//! 后端程序查找顺序：KYCHNET_BIN → 本程序同目录 → PATH。

use std::env;
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::process::{Command as Process, ExitCode};
use std::sync::atomic::{AtomicBool, Ordering};

const NAME: &str = "kych-net";
const VERSION: &str = "0.3.0";
static COLOR: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------- 命令定义

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
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

struct Cmd {
    kind: Kind,
    cn: &'static str,
    aliases: &'static [&'static str],
    en: &'static str,
    group: &'static str,
    usage: &'static str,
    desc: &'static str,
}

/// 与 `src/main.rs` 里的 clap 子命令一一对应（含 README 未列出的 `user enable`）。
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
        usage: "创建用户<用户名>名为<显示名>设为管理员",
        desc: "创建用户并输出 TOTP 密钥",
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
            "名为",
            "叫作",
            "显示名",
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
    let mut display: Option<String> = None;
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
            "名为" | "叫作" | "显示名" => {
                if value.is_empty() {
                    return Err("「名为」后面缺少显示名。".into());
                }
                display = Some(value);
            }
            "设为管理员" | "并设为管理员" | "作为管理员" | "授予管理员权限" => admin = true,
            "的通行密钥" | "的密钥" => has_de = true,
            _ => unreachable!(),
        }
        s = next;
    }

    match cmd.kind {
        Kind::Build => options.push(format!("--out={}", out)),
        Kind::InviteCreate => {
            options.push(format!("--uses={}", uses));
            options.push(format!("--expires={}", expires));
        }
        Kind::UserCreate | Kind::UserCreateAdmin => {
            if let Some(d) = display {
                options.push(format!("--display={}", d));
            }
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
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + usize::from(ca != *cb));
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
    let ok = !no_color && env::var_os("NO_COLOR").is_none() && io::stdout().is_terminal();
    COLOR.store(ok, Ordering::Relaxed);
}
fn paint(s: &str, code: &str) -> String {
    if COLOR.load(Ordering::Relaxed) {
        format!("\x1b[{}m{}\x1b[0m", code, s)
    } else {
        s.to_string()
    }
}
fn title(s: &str) -> String { paint(s, "1;36") }
fn bold(s: &str) -> String { paint(s, "1") }
fn dim(s: &str) -> String { paint(s, "90") }
fn red(s: &str) -> String { paint(s, "31") }

fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF
        | 0xFE10..=0xFE19 | 0xFE30..=0xFE6F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1FAFF | 0x20000..=0x3FFFD)
}
fn width(s: &str) -> usize { s.chars().map(|c| if is_wide(c) { 2 } else { 1 }).sum() }
fn pad(s: &str, n: usize) -> String { format!("{}{}", s, " ".repeat(n.saturating_sub(width(s)))) }

// ---------------------------------------------------------------- 全局选项

#[derive(Default)]
struct Global {
    no_color: bool,
    help: bool,
    version: bool,
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

// ---------------------------------------------------------------- 帮助

fn print_help() {
    println!("{}", title(&format!("{} {} — KychNet 中文命令行", NAME, VERSION)));
    println!("中文命令连写、不加空格：动词在前、定语前置、所属用「的」、数量用中文数量词。\n");
    println!("{}", bold("用法"));
    println!("  {} <中文命令>", NAME);
    println!("  例如：{} 创建邀请码可用三次有效期七天\n", NAME);
    println!("{}", bold("命令一览"));
    let mut last = "";
    for c in CMDS {
        if c.group != last {
            println!("\n  {}", title(c.group));
            last = c.group;
        }
        println!("    {}  {}", pad(c.usage, 44), c.desc);
    }
    println!("\n  {}", title("中文数量与时长"));
    println!("    可用三次 / 可用3次 / 可用十次      次数写成中文数字或阿拉伯数字");
    println!("    有效期七天 / 有效期7d / 24小时     时长可用中文或英文简写");
    println!("    有效期永不过期                     永不过期");
    println!("    到public / 输出到public            指定静态站目录，默认 dist");
    println!("\n  {}", title("全局选项"));
    println!("    {}  显示帮助；也可「帮助 <命令>」看单条用法", pad("--帮助 / -h", 20));
    println!("    {}  显示版本", pad("--版本 / -V", 20));
    println!("    {}  关闭彩色输出", pad("--无颜色", 20));
    println!("\n  {}", title("示例"));
    println!("    {} 启动服务", NAME);
    println!("    {} 生成静态站到public", NAME);
    println!("    {} 创建邀请码可用三次有效期七天", NAME);
    println!("    {} 作废邀请码ABCD1234", NAME);
    println!("    {} 创建管理员alice", NAME);
    println!("    {} 创建用户alice名为爱丽丝设为管理员", NAME);
    println!("    {} 重置alice的通行密钥", NAME);
    println!("\n  {}", title("中文别名（服务器环境变量）"));
    println!("    二进制名固定为 {}；在服务器的 shell 环境文件里设置别名即可：", NAME);
    println!("      /etc/profile.d/kychnet-alias.sh → alias 跨越晨昏='{}'", NAME);
    println!("    alias 是 shell 命令、不是环境变量；systemd 的 Environment= 与容器 ENV 里");
    println!("    写不了 alias，这类场景改用 PATH 目录里的同名链接（命令别名）：");
    println!("      ln -s /opt/kychnet/{} /usr/local/bin/跨越晨昏", NAME);
    println!("\n  {}", title("运行依赖"));
    println!("    依次查找 KYCHNET_BIN → 本程序同目录 → PATH 中的 kychnet。");
}

fn print_command_help(cmd: &Cmd) {
    println!("{}", title(&format!("用法：{} {}", NAME, cmd.usage)));
    println!("{}", cmd.desc);
    if !cmd.aliases.is_empty() {
        println!("同样说法：{}", cmd.aliases.join("、"));
    }
    println!("\n{}", dim("中文命令连写、不加空格，不用 --英文选项。"));
}

// ---------------------------------------------------------------- 执行后端

fn find_backend() -> Option<PathBuf> {
    if let Ok(p) = env::var("KYCHNET_BIN") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("kychnet");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    if let Some(paths) = env::var_os("PATH") {
        for dir in env::split_paths(&paths) {
            let p = dir.join("kychnet");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

// ---------------------------------------------------------------- 入口

fn main() -> ExitCode {
    let raw: Vec<String> = env::args().skip(1).collect();
    let (g, parts) = extract_globals(&raw);
    init_color(g.no_color);

    if g.version {
        println!("{} {} （KychNet 中文命令行）", NAME, VERSION);
        return ExitCode::SUCCESS;
    }
    let line = parts.join("");
    if g.help {
        match match_command(&line) {
            Some((cmd, _)) if !line.is_empty() => print_command_help(cmd),
            _ => print_help(),
        }
        return ExitCode::SUCCESS;
    }
    if parts.is_empty() {
        print_help();
        return ExitCode::SUCCESS;
    }

    let (cmd, argv) = match parse_line(&parts) {
        Ok(v) => v,
        Err(msg) => {
            eprintln!("{}{}", red("错误："), msg);
            return ExitCode::FAILURE;
        }
    };

    let backend = match find_backend() {
        Some(p) => p,
        None => {
            eprintln!("{}找不到 KychNet 后端程序 kychnet。", red("错误："));
            eprintln!("  · 把 kychnet 放到 PATH 或本程序同目录，或设置 KYCHNET_BIN=/path/to/kychnet");
            eprintln!("  · 本次命令：{}（{}）", cmd.usage, cmd.desc);
            return ExitCode::FAILURE;
        }
    };
    match Process::new(&backend).args(&argv).status() {
        Ok(status) => ExitCode::from(status.code().unwrap_or(1) as u8),
        Err(e) => {
            eprintln!("{}无法启动 {}：{}", red("错误："), backend.display(), e);
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------- 单元测试

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(v: &[&str]) -> Result<Vec<String>, String> {
        let t: Vec<String> = v.iter().map(|s| s.to_string()).collect();
        parse_line(&t).map(|(_, a)| a)
    }
    fn ok(v: &[&str]) -> Vec<String> { argv(v).unwrap() }
    fn words(v: &[&str]) -> Vec<String> { v.iter().map(|s| s.to_string()).collect() }

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
        assert_eq!(
            ok(&["创建用户alice名为爱丽丝设为管理员"]),
            words(&["user", "create", "alice", "--display=爱丽丝", "--admin"])
        );
        assert_eq!(ok(&["创建用户alice"]), words(&["user", "create", "alice"]));
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
}
