// dynwall — 动态壁纸更换器 (gpui-kit)
//
// 画廊(CC0/自有 loop 素材) + 生成页(prompt → holycrab h3 农场 → 下载 → 本地 crossfade
// 循环化) + 自动轮换。渲染后端: Linux 走 mpvpaper/koi-wallpaper, macOS 用 mpv/open 预览。
//
// 素材库目录: ~/Videos/wallpapers + $DYNWALL_SEED_DIR (默认本仓库 assets/)。
// 生成走 SSH 投批, 复用 gen-wallpaper 的 inbox/out 链路。连接凭据在「03 连接」页
// 管理 (~/.config/dynwall/holycrab.conf + id_holycrab), 未配置时回退 ~/.ssh alias
// (DYNWALL_FARM, 默认 holycrab; 旧 DYNWALL_DGX 仍兼容)。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use gpui_kit::base::h_flex;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    input::{Input, InputState, Textarea, TextareaState},
    label::Label,
    select::{Select, SelectState},
    separator::Separator,
    switch::Switch,
    tab::{Tab, TabBar},
    ActiveTheme as _, Disableable as _, IndexPath, Sizable as _, Size, Theme, TitleBar,
    WindowExt as _, v_flex,
};
use gpui_kit::{
    div, img, px, rgb, AppContext as _, Bounds, Context, Entity, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, StyledImage as _, Window, WindowBounds,
};
use smol::Timer;

const FARM_DIR: &str = "/home/spark/h3_factory";
const PROJECT: &str = "omarchy-demo";
const POLL_SECS: u64 = 30;
const GEN_TIMEOUT_SECS: u64 = 1500;

// Linux 无 Menlo; noto-fonts-cjk 的 Mono 变体 Latin+CJK 一个字体全包, 免逐字回退
#[cfg(target_os = "macos")]
const UI_FONT: &str = "Menlo";
#[cfg(not(target_os = "macos"))]
const UI_FONT: &str = "Noto Sans Mono CJK SC";

/// 未在「03 连接」配置时的回退 ssh alias (旧 env DYNWALL_DGX 仍兼容)。
fn legacy_host() -> String {
    std::env::var("DYNWALL_FARM")
        .or_else(|_| std::env::var("DYNWALL_DGX"))
        .unwrap_or_else(|_| "holycrab".into())
}

fn seed_dir() -> Option<PathBuf> {
    match std::env::var("DYNWALL_SEED_DIR") {
        Ok(d) => Some(PathBuf::from(d)),
        Err(_) => dirs::home_dir().map(|h| h.join("dev/omarchy-china-demo/assets")),
    }
}

fn library_dir() -> PathBuf {
    dirs::video_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join("Videos"))
        .join("wallpapers")
}

fn thumb_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".cache"))
        .join("dynwall/thumbs")
}

// ---------- holycrab 连接配置 (UI 管理, 凭据不进仓库) ----------

fn app_config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"))
        .join("dynwall")
}

/// holycrab.conf 三行: host= user= port=
fn parse_holycrab_conf() -> Option<(String, String, u16)> {
    let s = std::fs::read_to_string(app_config_dir().join("holycrab.conf")).ok()?;
    let (mut host, mut user, mut port) = (String::new(), String::new(), 22u16);
    for line in s.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k.trim() {
            "host" => host = v.trim().into(),
            "user" => user = v.trim().into(),
            "port" => port = v.trim().parse().unwrap_or(22),
            _ => {}
        }
    }
    (!host.is_empty() && !user.is_empty()).then_some((host, user, port))
}

/// 解析后的 SSH 连接: 「03 连接」的显式配置优先, 否则回退 ~/.ssh alias。
#[derive(Clone)]
struct SshTarget {
    dest: String,
    explicit: bool,
    key: Option<PathBuf>,
    port: u16,
    known_hosts: Option<PathBuf>,
}

impl SshTarget {
    fn ssh_args(&self) -> Vec<String> {
        let mut a: Vec<String> = vec!["-o".into(), "ConnectTimeout=10".into()];
        if let Some(k) = &self.key {
            a.extend(["-i".into(), k.to_string_lossy().into_owned()]);
        }
        if self.explicit {
            a.extend(["-p".into(), self.port.to_string()]);
            if let Some(kh) = &self.known_hosts {
                a.extend([
                    "-o".into(),
                    format!("UserKnownHostsFile={}", kh.display()),
                    "-o".into(),
                    "StrictHostKeyChecking=accept-new".into(),
                ]);
            }
        }
        a.push(self.dest.clone());
        a
    }
    fn scp_args(&self) -> Vec<String> {
        let mut a: Vec<String> = vec!["-q".into()];
        if let Some(k) = &self.key {
            a.extend(["-i".into(), k.to_string_lossy().into_owned()]);
        }
        if self.explicit {
            a.extend(["-P".into(), self.port.to_string()]);
            if let Some(kh) = &self.known_hosts {
                a.extend([
                    "-o".into(),
                    format!("UserKnownHostsFile={}", kh.display()),
                    "-o".into(),
                    "StrictHostKeyChecking=accept-new".into(),
                ]);
            }
        }
        a
    }
    /// 「03 连接」页的长状态。
    fn mode_label(&self) -> String {
        match (&self.explicit, &self.key) {
            (true, Some(k)) => {
                format!("EXPLICIT {}:{} · key ✓ {}", self.dest, self.port, k.display())
            }
            (true, None) => format!("EXPLICIT {}:{} · 无 key, 走本机默认私钥", self.dest, self.port),
            (false, _) => format!("LEGACY ~/.ssh alias {} (未在 UI 配置)", self.dest),
        }
    }
    /// 状态栏短状态。
    fn short_label(&self) -> String {
        match self.key {
            Some(_) => format!("{} 🔑", self.dest),
            None => self.dest.clone(),
        }
    }
}

fn load_conn() -> SshTarget {
    if let Some((host, user, port)) = parse_holycrab_conf() {
        let key = app_config_dir().join("id_holycrab");
        return SshTarget {
            dest: format!("{user}@{host}"),
            explicit: true,
            key: key.is_file().then_some(key),
            port,
            known_hosts: Some(app_config_dir().join("known_hosts")),
        };
    }
    SshTarget {
        dest: legacy_host(),
        explicit: false,
        key: None,
        port: 22,
        known_hosts: None,
    }
}

// ---------- 极客风主题 (phosphor terminal) ----------

const C_BG: u32 = 0x0B0E13;
const C_SURFACE: u32 = 0x11151D;
const C_SURFACE2: u32 = 0x171D27;
const C_BORDER: u32 = 0x1E2530;
const C_FG: u32 = 0xC6D0DB;
const C_DIM: u32 = 0x5C6875;
const C_ACCENT: u32 = 0x35E08C;
const C_ACCENT_HI: u32 = 0x4BE89B;
const C_ACCENT_LO: u32 = 0x2BC97A;
const C_ON_ACCENT: u32 = 0x062015;
const C_AMBER: u32 = 0xE0B055;
const C_DANGER: u32 = 0xE86A6A;
const C_CYAN: u32 = 0x5AC8E8;

fn hx(c: u32) -> Hsla {
    rgb(c).into()
}

/// 全局换肤: 所有 kit 组件(Button/Input/Select/Switch/Tab/通知)一起跟着变。
fn hack_theme(cx: &mut gpui_kit::App) {
    Theme::update(cx, |t| {
        t.radius = px(2.);
        t.radius_lg = px(3.);
        t.font_family = UI_FONT.into();
        t.font_size = px(13.);
        t.mono_font_family = UI_FONT.into();
        t.mono_font_size = px(12.);
        t.shadow = false;
        t.focus_ring = false;
        let c = &mut t.colors;
        c.background = hx(C_BG);
        c.foreground = hx(C_FG);
        c.border = hx(C_BORDER);
        c.window_border = hx(C_BORDER);
        c.title_bar = hx(C_BG);
        c.title_bar_border = hx(C_BORDER);
        c.status_bar = hx(C_SURFACE);
        c.status_bar_border = hx(C_BORDER);
        c.input = hx(C_BORDER);
        c.caret = hx(C_ACCENT);
        c.ring = hx(C_ACCENT_LO);
        c.selection = hx(0x1B3A2B);
        c.primary = hx(C_ACCENT);
        c.primary_foreground = hx(C_ON_ACCENT);
        c.primary_hover = hx(C_ACCENT_HI);
        c.primary_active = hx(C_ACCENT_LO);
        c.button_primary = hx(C_ACCENT);
        c.button_primary_foreground = hx(C_ON_ACCENT);
        c.button_primary_hover = hx(C_ACCENT_HI);
        c.button_primary_active = hx(C_ACCENT_LO);
        c.button = hx(C_SURFACE);
        c.button_foreground = hx(C_FG);
        c.button_hover = hx(C_SURFACE2);
        c.button_active = hx(C_SURFACE2);
        c.secondary = hx(C_SURFACE);
        c.secondary_foreground = hx(C_FG);
        c.secondary_hover = hx(C_SURFACE2);
        c.secondary_active = hx(C_SURFACE2);
        c.button_secondary = hx(C_SURFACE);
        c.button_secondary_foreground = hx(C_FG);
        c.button_secondary_hover = hx(C_SURFACE2);
        c.button_secondary_active = hx(C_SURFACE2);
        c.danger = hx(C_DANGER);
        c.danger_foreground = hx(C_DANGER);
        c.button_danger = hx(0x241419);
        c.button_danger_foreground = hx(C_DANGER);
        c.button_danger_hover = hx(0x33191E);
        c.button_danger_active = hx(0x3D1E24);
        c.accent = hx(C_ACCENT);
        c.accent_foreground = hx(C_ON_ACCENT);
        c.muted = hx(C_SURFACE);
        c.muted_foreground = hx(C_DIM);
        c.popover = hx(0x10141C);
        c.popover_foreground = hx(C_FG);
        c.list = hx(C_SURFACE);
        c.list_hover = hx(C_SURFACE2);
        c.list_active = hx(C_SURFACE2);
        c.list_active_border = hx(C_ACCENT);
        c.link = hx(C_CYAN);
        c.link_hover = hx(C_CYAN);
        c.switch = hx(C_ACCENT);
        c.progress_bar = hx(C_ACCENT);
        c.scrollbar = hx(C_SURFACE);
        c.scrollbar_thumb = hx(0x2A3340);
        c.scrollbar_thumb_hover = hx(0x39424E);
        c.overlay = hx(0x000000);
        c.tab_bar = hx(C_BG);
        c.tab_bar_segmented = hx(C_SURFACE);
        c.tab_active = hx(C_SURFACE2);
        c.tab_active_foreground = hx(C_ACCENT);
        c.tab_foreground = hx(C_DIM);
    });
}

// ---------- 素材库 ----------

struct WallEntry {
    path: PathBuf,
    name: String,
    thumb: Option<PathBuf>,
    meta: Option<String>,
}

impl WallEntry {
    fn scan() -> Vec<Self> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        let mut dirs: Vec<PathBuf> = Vec::new();
        if let Some(d) = seed_dir() {
            dirs.push(d);
        }
        dirs.push(library_dir());
        for dir in dirs {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_none_or(|x| x != "mp4") {
                    continue;
                }
                let Ok(canonical) = p.canonicalize() else {
                    continue;
                };
                if !seen.insert(canonical) {
                    continue;
                }
                let name = p
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let thumb = thumb_for(&p);
                out.push(Self { path: p, name, thumb, meta: None });
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

fn thumb_for(video: &Path) -> Option<PathBuf> {
    let mut h = DefaultHasher::new();
    video.hash(&mut h);
    let t = thumb_dir().join(format!("{:016x}.png", h.finish()));
    t.is_file().then_some(t)
}

/// 抽封面帧(-ss 快进到 2.5s)。阻塞, 请放后台。
fn make_thumb(video: &Path, out: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(out.parent().unwrap())?;
    let status = Command::new("ffmpeg")
        .args([
            "-y", "-v", "error", "-ss", "2.5", "-i",
            video.to_string_lossy().as_ref(), "-frames:v", "1", "-vf", "scale=480:-2",
            out.to_string_lossy().as_ref(),
        ])
        .status()?;
    if !status.success() {
        anyhow::bail!("ffmpeg thumbnail failed for {}", video.display());
    }
    Ok(())
}

/// 卡片元数据行: 1920×1080 · 10.0s · 2.4M。阻塞, 请放后台。
fn probe_meta(video: &Path) -> Option<String> {
    let out = Command::new("ffprobe")
        .args([
            "-v", "error", "-select_streams", "v:0",
            "-show_entries", "stream=width,height",
            "-show_entries", "format=duration,size",
            "-of", "csv=p=0",
            video.to_string_lossy().as_ref(),
        ])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let mut lines = s.lines().filter(|l| !l.trim().is_empty());
    let wh = lines.next()?;
    let ds = lines.next()?;
    let mut wh = wh.split(',');
    let w = wh.next()?.trim();
    let h = wh.next()?.trim();
    let mut ds = ds.split(',');
    let dur: f64 = ds.next()?.trim().parse().ok()?;
    let size: f64 = ds.next()?.trim().parse().ok()?;
    Some(format!("{w}×{h} · {dur:.1}s · {:.1}M", size / 1048576.))
}

// ---------- 应用上墙 / 预览 ----------

/// PATH 里找可执行文件。spawn 只报"文件不存在", 分不清"没装"和"装哪了", 先自查。
fn have(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

/// Try Omarchy / VirGL 虚机里 EGL 配置匹配不上 (EGL_BAD_MATCH) —
/// 软件 GL + 关硬解绕过; 裸金属不受影响。
fn in_vm() -> bool {
    Command::new("systemd-detect-virt")
        .arg("-q")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 上墙: Linux → koi-wallpaper (mpvpaper); macOS → mpv 循环 / open 兜底。
fn apply_wallpaper(video: &Path) -> anyhow::Result<()> {
    let v = video.to_string_lossy().into_owned();
    #[cfg(target_os = "linux")]
    {
        let vm = in_vm();
        // koi-wallpaper 依赖 mpvpaper, 不在就直接走下面兜底 (不然它后台静默失败)
        if have("koi-wallpaper") && have("mpvpaper") {
            if Command::new("koi-wallpaper").arg("on").arg(&v).spawn().is_ok() {
                return Ok(());
            }
        }
        if have("mpvpaper") {
            let mut cmd = Command::new("mpvpaper");
            if vm {
                cmd.env("LIBGL_ALWAYS_SOFTWARE", "1");
                cmd.args(["-o", "loop mute=yes panscan=1.0 hwdec=no"]);
            } else {
                cmd.args(["-o", "loop mute=yes panscan=1.0 hwdec=auto"]);
            }
            if cmd.arg("*").arg(&v).spawn().is_ok() {
                return Ok(());
            }
        }
        // mpv 全屏兜底 (窗口层, 非壁纸层, 但至少能看到); VM 里走 X11 输出, 完全不碰 EGL
        let mut mpv = Command::new("mpv");
        if vm {
            mpv.env("LIBGL_ALWAYS_SOFTWARE", "1")
                .args(["--hwdec=no", "--vo=x11"]);
        }
        if mpv
            .args(["--loop=inf", "--really-quiet", "--fullscreen"])
            .arg(&v)
            .spawn()
            .is_ok()
        {
            return Ok(());
        }
        anyhow::bail!("缺播放器 — 跑仓库里的 bin/install.sh 一键装 (或 sudo pacman -S mpv)");
    }
    #[cfg(not(target_os = "linux"))]
    {
        let mpv = Command::new("mpv")
            .args(["--loop=inf", "--really-quiet"])
            .arg(&v)
            .spawn();
        if mpv.is_err() {
            Command::new("open").arg(&v).spawn()?;
        }
        Ok(())
    }
}

fn stop_wallpaper() {
    #[cfg(target_os = "linux")]
    let _ = Command::new("koi-wallpaper").arg("off").spawn();
    #[cfg(not(target_os = "linux"))]
    {
        let _ = Command::new("pkill").args(["-f", "mpv.*--loop=inf"]).spawn();
    }
}

// ---------- SSH 投批链路 (与 gen-wallpaper 同路) ----------

fn ssh_run(t: &SshTarget, cmd: &str) -> anyhow::Result<String> {
    let out = Command::new("ssh").args(t.ssh_args()).arg(cmd).output()?;
    if !out.status.success() {
        anyhow::bail!("ssh {}: {}", t.dest, String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn ssh_write_manifest(t: &SshTarget, remote: &str, content: &str) -> anyhow::Result<()> {
    use std::io::Write as _;
    let mut child = Command::new("ssh")
        .args(t.ssh_args())
        .arg(format!("cat > {remote}"))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("ssh stdin closed"))?
        .write_all(content.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        anyhow::bail!("manifest submit: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

fn scp_pull(t: &SshTarget, remote: &str, local: &Path) -> anyhow::Result<()> {
    let out = Command::new("scp")
        .args(t.scp_args())
        .arg(format!("{}:{remote}", t.dest))
        .arg(local.to_string_lossy().as_ref())
        .output()?;
    if !out.status.success() {
        anyhow::bail!("scp pull: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// 原片 → 尾→首 0.75s crossfade 无缝循环 (gen-wallpaper 同款滤镜)。
fn crossfade_loop(raw: &Path, out: &Path) -> anyhow::Result<()> {
    let total: f64 = String::from_utf8(
        Command::new("ffprobe")
            .args([
                "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0",
                &raw.to_string_lossy(),
            ])
            .output()?
            .stdout,
    )?
    .trim()
    .parse()?;
    let fade = 0.75;
    let main_end = total - fade;
    let fc = format!(
        "[0:v]split=3[a][b][c];\
         [a]trim=end={main_end:.3},setpts=PTS-STARTPTS[main];\
         [b]trim=start={main_end:.3},setpts=PTS-STARTPTS[tail];\
         [c]trim=end={fade},setpts=PTS-STARTPTS[head];\
         [tail][head]xfade=transition=fade:duration={fade}:offset=0[blend];\
         [blend][main]concat=n=2:v=1:a=0[out]"
    );
    let status = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-i", raw.to_string_lossy().as_ref()])
        .args(["-filter_complex", &fc])
        .args(["-map", "[out]", "-an", "-c:v", "libx264", "-crf", "18", "-pix_fmt", "yuv420p"])
        .arg(out.to_string_lossy().as_ref())
        .status()?;
    if !status.success() {
        anyhow::bail!("crossfade ffmpeg failed");
    }
    Ok(())
}

// ---------- 生成任务状态 ----------

#[derive(Clone)]
enum GenPhase {
    Submitting,
    Rendering,
    Downloading,
    Looping,
    Done(String),
    Failed(String),
}

impl GenPhase {
    fn text(&self) -> String {
        match self {
            Self::Submitting => "投批中…".into(),
            Self::Rendering => "农场渲染中…".into(),
            Self::Downloading => "拉回成片中…".into(),
            Self::Looping => "本地 crossfade 循环化…".into(),
            Self::Done(m) => format!("✅ {m}"),
            Self::Failed(m) => format!("❌ {m}"),
        }
    }
    fn busy(&self) -> bool {
        matches!(self, Self::Submitting | Self::Rendering | Self::Downloading | Self::Looping)
    }
}

struct GenJob {
    id: String,
    phase: GenPhase,
    started: Instant,
}

// ---------- 主界面 ----------

struct DynWall {
    tab_ix: usize,
    entries: Vec<WallEntry>,
    selected: usize,
    applied: Option<String>,
    prompt: Entity<InputState>,
    dur_select: Entity<SelectState<Vec<&'static str>>>,
    ratio_select: Entity<SelectState<Vec<SharedString>>>,
    seamless: bool,
    gen: Option<GenJob>,
    rotate: bool,
    rotate_min: Entity<SelectState<Vec<&'static str>>>,
    rotate_tick: u64,
    cfg_host: Entity<InputState>,
    cfg_user: Entity<InputState>,
    cfg_port: Entity<InputState>,
    cfg_key: Entity<TextareaState>,
    conn_status: Option<(bool, String)>,
}

impl DynWall {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let entries = WallEntry::scan();
        let prompt = cx.new(|cx| {
            InputState::new(window, cx).placeholder("例: 雨夜霓虹的重庆洪崖洞, 江面倒影流动…")
        });
        let dur_select = cx.new(|cx| {
            SelectState::new(vec!["5", "8", "10", "15"], Some(IndexPath::new(2)), window, cx)
        });
        let ratio_select = cx.new(|cx| {
            SelectState::new(
                vec![SharedString::from("16:9"), SharedString::from("9:16")],
                Some(IndexPath::new(0)),
                window,
                cx,
            )
        });
        let rotate_min = cx.new(|cx| {
            SelectState::new(vec!["1", "3", "5", "10", "30"], Some(IndexPath::new(2)), window, cx)
        });
        // 「03 连接」预填: 已保存配置 > demo 默认值
        let (ch, cu, cp) = parse_holycrab_conf()
            .unwrap_or_else(|| ("192.168.1.227".into(), "spark".into(), 22));
        let cfg_host =
            cx.new(|cx| InputState::new(window, cx).default_value(ch).placeholder("192.168.1.227"));
        let cfg_user =
            cx.new(|cx| InputState::new(window, cx).default_value(cu).placeholder("spark"));
        let cfg_port =
            cx.new(|cx| InputState::new(window, cx).default_value(cp.to_string()).placeholder("22"));
        let cfg_key = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("粘贴 OpenSSH 私钥 (-----BEGIN ... PRIVATE KEY----- 整段)。留空 = 沿用已保存的 key")
        });
        let this = Self {
            tab_ix: 0,
            entries,
            selected: 0,
            applied: None,
            prompt,
            dur_select,
            ratio_select,
            seamless: true,
            gen: None,
            rotate: false,
            rotate_min,
            rotate_tick: 0,
            cfg_host,
            cfg_user,
            cfg_port,
            cfg_key,
            conn_status: None,
        };

        // 封面帧 + 元数据后台补齐
        cx.spawn(async move |this, cx| {
            eprintln!("[backfill] start");
            let todo: Vec<(PathBuf, PathBuf, bool)> = this.update(cx, |t, _| {
                t.entries
                    .iter()
                    .filter(|e| e.thumb.is_none() || e.meta.is_none())
                    .map(|e| {
                        let thumb = thumb_for(&e.path).unwrap_or_else(|| {
                            let mut h = DefaultHasher::new();
                            e.path.hash(&mut h);
                            thumb_dir().join(format!("{:016x}.png", h.finish()))
                        });
                        (e.path.clone(), thumb, e.thumb.is_none())
                    })
                    .collect()
            })?;
            eprintln!("[backfill] todo={} entries", todo.len());
            for (video, thumb, need_thumb) in todo {
                if need_thumb && make_thumb(&video, &thumb).is_err() {
                    continue;
                }
                eprintln!("[backfill] probing {}", video.display());
                let meta = smol::unblock({
                    let v = video.clone();
                    move || probe_meta(&v)
                })
                .await;
                eprintln!("[backfill] probed -> {meta:?}");
                this.update(cx, |t, cx| {
                    if let Some(e) = t.entries.iter_mut().find(|e| e.path == video) {
                        if e.thumb.is_none() {
                            e.thumb = Some(thumb.clone());
                        }
                        e.meta = meta;
                    }
                    cx.notify();
                })?;
            }
            eprintln!("[backfill] done");
            anyhow::Ok(())
        })
        .detach();

        // 自动轮换心跳: 每分钟一跳, 到点换下一张
        cx.spawn(async move |this, cx| {
            loop {
                Timer::after(Duration::from_secs(60)).await;
                let due = this
                    .update(cx, |t, cx| {
                        if !t.rotate || t.entries.is_empty() {
                            return false;
                        }
                        let every: u64 = t
                            .rotate_min
                            .read(cx)
                            .selected_value()
                            .copied()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(5);
                        t.rotate_tick += 1;
                        if t.rotate_tick >= every {
                            t.rotate_tick = 0;
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false);
                if due {
                    this.update(cx, |t, cx| {
                        t.selected = (t.selected + 1) % t.entries.len().max(1);
                        if let Some(e) = t.entries.get(t.selected) {
                            if apply_wallpaper(&e.path).is_ok() {
                                t.applied = Some(e.name.clone());
                            }
                        }
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();

        this
    }

    fn submit_generation(&mut self, cx: &mut Context<Self>) {
        if self.gen.as_ref().is_some_and(|g| g.phase.busy()) {
            return;
        }
        let prompt = self.prompt.read(cx).value().trim().to_string();
        if prompt.is_empty() {
            return;
        }
        let dur: u64 = self
            .dur_select
            .read(cx)
            .selected_value()
            .copied()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10);
        let ratio = self
            .ratio_select
            .read(cx)
            .selected_value()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "16:9".into());
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64 * 7919 + d.as_secs() % 100000)
            .unwrap_or(42);
        let id = format!("app_{}_{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() % 100000)
            .unwrap_or(0), seed % 1000);
        let manifest = format!(
            r#"{{"id": "{id}", "project": "{PROJECT}", "prompt": {}, "model": "h3-fast-12", "mp": 0.7, "dur": {dur}, "ratio": "{ratio}", "seed": {seed}, "raw": true, "sheet": false}}"#,
            json_str(&prompt),
        );
        self.gen = Some(GenJob { id: id.clone(), phase: GenPhase::Submitting, started: Instant::now() });
        cx.notify();

        let conn = load_conn();
        cx.spawn(async move |this, cx| {
            // 1) 投批
            let remote_manifest = format!("{FARM_DIR}/inbox/60_{id}.jsonl");
            let conn0 = conn.clone();
            let r = smol::unblock({
                let id = id.clone();
                move || ssh_write_manifest(&conn0, &remote_manifest, &manifest)
                    .map(|_| id)
            })
            .await;
            let id = match r {
                Ok(id) => id,
                Err(e) => {
                    this.update(cx, |t, cx| {
                        if let Some(g) = t.gen.as_mut() {
                            g.phase = GenPhase::Failed(format!("投批失败: {e}"));
                        }
                        cx.notify();
                    })?;
                    return anyhow::Ok(());
                }
            };
            this.update(cx, |t, cx| {
                if let Some(g) = t.gen.as_mut() {
                    g.phase = GenPhase::Rendering;
                }
                cx.notify();
            })?;

            // 2) 轮询 out/
            let mut product = String::new();
            let mut waited = 0u64;
            loop {
                Timer::after(Duration::from_secs(POLL_SECS)).await;
                waited += POLL_SECS;
                let probe = smol::unblock({
                    let id = id.clone();
                    let conn = conn.clone();
                    move || {
                        ssh_run(&conn,
                            &format!("ls {FARM_DIR}/out/ 2>/dev/null | grep -F '{PROJECT}_{id}_' | grep '\\.mp4$' | head -1"))
                            .or_else(|_| {
                                // 区分失败: worker 判退/失败记日志
                                ssh_run(&conn,
                                    &format!("grep -E 'REJECT|FAIL' {FARM_DIR}/worker.log | grep -F '{id}' | head -1"))
                                    .map(|_| "__failed__".to_string())
                            })
                    }
                })
                .await;
                match probe {
                    Ok(s) if s == "__failed__" => {
                        this.update(cx, |t, cx| {
                            if let Some(g) = t.gen.as_mut() {
                                g.phase = GenPhase::Failed("农场判退/失败, 查 worker.log".into());
                            }
                            cx.notify();
                        })?;
                        return anyhow::Ok(());
                    }
                    Ok(s) if !s.trim().is_empty() => {
                        product = s.trim().into();
                        break;
                    }
                    _ => {
                        if waited >= GEN_TIMEOUT_SECS {
                            this.update(cx, |t, cx| {
                                if let Some(g) = t.gen.as_mut() {
                                    g.phase = GenPhase::Failed(format!("{waited}s 超时"));
                                }
                                cx.notify();
                            })?;
                            return anyhow::Ok(());
                        }
                    }
                }
            }

            // 3) 拉回
            this.update(cx, |t, cx| {
                if let Some(g) = t.gen.as_mut() {
                    g.phase = GenPhase::Downloading;
                }
                cx.notify();
            })?;
            let raw_dir = dirs::cache_dir().unwrap_or_else(|| PathBuf::from("/tmp")).join("dynwall");
            let _ = std::fs::create_dir_all(&raw_dir);
            let raw = raw_dir.join(format!("raw_{id}.mp4"));
            if let Err(e) = smol::unblock({
                let product = product.clone();
                let raw = raw.clone();
                let conn = conn.clone();
                move || scp_pull(&conn, &format!("{FARM_DIR}/out/{product}"), &raw)
            })
            .await
            {
                this.update(cx, |t, cx| {
                    if let Some(g) = t.gen.as_mut() {
                        g.phase = GenPhase::Failed(format!("拉回失败: {e}"));
                    }
                    cx.notify();
                })?;
                return anyhow::Ok(());
            }

            // 4) 循环化 + 入库
            this.update(cx, |t, cx| {
                if let Some(g) = t.gen.as_mut() {
                    g.phase = GenPhase::Looping;
                }
                cx.notify();
            })?;
            std::fs::create_dir_all(library_dir())?;
            let out = library_dir().join(format!("{id}_loop.mp4"));
            let r = smol::unblock({
                let raw = raw.clone();
                let out = out.clone();
                move || crossfade_loop(&raw, &out)
            })
            .await;
            this.update(cx, |t, cx| {
                let phase = match r {
                    Ok(()) => {
                        t.entries = WallEntry::scan();
                        t.selected = t.entries.iter().position(|e| e.path == out).unwrap_or(0);
                        GenPhase::Done(format!("已入库 {}", out.display()))
                    }
                    Err(e) => GenPhase::Failed(format!("循环化失败: {e}")),
                };
                if let Some(g) = t.gen.as_mut() {
                    g.phase = phase;
                }
                cx.notify();
            })?;
            anyhow::Ok(())
        })
        .detach();
    }

    fn elapsed_text(&self) -> String {
        self.gen
            .as_ref()
            .filter(|g| g.phase.busy())
            .map(|g| format!("{}s", g.started.elapsed().as_secs()))
            .unwrap_or_default()
    }
}

fn json_str(s: &str) -> String {
    // 极简 JSON 字符串转义 (prompt 场景够用)
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl Render for DynWall {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tab_bar = TabBar::new("tabs")
            .selected_index(self.tab_ix)
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                this.tab_ix = *ix;
                cx.notify();
            }))
            .child(Tab::new().label("01 画廊"))
            .child(Tab::new().label("02 生成"))
            .child(Tab::new().label("03 连接"));

        let body = match self.tab_ix {
            0 => self.render_gallery(window, cx).into_any_element(),
            1 => self.render_generate(cx).into_any_element(),
            _ => self.render_conn(cx).into_any_element(),
        };

        // TUI 状态栏
        let (led, led_text, led_color) = match &self.applied {
            Some(n) => ("●", format!("ON WALL ▸ {n}"), hx(C_ACCENT)),
            None => ("○", "IDLE".into(), hx(C_DIM)),
        };
        let rotate_txt = if self.rotate {
            let m = self
                .rotate_min
                .read(cx)
                .selected_value()
                .copied()
                .unwrap_or("5");
            format!("ROTATE {m}MIN")
        } else {
            "ROTATE OFF".into()
        };
        let status_bar = h_flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .bg(hx(C_SURFACE))
            .border_t_1()
            .border_color(hx(C_BORDER))
            .text_size(px(11.))
            .child(div().text_color(led_color).child(led))
            .child(div().text_color(led_color).child(led_text))
            .child(div().flex_1())
            .child(
                div()
                    .text_color(hx(C_DIM))
                    .child(format!(
                        "{} ASSETS · {} · {} · DYNWALL v0.2",
                        self.entries.len(),
                        load_conn().short_label(),
                        rotate_txt
                    )),
            );

        div()
            .size_full()
            .flex()
            .flex_col()
            .font_family(UI_FONT)
            .bg(hx(C_BG))
            .text_color(hx(C_FG))
            .child(
                TitleBar::new()
                    .child("DYNWALL")
                    .child(Label::new("· 动态壁纸更换器").text_size(px(12.)).text_color(hx(C_DIM)))
                    .child(tab_bar),
            )
            .child(div().flex_1().min_h_0().child(body))
            .child(status_bar)
    }
}

impl DynWall {
    fn render_gallery(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        // 画廊网格
        let mut grid = div()
            .id("grid-scroll")
            .flex()
            .flex_wrap()
            .gap_3()
            .p_4()
            .content_start()
            .overflow_y_scroll();
        if self.entries.is_empty() {
            grid = grid.child(
                Label::new("// 素材库为空 — 把 loop mp4 丢进 ~/Videos/wallpapers/ 或切「02 生成」")
                    .text_size(px(12.))
                    .text_color(hx(C_DIM)),
            );
        }
        for (ix, e) in self.entries.iter().enumerate() {
            let selected = ix == self.selected;
            let on_wall = self.applied.as_deref() == Some(e.name.as_str());
            let mut card = div()
                .id(ix)
                .w(px(240.))
                .flex()
                .flex_col()
                .rounded(theme.radius_lg)
                .bg(if selected { hx(C_SURFACE2) } else { hx(C_SURFACE) })
                .border_1()
                .cursor_pointer()
                .overflow_hidden()
                .hover(|s| s.border_color(hx(C_ACCENT_LO)))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.selected = ix;
                    cx.notify();
                    if let Some(e) = this.entries.get(ix) {
                        window.push_notification(format!("已选 {}", e.name), cx);
                    }
                }));
            card = if selected {
                card.border_color(hx(C_ACCENT))
            } else {
                card.border_color(hx(C_BORDER))
            };
            let poster: Box<dyn Fn() -> gpui_kit::AnyElement> = match &e.thumb {
                Some(t) => {
                    let t = t.clone();
                    Box::new(move || img(t.clone()).w_full().h(px(135.)).object_fit(gpui_kit::ObjectFit::Cover).into_any_element())
                }
                None => Box::new(|| div().w_full().h(px(135.)).into_any_element()),
            };
            let name_color = if selected { hx(C_ACCENT) } else { hx(C_FG) };
            let marker: &str = if on_wall {
                " ▸W"
            } else if selected {
                " ▸"
            } else {
                ""
            };
            card = card
                .child(poster())
                .child(
                    div()
                        .px_2()
                        .pt_1()
                        .text_size(px(12.))
                        .text_ellipsis()
                        .child(format!("{:02} {}{}", ix + 1, e.name, marker))
                        .text_color(name_color),
                )
                .child(
                    div()
                        .px_2()
                        .pb_1()
                        .text_size(px(10.5))
                        .text_color(hx(C_DIM))
                        .child(e.meta.clone().unwrap_or_else(|| "probing…".into())),
                );
            grid = grid.child(card);
        }

        // 工具条
        let selected_name = self
            .entries
            .get(self.selected)
            .map(|e| e.name.clone())
            .unwrap_or_else(|| "—".into());
        let toolbar = h_flex()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .bg(hx(C_SURFACE))
            .border_b_1()
            .border_color(theme.border)
            .text_size(px(12.))
            .child(
                Label::new(format!("TARGET ▸ {selected_name}"))
                    .text_size(px(12.))
                    .text_color(hx(C_ACCENT))
                    .flex_1(),
            )
            .child(
                Button::new("apply")
                    .primary()
                    .label("上墙")
                    .with_size(Size::Small)
                    .on_click(cx.listener(|this, _, window, cx| {
                        match this.entries.get(this.selected) {
                            Some(e) => match apply_wallpaper(&e.path) {
                                Ok(()) => {
                                    this.applied = Some(e.name.clone());
                                    window.push_notification(format!("已上墙 {}", e.name), cx);
                                }
                                Err(er) => window.push_notification(format!("上墙失败: {er}"), cx),
                            },
                            None => window.push_notification("先选一张素材", cx),
                        }
                    })),
            )
            .child(
                Button::new("preview")
                    .label("预览")
                    .with_size(Size::Small)
                    .on_click(cx.listener(|this, _, _, _| {
                        if let Some(e) = this.entries.get(this.selected) {
                            let _ = Command::new(if cfg!(target_os = "macos") { "open" } else { "xdg-open" })
                                .arg(&e.path)
                                .spawn();
                        }
                    })),
            )
            .child(
                Button::new("stop")
                    .danger()
                    .label("停")
                    .with_size(Size::Small)
                    .on_click(cx.listener(|this, _, window, cx| {
                        stop_wallpaper();
                        this.applied = None;
                        window.push_notification("已停, 恢复静态壁纸", cx);
                    })),
            )
            .child(Separator::horizontal())
            .child(Label::new("轮换").text_size(px(12.)).text_color(hx(C_DIM)))
            .child(
                Switch::new("rotate")
                    .checked(self.rotate)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.rotate = *checked;
                        this.rotate_tick = 0;
                        cx.notify();
                    })),
            )
            .child(Select::new(&self.rotate_min).w(px(88.)).with_size(Size::Small));

        v_flex().size_full()
            .child(toolbar)
            .child(div().flex_1().min_h_0().child(grid))
    }

    fn render_generate(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let busy = self.gen.as_ref().is_some_and(|g| g.phase.busy());
        let status = self.gen.as_ref().map(|g| {
            let el = self.elapsed_text();
            let time = if el.is_empty() { String::new() } else { format!(" · {el}") };
            let color = match &g.phase {
                GenPhase::Done(_) => hx(C_ACCENT),
                GenPhase::Failed(_) => hx(C_DANGER),
                _ => hx(C_AMBER),
            };
            (
                color,
                format!("{}{} ▌", g.phase.text(), time),
            )
        });

        v_flex()
            .p_4()
            .gap_2()
            .max_w(px(680.))
            .child(
                Label::new("PROMPT ▸ 一句话描述想要的壁纸 (T2VA 直出 + 本地 crossfade 无缝循环)")
                    .text_size(px(12.))
                    .text_color(hx(C_DIM)),
            )
            .child(Input::new(&self.prompt))
            .child(
                h_flex().items_center().gap_3().mt_1()
                    .child(Label::new("PARAMS ▸").text_size(px(12.)).text_color(hx(C_DIM)))
                    .child(Label::new("时长").text_size(px(12.)).text_color(hx(C_DIM)))
                    .child(Select::new(&self.dur_select).w(px(88.)).with_size(Size::Small))
                    .child(Label::new("比例").text_size(px(12.)).text_color(hx(C_DIM)))
                    .child(Select::new(&self.ratio_select).w(px(104.)).with_size(Size::Small))
                    .child(
                        Switch::new("seamless")
                            .checked(self.seamless)
                            .label("自动 crossfade 循环化")
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.seamless = *checked;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                Button::new("gen")
                    .primary()
                    .label(if busy { "RENDERING ▸▸▸" } else { "▸ RENDER · h3-fast-12 @0.7MP · ~7MIN" })
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.submit_generation(cx);
                    })),
            )
            .children(status.map(|(color, s)| {
                Label::new(format!("$ {s}")).text_size(px(12.)).text_color(color)
            }))
            .child(
                Label::new("// 投批 → 农场 inbox → 轮询 out/ → 拉回 → crossfade → 自动入库 (连接在「03 连接」配)")
                    .text_size(px(10.5))
                    .text_color(hx(C_DIM)),
            )
            .text_color(theme.foreground)
    }

    fn render_conn(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let conn = load_conn();
        v_flex()
            .p_4()
            .gap_2()
            .max_w(px(680.))
            .child(
                Label::new("HOLYCRAB SSH ▸ 生成投批用的连接凭据 (存本机 ~/.config/dynwall/, 不进仓库)")
                    .text_size(px(12.))
                    .text_color(hx(C_DIM)),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(Label::new("HOST").text_size(px(12.)).text_color(hx(C_DIM)))
                    .child(Input::new(&self.cfg_host).w(px(180.)))
                    .child(Label::new("USER").text_size(px(12.)).text_color(hx(C_DIM)))
                    .child(Input::new(&self.cfg_user).w(px(110.)))
                    .child(Label::new("PORT").text_size(px(12.)).text_color(hx(C_DIM)))
                    .child(Input::new(&self.cfg_port).w(px(72.))),
            )
            .child(
                Label::new("PRIVATE KEY ▸ 贴入 OpenSSH 私钥; 留空则沿用已保存的 key")
                    .text_size(px(12.))
                    .text_color(hx(C_DIM)),
            )
            .child(Textarea::new(&self.cfg_key).h(px(190.)))
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("save-conn")
                            .primary()
                            .label("保存凭据")
                            .with_size(Size::Small)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.save_conn(window, cx);
                            })),
                    )
                    .child(
                        Button::new("test-conn")
                            .label("测连")
                            .with_size(Size::Small)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.test_conn(cx);
                            })),
                    )
                    .child(
                        Button::new("clear-conn")
                            .danger()
                            .label("清除")
                            .with_size(Size::Small)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.clear_conn(window, cx);
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        Label::new(format!("MODE ▸ {}", conn.mode_label()))
                            .text_size(px(10.5))
                            .text_color(hx(C_AMBER)),
                    ),
            )
            .children(self.conn_status.clone().map(|(ok, s)| {
                Label::new(format!("$ {s}"))
                    .text_size(px(12.))
                    .text_color(if ok { hx(C_ACCENT) } else { hx(C_DANGER) })
            }))
            .child(
                Label::new("// 保存后「02 生成」即走此凭据; VM/新机器无需再拷 ~/.ssh")
                    .text_size(px(10.5))
                    .text_color(hx(C_DIM)),
            )
            .text_color(theme.foreground)
    }

    fn save_conn(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let host = self.cfg_host.read(cx).value().trim().to_string();
        let user = self.cfg_user.read(cx).value().trim().to_string();
        let port: u16 = self.cfg_port.read(cx).value().trim().parse().unwrap_or(22);
        let key_paste = self.cfg_key.read(cx).value().trim().to_string();
        if host.is_empty() || user.is_empty() {
            window.push_notification("❌ host / user 不能为空", cx);
            return;
        }
        if !key_paste.is_empty() && !key_paste.contains("PRIVATE KEY") {
            window.push_notification("❌ 私钥内容不对: 应为 -----BEGIN ... PRIVATE KEY----- 开头的整段", cx);
            return;
        }
        let dir = app_config_dir();
        if std::fs::create_dir_all(&dir).is_err()
            || std::fs::write(
                dir.join("holycrab.conf"),
                format!("host={host}\nuser={user}\nport={port}\n"),
            )
            .is_err()
        {
            window.push_notification("❌ 写 holycrab.conf 失败", cx);
            return;
        }
        if !key_paste.is_empty() {
            let kp = dir.join("id_holycrab");
            if std::fs::write(&kp, format!("{key_paste}\n")).is_err() {
                window.push_notification("❌ 写私钥失败", cx);
                return;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let _ = std::fs::set_permissions(&kp, std::fs::Permissions::from_mode(0o600));
            }
        }
        window.push_notification(format!("✅ 已保存 · {}", load_conn().short_label()), cx);
        cx.notify();
    }

    fn test_conn(&mut self, cx: &mut Context<Self>) {
        let conn = load_conn();
        self.conn_status = Some((true, "测连中…".into()));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let dest = conn.dest.clone();
            let r = smol::unblock(move || ssh_run(&conn, "echo ok")).await;
            this.update(cx, |t, cx| {
                t.conn_status = Some(match r {
                    Ok(s) if s.trim() == "ok" => (true, format!("✅ {dest} 连通, 投批链路可用")),
                    Ok(s) => (false, format!("❌ {dest} 异常回包: {}", s.trim())),
                    Err(e) => (false, format!("❌ {dest}: {e}")),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn clear_conn(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dir = app_config_dir();
        let _ = std::fs::remove_file(dir.join("holycrab.conf"));
        let _ = std::fs::remove_file(dir.join("id_holycrab"));
        window.push_notification(format!("已清除 UI 凭据, 回退 {}", load_conn().mode_label()), cx);
        cx.notify();
    }
}

fn main() {
    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        hack_theme(cx);
        let mut options = TitleBar::window_options();
        options.window_bounds = Some(WindowBounds::Windowed(Bounds::centered(
            None,
            gpui_kit::size(px(980.), px(680.)),
            cx,
        )));
        gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| DynWall::new(window, cx))
        })
        .expect("failed to open dynwall window");
    });
}
