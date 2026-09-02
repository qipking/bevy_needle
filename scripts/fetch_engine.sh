#!/usr/bin/env bash
# ============================================================================
# fetch_engine.sh — 获取 needle2 预构建引擎（libneedle）并解压到 bevy_needle
# 与官方 Python 绑定共用的缓存路径。
#
# 引擎是进程内推理的唯一产物（约 14 MB，权重已烘焙，之后零网络）。
#
# 用法：
#   ./scripts/fetch_engine.sh                      # 在线下载当前平台引擎
#   NEEDLE_VERSION=2.0.3 ./scripts/fetch_engine.sh
#   ./scripts/fetch_engine.sh --platform linux-x86_64    # 交叉获取其他平台
#   ./scripts/fetch_engine.sh --wheel /path/to/cactus_needle-*.whl
#                                                  # 离线模式：从本地 wheel 解压
#   ./scripts/fetch_engine.sh --out ./third_party/needle  # 自定输出根（默认缓存）
#   ./scripts/fetch_engine.sh --list               # 列出全部可用平台
#   ./scripts/fetch_engine.sh --force              # 已存在也重新下载
#
# 输出布局（默认）：
#   ~/.cache/cactus-needle/<version>/libneedle.{so|dylib|dll}
#
# 平台矩阵（wheel 构建共 8 种；其余为上游 standalone runner 平台，见 --list）：
#   linux-x86_64   manylinux2014_x86_64   libneedle.so
#   linux-arm64    manylinux2014_aarch64  libneedle.so
#   linux-x86_64-musl  musllinux_1_2_x86_64  libneedle.so
#   linux-arm64-musl   musllinux_1_2_aarch64 libneedle.so
#   macos-arm64    macosx_11_0_arm64      libneedle.dylib
#   macos-x86_64   macosx_11_0_x86_64     libneedle.dylib
#   windows-x86_64 win_amd64              libneedle.dll
#   windows-arm64  win_arm64              libneedle.dll
# ============================================================================
set -euo pipefail

VERSION="${NEEDLE_VERSION:-2.0.3}"
REPO="Cactus-Compute/needle2"
HF_BASE="https://huggingface.co/${REPO}/resolve/main/python"
OUT_ROOT=""          # 空 = 默认缓存 ~/.cache/cactus-needle
WHEEL=""             # 离线模式：本地 wheel 路径
FORCE=0
PLATFORM=""

usage() { sed -n '2,30p' "$0"; exit "${1:-0}"; }

# ---------------------------------------------------------------- 参数解析 ---
while [ $# -gt 0 ]; do
  case "$1" in
    --wheel)   WHEEL="$2"; shift 2 ;;
    --out)     OUT_ROOT="$2"; shift 2 ;;
    --platform) PLATFORM="$2"; shift 2 ;;
    --force)   FORCE=1; shift ;;
    --list)    LIST=1; shift ;;
    -h|--help) usage 0 ;;
    *) echo "未知参数: $1" >&2; usage 1 ;;
  esac
  unset LIST_DONE
done

# ------------------------------------------------------------ 平台矩阵表 ---
# name|wheel-tag|lib-name|说明
PLATFORMS="
linux-x86_64|manylinux2014_x86_64|libneedle.so|Linux x86_64 (glibc)
linux-arm64|manylinux2014_aarch64|libneedle.so|Linux ARM64 (glibc)
linux-x86_64-musl|musllinux_1_2_x86_64|libneedle.so|Linux x86_64 (musl)
linux-arm64-musl|musllinux_1_2_aarch64|libneedle.so|Linux ARM64 (musl)
macos-arm64|macosx_11_0_arm64|libneedle.dylib|macOS Apple Silicon
macos-x86_64|macosx_11_0_x86_64|libneedle.dylib|macOS Intel
windows-x86_64|win_amd64|libneedle.dll|Windows x86_64
windows-arm64|win_arm64|libneedle.dll|Windows ARM64
"

list_platforms() {
  echo "可请求的平台（--platform <name>）与 wheel tag："
  echo "$PLATFORMS" | while IFS='|' read -r name tag lib desc; do
    [ -z "$name" ] && continue
    printf '  %-20s %-24s %-16s %s\n' "$name" "$tag" "$lib" "$desc"
  done
  echo
  echo "说明：上游另有 armv7/riscv64/mipsel/wasm 等 standalone runner 构建"
  echo "（见 needle/agent/fetch.py 的 PLATFORMS），但未发布 wheel；"
  echo "这些平台请用 --features link 构建期链接自有引擎。"
}

if [ "${LIST:-0}" = "1" ]; then list_platforms; exit 0; fi

# --------------------------------------------------------- 平台检测与解析 ---
detect_platform() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os" in
    Linux)
      # musl 检测：上游 _is_musl 的 shell 等价物。
      # 依次尝试 getconf（glibc 返回 "glibc 2.x"）、locale -a 不存在时退回 ldd。
      # 注意本地化后 ldd --version 可能输出 "GNU libc" 而非 "glibc"。
      if getconf GNU_LIBC_VERSION >/dev/null 2>&1 \
         || ldd --version 2>/dev/null | grep -qiE "glibc|gnu libc"; then
        # glibc
        case "$arch" in
          x86_64)  echo "linux-x86_64" ;;
          aarch64|arm64) echo "linux-arm64" ;;
          *)       echo "unknown" ;;
        esac
      else
        # musl
        case "$arch" in
          x86_64)  echo "linux-x86_64-musl" ;;
          aarch64|arm64) echo "linux-arm64-musl" ;;
          *)       echo "unknown" ;;
        esac
      fi
      ;;
    Darwin) case "$arch" in
      arm64) echo "macos-arm64" ;;
      x86_64) echo "macos-x86_64" ;;
      *) echo "unknown" ;;
    esac ;;
    MINGW*|MSYS*|CYGWIN*|Windows*)
      case "$arch" in
        x86_64|AMD64) echo "windows-x86_64" ;;
        aarch64|ARM64) echo "windows-arm64" ;;
        *) echo "unknown" ;;
      esac ;;
    *) echo "unknown" ;;
  esac
}

# 反查：平台名 → tag/lib（支持宽松写法，如 linux-x86_64/musl/x86_64 等）
lookup_platform() {
  local want="$1" row name tag lib desc
  echo "$PLATFORMS" | while IFS='|' read -r name tag lib desc; do
    [ -z "$name" ] && continue
    if [ "$name" = "$want" ] || [ "${name%-musl}" = "$want" ] || [ "$name" = "${want}-musl" ]; then
      echo "$tag $lib"
    fi
  done | head -n1
}

if [ -n "$PLATFORM" ]; then
  RESOLVED="$(lookup_platform "$PLATFORM")"
  if [ -z "$RESOLVED" ]; then
    echo "✗ 未知平台: $PLATFORM（合法值见 --list）" >&2
    exit 1
  fi
  read -r TAG LIB <<< "$RESOLVED"
else
  DETECTED="$(detect_platform)"
  if [ "$DETECTED" = "unknown" ]; then
    echo "✗ 无法识别平台 '$(uname -s)/$(uname -m)'。" >&2
    echo "  用 --platform 显式指定（合法值见 --list）。" >&2
    exit 1
  fi
  read -r TAG LIB <<< "$(lookup_platform "$DETECTED")"
  echo "→ 检测到平台: $DETECTED (wheel tag: $TAG)"
fi

# ---------------------------------------------------------------- 输出目录 ---
if [ -z "$OUT_ROOT" ]; then
  case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*|Windows*) DEST_ROOT="${USERPROFILE:-$HOME}/.cache/cactus-needle" ;;
    *) DEST_ROOT="${HOME}/.cache/cactus-needle" ;;
  esac
  DEST_ROOT="${DEST_ROOT}/${VERSION}"
else
  DEST_ROOT="${OUT_ROOT}/${VERSION}"
fi
DEST="${DEST_ROOT}/${LIB}"

# 幂等：已存在则跳过（--force 覆盖）
if [ -s "$DEST" ] && [ "$FORCE" != "1" ]; then
  echo "✓ 引擎已存在: ${DEST}（--force 可强制重新获取）"
  exit 0
fi

mkdir -p "$DEST_ROOT"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------- 获取 wheel ---
if [ -n "$WHEEL" ]; then
  if [ ! -f "$WHEEL" ]; then
    echo "✗ wheel 不存在: $WHEEL" >&2
    exit 1
  fi
  # 离线模式：校验 wheel 名与目标平台匹配
  WHEEL_NAME="$(basename "$WHEEL")"
  case "$WHEEL_NAME" in
    *"${TAG}"*) ;;
    *) echo "✗ wheel '$WHEEL_NAME' 与目标平台 tag '${TAG}' 不匹配。" >&2
       echo "  请提供对应平台的 wheel，或省略 --wheel 在线下载。" >&2
       exit 1 ;;
  esac
  echo "→ 离线模式: 从 $(basename "$WHEEL") 解压"
  cp "$WHEEL" "${TMP}/engine.whl"
else
  URL="${HF_BASE}/cactus_needle-${VERSION}-py3-none-${TAG}.whl"
  echo "→ 下载 ${URL}"
  if command -v curl >/dev/null 2>&1; then
    curl -fL --retry 3 --progress-bar "$URL" -o "${TMP}/engine.whl"
  elif command -v wget >/dev/null 2>&1; then
    wget -q --show-progress -O "${TMP}/engine.whl" "$URL"
  else
    echo "✗ 需要 curl 或 wget" >&2
    exit 1
  fi
fi

# ---------------------------------------------------------------- 解压校验 ---
command -v unzip >/dev/null 2>&1 || { echo "✗ 需要 unzip" >&2; exit 1; }
unzip -oq "${TMP}/engine.whl" "needle/${LIB}" -d "$TMP"

if [ ! -s "${TMP}/needle/${LIB}" ]; then
  echo "✗ wheel 中未找到 needle/${LIB}（wheel 与平台不匹配？）" >&2
  echo "  wheel 内容：" >&2
  unzip -l "${TMP}/engine.whl" >&2
  exit 1
fi

mv "${TMP}/needle/${LIB}" "${DEST}"

SIZE="$(du -h "$DEST" | cut -f1)"
echo "✓ 引擎已就绪: ${DEST} (${SIZE})"
echo
echo "  bevy_needle 会自动发现该路径（~/.cache/cactus-needle/${VERSION}/）。"
echo "  也可显式指定："
echo "    export NEEDLE_LIB_PATH=\"${DEST}\""
echo "    或 EngineConfig::with_library(\"${DEST}\")"
