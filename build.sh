#!/bin/bash
#
# EasyTier Android (arm64) 一键编译并安装到手机脚本
#
# 用法:
#   ./build.sh            # 编译 easytier-arm64.apk 并尝试安装到已连接的 adb 设备
#   ./build.sh --no-install   # 只编译,不安装
#
# 依赖 (需提前准备):
#   - rustup + aarch64-linux-android target  (install: curl https://sh.rustup.rs | sh)
#   - cargo install cargo-ndk
#   - pnpm (前端依赖安装)
#   - Android SDK (见下方 ANDROID_SDK_ROOT) 且已安装 NDK 26.1.10909125
#     sdkmanager "ndk;26.1.10909125"
#   - JDK 17 (Gradle 8.x 不兼容 JDK 21+)
#
set -euo pipefail

# ---------------------------------------------------------------------------
# 1. 环境定位
# ---------------------------------------------------------------------------
REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "$REPO_ROOT"

# ---------------------------------------------------------------------------
# 2. Android / Rust 编译环境变量
# ---------------------------------------------------------------------------
export JAVA_HOME="${JAVA_HOME:-/usr/lib/jvm/java-17-openjdk}"
export ANDROID_SDK_ROOT="${ANDROID_SDK_ROOT:-$HOME/Android/Sdk}"
export ANDROID_NDK_ROOT="${ANDROID_NDK_ROOT:-$ANDROID_SDK_ROOT/ndk/26.1.10909125}"
export NDK_HOME="$ANDROID_NDK_ROOT"
export ANDROID_HOME="$ANDROID_SDK_ROOT"   # 部分工具只读 ANDROID_HOME
export PATH="$HOME/.cargo/bin:$JAVA_HOME/bin:$ANDROID_SDK_ROOT/cmdline-tools/latest/bin:$ANDROID_SDK_ROOT/platform-tools:$PATH"
export LIBCLANG_PATH="$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/lib"
export ZSTD_SYS_STATIC=1
export KCP_SYS_EXTRA_HEADER_PATH="$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/lib/clang/17/include"
export BINDGEN_EXTRA_CLANG_ARGS="--sysroot=$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/sysroot -isystem $ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/include -I$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/lib/clang/17/include -isystem $ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/lib/clang/17/include"

# 加载 cargo 环境 (rustup)
# shellcheck disable=SC1090
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"

# ---------------------------------------------------------------------------
# 3. 前置检查
# ---------------------------------------------------------------------------
err=0
command -v cargo >/dev/null 2>&1 || { echo "[错误] 未找到 cargo,请先安装 rustup"; err=1; }
command -v pnpm  >/dev/null 2>&1 || { echo "[错误] 未找到 pnpm,请先安装"; err=1; }
[ -d "$ANDROID_NDK_ROOT" ] || { echo "[错误] 未找到 NDK: $ANDROID_NDK_ROOT (请 sdkmanager \"ndk;26.1.10909125\")"; err=1; }
[ -d "$JAVA_HOME" ] || { echo "[错误] 未找到 JDK: $JAVA_HOME"; err=1; }
[ "$err" -eq 0 ] || exit 1

rustup target add aarch64-linux-android >/dev/null 2>&1 || true

# ---------------------------------------------------------------------------
# 4. 补丁: proto 字段号冲突 (peers 固定为 68 以兼容上游, http_proxy 不得占用 68)
#    若 http_proxy 仍占用 68,则改到 72; 绝不改动 peers 的字段号。
# ---------------------------------------------------------------------------
PROTO_FILE="$REPO_ROOT/easytier-proto/proto/api_manage.proto"
if grep -q "optional string http_proxy = 68;" "$PROTO_FILE" 2>/dev/null; then
  echo "[补丁] 修复 api_manage.proto 字段号冲突 (http_proxy 68 -> 72)"
  sed -i 's/optional string http_proxy = 68;/optional string http_proxy = 72;/' "$PROTO_FILE"
fi
if grep -q "repeated NetworkPeerConfig peers = 68;" "$PROTO_FILE" 2>/dev/null; then
  echo "[补丁] 确认 peers 字段号保持 68 (兼容上游)"
fi

# ---------------------------------------------------------------------------
# 5. 补丁: kcp-sys build.rs 给 Android target 追加 API level (-24)
#    bindgen 在 NDK 下需要 --target=aarch64-linux-android24 才能解析 stdint.h
# ---------------------------------------------------------------------------
KCP_BUILD="$(find "$HOME/.cargo/git/checkouts" -path '*kcp-sys-*/d7427c2/build.rs' 2>/dev/null | head -1)"
if [ -n "$KCP_BUILD" ] && ! grep -q 'ends_with("-linux-android")' "$KCP_BUILD"; then
  echo "[补丁] 修复 kcp-sys build.rs 的 bindgen target (追加 -24 API level)"
  python3 - "$KCP_BUILD" <<'PY'
import sys
f = sys.argv[1]
s = open(f).read()
old = '''    if target.starts_with("riscv64gc-") {
        args.push("-march=rv64gc".to_owned());
    }
    args
}'''
new = '''    if target.starts_with("riscv64gc-") {
        args.push("-march=rv64gc".to_owned());
    }
    if target.ends_with("-linux-android") {
        args[0] = format!("--target={}-24", clang_target(target));
    }
    args
}'''
assert old in s, "kcp-sys build.rs 补丁锚点未找到,可能版本已变化"
open(f, "w").write(s.replace(old, new))
print("patched", f)
PY
fi

# ---------------------------------------------------------------------------
# 6. 编译 Rust 库 + 前端 + universal APK (tauri 会构建并软链 libapp_lib.so)
# ---------------------------------------------------------------------------
echo "==> 构建 Android (arm64) 中..."
cd "$REPO_ROOT/easytier-gui"
pnpm tauri android build --target aarch64

# ---------------------------------------------------------------------------
# 7. 单独产出 arm64-only APK (跳过 rust 任务,复用上一步已构建/软链的 .so)
# ---------------------------------------------------------------------------
cd "$REPO_ROOT/easytier-gui/src-tauri/gen/android"
./gradlew :app:assembleArm64Release -x :app:rustBuildArm64Release

APK="$(find app/build/outputs/apk/arm64 -name 'app-arm64-release.apk' | head -1)"
[ -n "$APK" ] || { echo "[错误] 未找到 arm64 APK"; exit 1; }

OUT_APK="$REPO_ROOT/easytier-arm64.apk"
cp "$APK" "$OUT_APK"
echo "==> 产物: $OUT_APK ($(du -h "$OUT_APK" | cut -f1))"

# ---------------------------------------------------------------------------
# 8. 安装到手机
# ---------------------------------------------------------------------------
if [ "${1:-}" = "--no-install" ]; then
  echo "==> 已完成编译 (--no-install,跳过安装)"
  exit 0
fi

if ! command -v adb >/dev/null 2>&1; then
  echo "[警告] 未找到 adb,跳过安装"
  exit 0
fi

DEVICES="$(adb devices | awk 'NR>1 && $2=="device" {print $1}')"
if [ -z "$DEVICES" ]; then
  echo "[警告] 未检测到已连接的 adb 设备,跳过安装"
  echo "        连接手机后执行: adb install -r $OUT_APK"
  exit 0
fi

for dev in $DEVICES; do
  echo "==> 安装到设备 $dev"
  adb -s "$dev" install -r "$OUT_APK"
done

echo "==> 完成"
