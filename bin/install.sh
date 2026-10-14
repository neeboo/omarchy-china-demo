#!/usr/bin/env bash
# install.sh — omarchy 机器 / Try Omarchy VM 一键装依赖 + 编译 dynwall app
#
# 覆盖: 系统依赖 → mpvpaper (aarch64 源里没有, 自动源码编译) → koi-wallpaper →
#        中文字体 → rust 工具链 → app release 编译
# 用法:  ./bin/install.sh            # 全量
#        ./bin/install.sh --no-app   # 只装系统依赖, 不编译 app
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
NO_APP=0
[ "${1:-}" = "--no-app" ] && NO_APP=1

say()  { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m !! \033[0m %s\n' "$*"; }

ARCH="$(uname -m)"
say "omarchy demo 安装 (${ARCH})"

# ---- 1) 系统依赖 ----
say "安装系统依赖 (pacman, 全量升级)…"
sudo pacman -Syu --needed --noconfirm \
  base-devel git meson ninja \
  mpv ffmpeg jq fontconfig \
  noto-fonts-cjk pkgconf \
  wayland wayland-protocols

# ---- 2) mpvpaper: 官方源只有 x86_64, aarch64 自动源码编译 ----
if command -v mpvpaper >/dev/null 2>&1; then
  say "mpvpaper 已安装: $(command -v mpvpaper)"
else
  say "${ARCH} 源里没有 mpvpaper — 从源码编译 (C 项目, 1-2 分钟)…"
  rm -rf /tmp/mpvpaper-src
  git clone --depth 1 https://github.com/GhostNaN/mpvpaper /tmp/mpvpaper-src
  (cd /tmp/mpvpaper-src && meson setup build --buildtype=release && ninja -C build)
  sudo ninja -C /tmp/mpvpaper-src/build install
  command -v mpvpaper >/dev/null 2>&1 || sudo ldconfig
  command -v mpvpaper >/dev/null 2>&1 || { warn "mpvpaper 编译安装后仍未找到, 查看上方输出"; exit 1; }
  say "mpvpaper 源码编译完成: $(command -v mpvpaper)"
fi

# ---- 3) koi-wallpaper / gen-wallpaper 装进 PATH ----
say "安装 koi-wallpaper + gen-wallpaper → /usr/local/bin"
sudo install -m755 "$REPO/bin/koi-wallpaper" /usr/local/bin/
sudo install -m755 "$REPO/bin/gen-wallpaper" /usr/local/bin/

# ---- 4) 中文字体检查 (app 界面中文, 缺了全是空框) ----
if fc-list :lang=zh 2>/dev/null | grep -q .; then
  say "中文字体 OK"
else
  warn "没检测到中文字体 — noto-fonts-cjk 刚装, 注销重登一次会话即可"
fi

# ---- 5) 虚机提示 (VirGL 的 EGL 坏, 脚本会自动软件 GL, 这里只说明) ----
if systemd-detect-virt -q 2>/dev/null; then
  say "检测到虚机 (Try Omarchy/VirGL): koi-wallpaper 与 app 已内置软件 GL + X11 输出自适应, 无需手动设置"
fi

# ---- 6) rust 工具链 + app 编译 ----
if [ "$NO_APP" = "0" ]; then
  if ! command -v cargo >/dev/null 2>&1; then
    say "安装 rustup (minimal profile)…"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
    . "$HOME/.cargo/env"
  fi
  say "编译 dynwall app (release, 首次全量编译较久)…"
  (cd "$REPO/app" && cargo build --release)
  say "app 就绪: $REPO/app/target/release/dynwall"
fi

say "完成 — 跑 bin/start-demo.sh 启动演示"
