# Omarchy 中国行 Demo — h3 生成式动态壁纸

把 h3 视频生成农场变成动态壁纸素材源:文本 prompt → holycrab 云端渲染 → 无缝循环 →
直接上桌面。预生成精品素材保底,现场投一单看它几分钟后自动上墙。

## 布局

```
bin/koi-wallpaper    # omarchy 机器端:mpvpaper 播放/停止/切换(自研,不改 waybar 配置)
bin/gen-wallpaper    # 现场生成闭环:prompt → 投批 → 等片 → 拉回 → 循环化 → 上墙
assets/              # 成品素材(loop 版)
manifests/           # 投批 JSONL 存档
app/                 # dynwall — gpui-kit 动态壁纸更换器(画廊/生成/连接/轮换, 见下)
```

## dynwall — gpui-kit 壁纸更换器 app

`app/` 是一个 Rust 桌面 app(`gpui-kit = "0.7"`,longbridge/gpui-kit 组件库):

- **画廊**:扫 `~/Videos/wallpapers/` + 仓库 `assets/`(可用 `DYNWALL_SEED_DIR` 覆盖),
  ffmpeg 抽封面帧(缓存于 `~/.cache/dynwall/thumbs/`),点选 → 上墙/预览/停
- **生成**:一句话 prompt → SSH 投批 holycrab 农场(与 gen-wallpaper 同一 inbox/out
  链路)→ 轮询 → 拉回 → 本地 crossfade 循环化 → 自动入库刷新画廊(约 7-8 分钟)
- **连接**:farm 的 SSH 凭据直接在 app 里配 — host/user/port + 粘贴私钥 → 保存 →
  测连。存 `~/.config/dynwall/holycrab.conf` + `id_holycrab`(0600),凭据不进仓库;
  新机器(如 VM)开箱贴一次 key 即可,不用再拷 `~/.ssh`。未配置时回退 `~/.ssh`
  alias(`DYNWALL_FARM`,默认 `holycrab`;旧 `DYNWALL_DGX` 仍兼容)
- **自动轮换**:开关 + 1/3/5/10/30 分钟间隔,到点切下一张并上墙
- 上墙后端:Linux 走 `koi-wallpaper`(mpvpaper),缺了自动退 mpvpaper → mpv;
  macOS 预览用 mpv 循环/open 兜底

```bash
cd app && cargo run --release
```

> 生成链路复用农场 SSH 投批,不在 app 内存任何 API key;FL2VA 首尾同帧档
> 仍走 `bin/gen-wallpaper --keyframe`(需要挑关键帧,脚本链路更顺手)。

## 现场演示节奏(约 10 分钟)

1. 桌面先挂着预生成的锦鲤循环素材(koi-wallpaper on assets/koi_loop.mp4)
2. 现场收集一个主题词,写入一句话 prompt:
   `gen-wallpaper "雨夜霓虹的重庆洪崖洞,江面倒影流动,竖版"`
3. 脚本投批 h3-fast-12 @0.7MP(~7 分钟渲染),期间继续讲别的
4. 出片自动拉回 + 尾首 crossfade 循环化 + `--push` 推到 omarchy 机器上墙

## 素材规格

- 16:9 桌面走 `h3-fast-12`(lightx2v v1.2,4 步)@0.7MP/10s ≈ 7.6 min 渲染
- **无缝循环主路线 = FL2VA(首尾同帧)**:`first_frame == last_frame` 指向同一张
  关键帧 → 模型被约束在"从这张图出发、回到这张图收尾"的路径上,接缝按构造≈0。
  纯 T2VA 直出不回环(两个种子实测接缝 p99 = 113/131 of 255,鱼群位置收不回来)
- `raw:true` 直出原片;`extra_lora: Motion_Repair` @0.9 压中途鱼形崩坏
- 现场快速档(T2VA + 本地 ffmpeg 尾→首 0.75s crossfade)仍保留在 `gen-wallpaper`,
  接缝靠融合遮,胜在不用抽关键帧、单轮出片
- 接缝验收:`bin/seam-check <video>`(按帧号抽首尾,勿用 `-ss` 时间戳——越过末帧
  pts 会静默回落到第 0 帧,报假 0 接缝)

## omarchy 机器一次性准备(一键脚本)

```bash
git pull
./bin/install.sh          # 装全部依赖 + 源码编译 mpvpaper(aarch64)+ 编译 app
./bin/start-demo.sh       # 保底锦鲤上墙 + 启动 dynwall
```

脚本会:

- 装齐 base-devel / mpv / ffmpeg / jq / noto-fonts-cjk / pkgconf 等
- **aarch64(Try Omarchy VM)官方源没有 mpvpaper → 自动从源码编译**(C 项目
  1-2 分钟);x86_64 机器直接用 pacman 包
- 装 koi-wallpaper / gen-wallpaper 到 /usr/local/bin
- 检查中文字体(app 界面中文缺 `noto-fonts-cjk` 会全是空框)
- 虚机里自动启用软件 GL(`LIBGL_ALWAYS_SOFTWARE`)——VirGL 的 EGL 配置匹配
  不上(`EGL_BAD_MATCH`),软件渲染绕过;koi-wallpaper 与 app 均内置该自适应

「03 连接」页贴一次 farm 私钥即可,不用拷 `~/.ssh`。

Mac 端预览:Plash(brew install plash)→ 指向 loop mp4 的 file:// URL。

## 已知边界

- Try Omarchy VM 无硬解(CPU 软解 736p 无压力),VirGL 仅 OpenGL;裸金属最顺
- 1.0MP 档强制 5min 间隔 + 白天热学受限;demo 用 0.7MP 档全天安全
- h3 成品默认进 COS;本 demo 走 farm 内网 inbox 投递,out/ 直取,不经公网
