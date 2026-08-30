#!/bin/sh
# 在 amd64 容器里用 mingw 交叉编译 Windows CLI 产物，并用 Wine 验收
# 「打包 → 删除源码 → 运行 → 验证包」。Wine 不等于真实 Windows，
# 真机验收仍按 docs/发布清单.md 执行；这里的目标是让 PE 产物至少被真实执行过一次。
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
image=${HENGSHU_WINDOWS_IMAGE:-rust:slim}

docker run --rm --platform linux/amd64 \
  -v "$root":/work \
  -v hengshu-windows-target:/tmp/target-windows \
  -w /work \
  -e CARGO_TARGET_DIR=/tmp/target-windows \
  "$image" \
  sh -eu -c '
export DEBIAN_FRONTEND=noninteractive
export LANG=C.UTF-8
apt-get update -qq
apt-get install -y -qq --no-install-recommends mingw-w64 wine wine64 ca-certificates file >/dev/null
rustup target add x86_64-pc-windows-gnu >/dev/null
cargo build --release --target x86_64-pc-windows-gnu --bin 衡枢
# Wine 对非 ASCII 可执行名支持不稳，验收时统一用 ASCII 文件名。
cp /tmp/target-windows/x86_64-pc-windows-gnu/release/衡枢.exe /tmp/hengshu.exe
export WINEDEBUG=-all
export WINEPREFIX=/tmp/wine
wineboot --init >/dev/null 2>&1 || true
work=$(mktemp -d)
cat > "$work/主程序.代码" <<PROGRAM
输出(6 * 2)
PROGRAM
cat > "$work/衡枢.toml" <<MANIFEST
["项目"]
"名称" = "验收工具"
"版本" = "0.1.0"
"入口" = "主程序.代码"
"许可证" = "MIT"
MANIFEST
wine /tmp/hengshu.exe 打包 "$work" -o "$work/product.exe"
rm "$work/主程序.代码" "$work/衡枢.toml"
echo "--- 删除源码后运行 ---"
wine "$work/product.exe"
echo "--- 验证包 ---"
wine /tmp/hengshu.exe 验证包 "$work/product.exe"
file /tmp/target-windows/x86_64-pc-windows-gnu/release/衡枢.exe
uname -sm
echo "--- Windows CLI（Wine）验收通过 ---"
'
