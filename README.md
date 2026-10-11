# Omarchy 中国行 Demo — h3 生成式动态壁纸

把 h3 视频生成农场变成动态壁纸素材源:文本 prompt → DGX 渲染 → 无缝循环 →
直接上桌面。预生成精品素材保底,现场投一单看它几分钟后自动上墙。

## 布局

```
bin/koi-wallpaper    # omarchy 机器端:mpvpaper 播放/停止/切换(自研,不改 waybar 配置)
bin/gen-wallpaper    # Mac 端现场生成闭环:prompt → 投批 → 等片 → 拉回 → 循环化 → 上墙
assets/              # 成品素材(loop 版)
manifests/           # 投批 JSONL 存档
app/                 # dynwall — gpui-kit 动态壁纸更换器(画廊/生成/轮换, 见下)
```

## dynwall — gpui-kit 壁纸更换器 app

`app/` 是一个 Rust 桌面 app(`gpui-kit = "0.7"`,longbridge/gpui-kit 组件库):

- **画廊**:扫 `~/Videos/wallpapers/` + 仓库 `assets/`(可用 `DYNWALL_SEED_DIR` 覆盖),
  ffmpeg 抽封面帧(缓存于 `~/.cache/dynwall/thumbs/`),点选 → 上墙/预览/停
- **生成**:一句话 prompt → SSH 投批 DGX 农场(`DYNWALL_DGX`,默认 `dgx_spark`,
  与 gen-wallpaper 同一 inbox/out 链路)→ 轮询 → 拉回 → 本地 crossfade 循环化 →
  自动入库刷新画廊(约 7-8 分钟)
- **自动轮换**:开关 + 1/3/5/10/30 分钟间隔,到点切下一张并上墙
- 上墙后端:Linux 走 `koi-wallpaper`(mpvpaper);macOS 预览用 mpv 循环/open 兜底

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

## omarchy 机器一次性准备

```bash
sudo pacman -S --needed mpvpaper jq ffmpeg
sudo install -m755 bin/koi-wallpaper /usr/local/bin/
```

Mac 端预览:Plash(brew install plash)→ 指向 loop mp4 的 file:// URL。

## 已知边界

- M1 Max 跑不了 Omarchy(Asahi 无 Hyprland + 无外接显示器),现场需任意 x86 机器
- 1.0MP 档强制 5min 间隔 + 白天热学受限;demo 用 0.7MP 档全天安全
- h3 成品默认进 COS;本 demo 走 DGX 本地 inbox 投递,out/ 直取,不经公网
