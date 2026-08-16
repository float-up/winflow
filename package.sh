#!/usr/bin/env bash
#
# package.sh — 把 winflow 打包成 macOS .app 应用包
#
# 用法:
#   ./package.sh                # 构建并打包到 dist/winflow.app
#   ./package.sh --install      # 打包并拷贝到 /Applications
#   ./package.sh --open         # 打包并启动
#
# 产物: dist/winflow.app
# 依赖: cargo、Xcode 命令行工具（sips/iconutil/codesign，均为 macOS 自带）
#
set -euo pipefail
cd "$(dirname "$0")"

APP_NAME="winflow"
BUNDLE_ID="com.winflow.app"
VERSION="$(grep '^version' Cargo.toml | head -1 | sed -E 's/.*"([^"]+)".*/\1/')"
DIST="dist"
APP="$DIST/$APP_NAME.app"
ICON_SRC="assets/icon.png"
FLAG_INSTALL=0
FLAG_OPEN=0
for arg in "$@"; do
  case "$arg" in
    --install) FLAG_INSTALL=1 ;;
    --open)    FLAG_OPEN=1 ;;
    *) echo "未知参数: $arg" >&2; exit 1 ;;
  esac
done

echo "==> cargo build --release"
cargo build --release

echo "==> 组装 $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "target/release/$APP_NAME" "$APP/Contents/MacOS/$APP_NAME"

# ---- 应用图标: icon.png -> .icns（sips + iconutil，均为系统自带）----
ICON_SET="$APP/Contents/Resources/AppIcon.iconset"
ICON_ICNS="$APP/Contents/Resources/AppIcon.icns"
HAS_ICON=0
if [ -f "$ICON_SRC" ]; then
  echo "==> 生成 .icns 图标"
  mkdir -p "$ICON_SET"
  for size in 16 32 64 128 256 512 1024; do
    sips -z "$size" "$size" "$ICON_SRC" --out "$ICON_SET/icon_${size}x${size}.png" >/dev/null
    half=$((size / 2))
    sips -z "$size" "$size" "$ICON_SRC" --out "$ICON_SET/icon_${half}x${half}@2x.png" >/dev/null
  done
  iconutil -c icns "$ICON_SET" -o "$ICON_ICNS"
  rm -rf "$ICON_SET"
  HAS_ICON=1
else
  echo "!! 未找到 $ICON_SRC，跳过图标（应用将使用默认图标）"
fi

# ---- Info.plist ----
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundleDisplayName</key>
    <string>$APP_NAME</string>
    <key>CFBundleIdentifier</key>
    <string>$BUNDLE_ID</string>
    <key>CFBundleExecutable</key>
    <string>$APP_NAME</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleVersion</key>
    <string>$VERSION</string>
    <key>CFBundleShortVersionString</key>
    <string>$VERSION</string>
    <key>LSMinimumSystemVersion</key>
    <string>12.0</string>
    <key>LSUIElement</key>
    <true/>
    <key>NSHighResolutionCapable</key>
    <true/>
$(if [ "$HAS_ICON" = "1" ]; then
  cat <<SUB
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
SUB
fi)
</dict>
</plist>
PLIST

# ---- 签名（ad-hoc，保证 TCC 权限与启动器行为正常）----
echo "==> codesign (ad-hoc)"
codesign --force --sign - "$APP" >/dev/null

echo ""
echo "打包完成: $APP"
echo "  - 启动: open \"$APP\""
echo "  - 授权: 首次运行后在 系统设置 → 隐私与安全性 中为 winflow 勾选"
echo "          辅助功能 和 屏幕录制 权限"

if [ "$FLAG_INSTALL" = "1" ]; then
  echo "==> 拷贝到 /Applications"
  rm -rf "/Applications/$APP_NAME.app"
  cp -R "$APP" "/Applications/$APP_NAME.app"
  codesign --force --sign - "/Applications/$APP_NAME.app" >/dev/null
  echo "已安装: /Applications/$APP_NAME.app"
fi

if [ "$FLAG_OPEN" = "1" ]; then
  echo "==> 启动 $APP"
  open "$APP"
fi
