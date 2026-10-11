// dynwall — 动态壁纸更换器 (gpui-kit)
//
// 画廊(CC0/自有 loop 素材) + 生成页(prompt → DGX h3 farm → 下载 → 本地 crossfade
// 循环化) + 自动轮换。渲染后端: Linux 走 mpvpaper/koi-wallpaper, macOS 用 mpv/open 预览。
//
// 素材库目录: ~/Videos/wallpapers + $DYNWALL_SEED_DIR (默认本仓库 assets/)。
// 生成走 SSH 投批 (DYNWALL_DGX, 默认 dgx_spark), 复用 gen-wallpaper 的 inbox/out 链路。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use gpui_kit::base::h_flex;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    input::{Input, InputState},
    label::Label,
    select::{Select, SelectState},
    separator::Separator,
    switch::Switch,
    tab::{Tab, TabBar},
    ActiveTheme as _, Disableable as _, IndexPath, Sizable as _, Size, TitleBar, WindowExt as _,
    v_flex,
};
use gpui_kit::{
    div, img, px, AppContext as _, Bounds, Context, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, StyledImage as _, Window, WindowBounds,
};
use smol::Timer;

const FARM_DIR: &str = "/home/spark/h3_factory";
const PROJECT: &str = "omarchy-demo";
const POLL_SECS: u64 = 30;
const GEN_TIMEOUT_SECS: u64 = 1500;

fn dgx_host() -> String {
    std::env::var("DYNWALL_DGX").unwrap_or_else(|_| "dgx_spark".into())
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

// ---------- 素材库 ----------

struct WallEntry {
    path: PathBuf,
    name: String,
    thumb: Option<PathBuf>,
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
                out.push(Self { path: p, name, thumb });
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

// ---------- 应用上墙 / 预览 ----------

/// 上墙: Linux → koi-wallpaper (mpvpaper); macOS → mpv 循环 / open 兜底。
fn apply_wallpaper(video: &Path) -> anyhow::Result<()> {
    let v = video.to_string_lossy().into_owned();
    #[cfg(target_os = "linux")]
    {
        let ok = Command::new("koi-wallpaper").arg("on").arg(&v).spawn().is_ok();
        if !ok {
            Command::new("mpvpaper")
                .args(["-o", "loop mute=yes panscan=1.0 hwdec=auto"])
                .arg("*")
                .arg(&v)
                .spawn()?;
        }
        Ok(())
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

fn ssh_run(host: &str, cmd: &str) -> anyhow::Result<String> {
    let out = Command::new("ssh")
        .args(["-o", "ConnectTimeout=10", host, cmd])
        .output()?;
    if !out.status.success() {
        anyhow::bail!("ssh {}: {}", host, String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn ssh_write_manifest(host: &str, remote: &str, content: &str) -> anyhow::Result<()> {
    use std::io::Write as _;
    let mut child = Command::new("ssh")
        .arg(host)
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

fn scp_pull(host: &str, remote: &str, local: &Path) -> anyhow::Result<()> {
    let out = Command::new("scp")
        .args(["-q", &format!("{host}:{remote}"), &local.to_string_lossy()])
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
    prompt: Entity<InputState>,
    dur_select: Entity<SelectState<Vec<&'static str>>>,
    ratio_select: Entity<SelectState<Vec<SharedString>>>,
    seamless: bool,
    gen: Option<GenJob>,
    rotate: bool,
    rotate_min: Entity<SelectState<Vec<&'static str>>>,
    rotate_tick: u64,
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
        let this = Self {
            tab_ix: 0,
            entries,
            selected: 0,
            prompt,
            dur_select,
            ratio_select,
            seamless: true,
            gen: None,
            rotate: false,
            rotate_min,
            rotate_tick: 0,
        };

        // 封面帧后台补齐
        cx.spawn(async move |this, cx| {
            let missing: Vec<(PathBuf, PathBuf)> = this.update(cx, |t, _| {
                t.entries
                    .iter()
                    .filter(|e| e.thumb.is_none())
                    .map(|e| (e.path.clone(), thumb_for(&e.path).unwrap_or_else(|| {
                        let mut h = DefaultHasher::new();
                        e.path.hash(&mut h);
                        thumb_dir().join(format!("{:016x}.png", h.finish()))
                    })))
                    .collect()
            })?;
            for (video, thumb) in missing {
                if make_thumb(&video, &thumb).is_ok() {
                    this.update(cx, |t, cx| {
                        if let Some(e) = t.entries.iter_mut().find(|e| e.path == video) {
                            e.thumb = Some(thumb.clone());
                        }
                        cx.notify();
                    })?;
                }
            }
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
                            let _ = apply_wallpaper(&e.path);
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

        let host = dgx_host();
        cx.spawn(async move |this, cx| {
            // 1) 投批
            let remote_manifest = format!("{FARM_DIR}/inbox/60_{id}.jsonl");
            let host0 = host.clone();
            let r = smol::unblock({
                let id = id.clone();
                move || ssh_write_manifest(&host0, &remote_manifest, &manifest)
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
                    let host = host.clone();
                    move || {
                        ssh_run(&host,
                            &format!("ls {FARM_DIR}/out/ 2>/dev/null | grep -F '{PROJECT}_{id}_' | grep '\\.mp4$' | head -1"))
                            .or_else(|_| {
                                // 区分失败: worker 判退/失败记日志
                                ssh_run(&host,
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
                let host = host.clone();
                move || scp_pull(&host, &format!("{FARM_DIR}/out/{product}"), &raw)
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
            .child(Tab::new().label("画廊"))
            .child(Tab::new().label("生成"));

        let body = match self.tab_ix {
            0 => self.render_gallery(window, cx).into_any_element(),
            _ => self.render_generate(cx).into_any_element(),
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(TitleBar::new().child("动态壁纸 · DynWall").child(tab_bar))
            .child(div().flex_1().min_h_0().child(body))
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
            .overflow_y_scroll();
        if self.entries.is_empty() {
            grid = grid.child(
                Label::new("素材库为空 — 把 loop mp4 放进 ~/Videos/wallpapers/ 或点「生成」")
                    .text_color(theme.muted_foreground),
            );
        }
        for (ix, e) in self.entries.iter().enumerate() {
            let selected = ix == self.selected;
            let mut card = div()
                .id(ix)
                .w(px(240.))
                .flex()
                .flex_col()
                .rounded(theme.radius_lg)
                .border_1()
                .cursor_pointer()
                .overflow_hidden()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.selected = ix;
                    cx.notify();
                    if let Some(e) = this.entries.get(ix) {
                        window.push_notification(format!("已选 {}", e.name), cx);
                    }
                }));
            card = if selected {
                card.border_color(theme.primary).border_2()
            } else {
                card.border_color(theme.border)
            };
            let poster: Box<dyn Fn() -> gpui_kit::AnyElement> = match &e.thumb {
                Some(t) => {
                    let t = t.clone();
                    Box::new(move || img(t.clone()).w_full().h(px(135.)).object_fit(gpui_kit::ObjectFit::Cover).into_any_element())
                }
                None => Box::new(|| div().w_full().h(px(135.)).into_any_element()),
            };
            card = card.child(poster()).child(
                div()
                    .px_2()
                    .py_1()
                    .text_sm()
                    .text_ellipsis()
                    .child(e.name.clone()),
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
            .border_b_1()
            .border_color(theme.border)
            .child(Label::new(format!("当前: {selected_name}")).text_sm().flex_1())
            .child(
                Button::new("apply")
                    .primary()
                    .label("上墙")
                    .on_click(cx.listener(|this, _, window, cx| {
                        match this.entries.get(this.selected) {
                            Some(e) => match apply_wallpaper(&e.path) {
                                Ok(()) => window.push_notification(format!("已上墙 {}", e.name), cx),
                                Err(er) => window.push_notification(format!("上墙失败: {er}"), cx),
                            },
                            None => window.push_notification("先选一张素材", cx),
                        }
                    })),
            )
            .child(
                Button::new("preview")
                    .label("预览")
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
                    .on_click(cx.listener(|_, _, window, cx| {
                        stop_wallpaper();
                        window.push_notification("已停, 恢复静态壁纸", cx);
                    })),
            )
            .child(Separator::horizontal())
            .child(Label::new("轮换").text_sm())
            .child(
                Switch::new("rotate")
                    .checked(self.rotate)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.rotate = *checked;
                        this.rotate_tick = 0;
                        cx.notify();
                    })),
            )
            .child(Select::new(&self.rotate_min).w(px(96.)).with_size(Size::Small));

        v_flex().size_full()
            .child(toolbar)
            .child(div().flex_1().min_h_0().child(grid))
    }

    fn render_generate(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let status = self.gen.as_ref().map(|g| {
            let el = self.elapsed_text();
            let time = if el.is_empty() { String::new() } else { format!(" ({el})") };
            format!("{}{time}", g.phase.text())
        });

        v_flex()
            .p_4()
            .gap_3()
            .max_w(px(640.))
            .child(Label::new("一句话描述想要的壁纸 (T2VA 直出 + 本地 crossfade 无缝循环)").text_sm().text_color(theme.muted_foreground))
            .child(Input::new(&self.prompt))
            .child(
                h_flex().items_center().gap_3()
                    .child(Label::new("时长").text_sm())
                    .child(Select::new(&self.dur_select).w(px(96.)))
                    .child(Label::new("比例").text_sm())
                    .child(Select::new(&self.ratio_select).w(px(112.)))
                    .child(
                        Switch::new("seamless")
                            .checked(self.seamless)
                            .label("落库自动 crossfade 循环化")
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.seamless = *checked;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                Button::new("gen")
                    .primary()
                    .label(if self.gen.as_ref().is_some_and(|g| g.phase.busy()) { "投批中/渲染中…" } else { "生成 (约 7-8 分钟)" })
                    .disabled(self.gen.as_ref().is_some_and(|g| g.phase.busy()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.submit_generation(cx);
                    })),
            )
            .children(status.map(|s| Label::new(s).text_sm()))
    }
}

fn main() {
    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
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
