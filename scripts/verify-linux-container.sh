#!/bin/sh
# 在 Linux 容器里验证 CLI 产物：Release 构建、追加载荷、删除源码后运行、验证包。
# 只依赖 docker 与官方 rust 镜像，不引入新的构建依赖；GUI 窗口启动仍需真实图形会话。
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
image=${HENGSHU_LINUX_IMAGE:-rust:slim}

docker run --rm \
  -v "$root":/work \
  -w /work \
  -e CARGO_TARGET_DIR=/tmp/target-linux \
  "$image" \
  sh -eu -c '
cargo build --release --bin 衡枢
binary=/tmp/target-linux/release/衡枢
work=$(mktemp -d)
mkdir -p "$work/结果"
cat > "$work/主程序.代码" <<PROGRAM
定义 数量 为 3
输出(数量 * 4)
PROGRAM
cat > "$work/衡枢.toml" <<MANIFEST
["项目"]
"名称" = "Linux验收"
"版本" = "0.1.0"
"入口" = "主程序.代码"
"许可证" = "MIT"

["工具"."验收工具"]
version = 1
"名称" = "验收工具"
"工具版本" = "1"
"入口" = "main"
"参数" = {}

["工具"."验收工具".cli]
command = "验收工具"
output_format = "text"
output_root = "结果"
MANIFEST
"$binary" 打包 "$work" -o "$work/验收工具"
rm "$work/主程序.代码" "$work/衡枢.toml"
echo "--- 删除源码后运行 ---"
cd "$work" && ./验收工具
echo "--- 验证包 ---"
"$binary" 验证包 "$work/验收工具"
echo "--- Linux CLI 验收通过 ---"
uname -sm
'
