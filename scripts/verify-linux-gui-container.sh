#!/bin/sh
# 在 Linux 容器里用软件渲染验收 GUI 产物：构建 GUI 宿主、追加载荷、在 Xvfb 下启动，
# 检查窗口真的创建出来并能截图。窄窗口与缩放通过 Xvfb 屏幕尺寸和缩放因子覆盖。
# 需要 docker；镜像内安装的都是构建/运行 GUI 所需的系统库，不是衡枢的构建依赖。
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
image=${HENGSHU_LINUX_IMAGE:-rust:slim}
screen=${HENGSHU_GUI_SCREEN:-640x480x24}
scale=${HENGSHU_GUI_SCALE:-2}

docker run --rm \
  -v "$root":/work \
  -v hengshu-linux-gui-target:/tmp/target-linux-gui \
  -w /work \
  -e CARGO_TARGET_DIR=/tmp/target-linux-gui \
  -e HENGSHU_GUI_SCREEN="$screen" \
  -e HENGSHU_GUI_SCALE="$scale" \
  "$image" \
  sh -eu -c '
export DEBIAN_FRONTEND=noninteractive
export LANG=C.UTF-8
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
  pkg-config libx11-dev libxcursor-dev libxrandr-dev libxi-dev libxkbcommon-dev \
  libxkbcommon-x11-dev x11-utils libwayland-dev libgl1-mesa-dev libgl1-mesa-dri xvfb xauth xdotool x11-apps \
  fonts-noto-cjk >/dev/null
cargo build --release --features gui --bin 衡枢 --bin 衡枢GUI
cli=/tmp/target-linux-gui/release/衡枢
work=$(mktemp -d)
mkdir -p "$work/结果"
cat > "$work/主程序.代码" <<PROGRAM
输出("界面验收")
PROGRAM
cat > "$work/衡枢.toml" <<MANIFEST
["项目"]
"名称" = "界面验收"
"版本" = "0.1.0"
"入口" = "主程序.代码"
"许可证" = "MIT"

["工具"."界面工具"]
version = 1
"名称" = "界面工具"
"工具版本" = "1"
"入口" = "main"
"参数" = { "长说明" = { type_name = "文本", required = false } }

["工具"."界面工具".gui]
title = "界面验收：中文长文本换行与窄窗口"
width = 520
height = 420
output_root = "结果"
MANIFEST
HENGSHU_GUI_BINARY=/tmp/target-linux-gui/release/衡枢GUI \
  "$cli" 打包 "$work" -o "$work/界面工具" --界面 --工具 界面工具
rm "$work/主程序.代码" "$work/衡枢.toml"
"$cli" 验证包 "$work/界面工具"
export LIBGL_ALWAYS_SOFTWARE=1
export WINIT_X11_SCALE_FACTOR="$HENGSHU_GUI_SCALE"
export WINIT_UNIX_BACKEND=x11
echo "--- 在 Xvfb（$HENGSHU_GUI_SCREEN，缩放 $HENGSHU_GUI_SCALE）下启动 GUI ---"
xvfb-run -a -s "-screen 0 $HENGSHU_GUI_SCREEN" sh -eu -c "
  \"$work/界面工具\" >/tmp/gui.log 2>&1 &
  gui=\$!
  if ! timeout 90 xdotool search --sync --onlyvisible --name . >/tmp/window-id; then
    echo \"--- 窗口未在 90 秒内出现，GUI 日志 ---\"
    cat /tmp/gui.log
    echo \"--- X 窗口树 ---\"
    xwininfo -root -tree 2>&1 | head -40
    kill \$gui 2>/dev/null || true
    exit 1
  fi
  window=\$(head -1 /tmp/window-id)
  xprop -id \$window _NET_WM_NAME | tee /tmp/window-name
  grep -q 界面验收 /tmp/window-name || {
    echo \"--- 窗口标题不是中文工具标题 ---\"
    cat /tmp/gui.log
    kill \$gui 2>/dev/null || true
    exit 1
  }
  xwininfo -id \$window | grep -E \"Width|Height\"
  xwd -root -silent > /tmp/screen.xwd
  sleep 2
  kill -0 \$gui
  kill \$gui
  wait \$gui 2>/dev/null || true
"
echo "窗口 ID：$(cat /tmp/window-id)"
ls -l /tmp/screen.xwd
[ -s /tmp/gui.log ] && { echo "--- GUI 日志 ---"; cat /tmp/gui.log; }
echo "--- Linux GUI 软件渲染验收通过 ---"
uname -sm
'
