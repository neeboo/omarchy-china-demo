#!/usr/bin/env bash
# start-demo.sh — 一键启动: 保底锦鲤循环上墙 + dynwall app
# 前置: 先跑过 bin/install.sh
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$REPO/app/target/release/dynwall"

[ -x "$BIN" ] || { echo "app 未编译 — 先跑 bin/install.sh" >&2; exit 1; }

# 保底素材先上墙 (有 koi-wallpaper 才挂; 没有就 app 里手动上)
LOOP="$(ls "$REPO"/assets/koi_loop*.mp4 2>/dev/null | head -1 || true)"
if [ -n "$LOOP" ] && command -v koi-wallpaper >/dev/null 2>&1; then
  echo "保底素材上墙: $LOOP"
  koi-wallpaper on "$LOOP" || true
fi

echo "启动 dynwall…"
exec "$BIN"
