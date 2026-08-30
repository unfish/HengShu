#!/bin/sh
set -eu

usage() {
    echo "用法：scripts/package-platform.sh <程序.代码或项目目录> <工具名> <输出目录> [--skip-appimage]" >&2
    exit 2
}

[ "$#" -ge 3 ] || usage
INPUT=$1
TOOL=$2
OUTPUT=$3
SKIP_APPIMAGE=0
[ "${4:-}" = "--skip-appimage" ] && SKIP_APPIMAGE=1

case "$(uname -s)" in
    Darwin) PLATFORM=macos ;;
    Linux) PLATFORM=linux ;;
    MINGW*|MSYS*|CYGWIN*) echo "Windows 请使用 scripts/package-platform.ps1" >&2; exit 2 ;;
    *) echo "不支持的平台：$(uname -s)" >&2; exit 2 ;;
esac

if [ -d "$INPUT" ]; then
    INPUT=$(CDPATH= cd -- "$INPUT" && pwd)
    PROJECT_ROOT=$INPUT
else
    INPUT_DIR=$(CDPATH= cd -- "$(dirname -- "$INPUT")" && pwd)
    INPUT=$INPUT_DIR/$(basename -- "$INPUT")
    PROJECT_ROOT=$INPUT_DIR
fi
while [ ! -f "$PROJECT_ROOT/衡枢.toml" ] && [ "$PROJECT_ROOT" != / ]; do
    PROJECT_ROOT=$(dirname -- "$PROJECT_ROOT")
done
[ -f "$PROJECT_ROOT/衡枢.toml" ] || { echo "找不到衡枢.toml" >&2; exit 1; }
REPO_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

NAME=$(basename -- "$INPUT")
NAME=${NAME%.代码}
ARCH=$(uname -m)
DEST=$OUTPUT/$PLATFORM-$ARCH
BUILD=$REPO_ROOT/target/release
mkdir -p "$DEST"

(cd "$REPO_ROOT" && cargo build --release --bin 衡枢)
(cd "$REPO_ROOT" && cargo build --release --features gui --bin 衡枢GUI)

CLI_HOST=$BUILD/衡枢
GUI_HOST=$BUILD/衡枢GUI
CLI_PAYLOAD=$DEST/$NAME-cli
GUI_PAYLOAD=$DEST/$NAME-gui

"$CLI_HOST" 打包 "$INPUT" --命令行 --工具 "$TOOL" -o "$CLI_PAYLOAD"
HENGSHU_GUI_BINARY="$GUI_HOST" "$CLI_HOST" 打包 "$INPUT" --界面 --工具 "$TOOL" -o "$GUI_PAYLOAD"
"$CLI_HOST" 验证包 "$CLI_PAYLOAD"
"$CLI_HOST" 验证包 "$GUI_PAYLOAD"

GUI_ARTIFACT=$NAME-gui
if [ "$PLATFORM" = macos ]; then
    APP=$DEST/$NAME.app
    # Keep the Mach-O filename ASCII. macOS 26 has a CoreFoundation launch
    # crash for ad-hoc signed bundles whose CFBundleExecutable contains CJK.
    APP_EXECUTABLE=hengshu_gui
    PLIST=$APP/Contents/Info.plist
    mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
    mv "$GUI_PAYLOAD" "$APP/Contents/MacOS/$APP_EXECUTABLE"
    /usr/bin/plutil -create xml1 "$PLIST"
    /usr/bin/plutil -insert CFBundleDevelopmentRegion -string zh_CN "$PLIST"
    /usr/bin/plutil -insert CFBundleDisplayName -string "$NAME" "$PLIST"
    /usr/bin/plutil -insert CFBundleExecutable -string "$APP_EXECUTABLE" "$PLIST"
    /usr/bin/plutil -insert CFBundleIdentifier -string "com.hengshu.tool" "$PLIST"
    /usr/bin/plutil -insert CFBundleInfoDictionaryVersion -string 6.0 "$PLIST"
    /usr/bin/plutil -insert CFBundleName -string "$NAME" "$PLIST"
    /usr/bin/plutil -insert CFBundlePackageType -string APPL "$PLIST"
    /usr/bin/plutil -insert CFBundleShortVersionString -string 0.9.0 "$PLIST"
    /usr/bin/plutil -insert CFBundleVersion -string 0.9.0 "$PLIST"
    /usr/bin/plutil -insert LSMinimumSystemVersion -string 11.0 "$PLIST"
    /usr/bin/plutil -insert NSHighResolutionCapable -bool true "$PLIST"
    chmod 755 "$APP/Contents/MacOS/$APP_EXECUTABLE"
    if [ -n "${HENGSHU_SIGN_IDENTITY:-}" ]; then
        /usr/bin/codesign --force --deep --options runtime --timestamp \
            --sign "$HENGSHU_SIGN_IDENTITY" "$APP"
    else
        /usr/bin/codesign --force --deep --sign - --timestamp=none "$APP"
        echo "提示：使用 ad-hoc 签名，仅供本机验证；对外分发需设置 HENGSHU_SIGN_IDENTITY 并公证" >&2
    fi
    GUI_ARTIFACT=$NAME.app
elif [ "$PLATFORM" = linux ]; then
    APPDIR=$DEST/$NAME.AppDir
    mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/applications"
    mv "$GUI_PAYLOAD" "$APPDIR/usr/bin/$NAME"
    sed "s/__APP_NAME__/$NAME/g" "$REPO_ROOT/scripts/linux-app.desktop.in" > "$APPDIR/$NAME.desktop"
    cp "$APPDIR/$NAME.desktop" "$APPDIR/usr/share/applications/$NAME.desktop"
    sed "s/__APP_NAME__/$NAME/g" "$REPO_ROOT/scripts/linux-AppRun.in" > "$APPDIR/AppRun"
    chmod 755 "$APPDIR/AppRun" "$APPDIR/usr/bin/$NAME"
    GUI_ARTIFACT=$NAME.AppDir
    if [ "$SKIP_APPIMAGE" -eq 0 ]; then
        command -v appimagetool >/dev/null 2>&1 || {
            echo "Linux AppImage 构建需要先安装 appimagetool，或传 --skip-appimage 只生成 AppDir" >&2
            exit 1
        }
        ARCH="$ARCH" appimagetool "$APPDIR" "$DEST/$NAME-gui.AppImage"
        GUI_ARTIFACT=$NAME-gui.AppImage
    fi
fi

(cd "$REPO_ROOT" && cargo metadata --format-version 1 --locked) > "$DEST/DEPENDENCIES.cargo-metadata.json"
cat > "$DEST/BUILD-MANIFEST" <<EOF
platform=$PLATFORM
architecture=$ARCH
package=$NAME
tool=$TOOL
source=$INPUT
rust=$(rustc --version)
cli=$NAME-cli
gui=$GUI_ARTIFACT
EOF

if command -v sha256sum >/dev/null 2>&1; then
    (cd "$DEST" && find . -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 sha256sum) > "$DEST/SHA256SUMS"
else
    find "$DEST" -type f ! -name SHA256SUMS -exec shasum -a 256 {} \; | sed "s#  $DEST/#  #" | sort > "$DEST/SHA256SUMS"
fi
echo "平台产物已生成：$DEST"
