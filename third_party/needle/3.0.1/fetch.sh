#!/usr/bin/env bash
# 获取 needle3 引擎产物（libneedle3.so + needle3.cact）。
# 来源：HuggingFace Cactus-Compute/needle3（上游 fetch.py 同源）。
# 注意：HTTP/2 流会被中途掐断——必须 --http1.1 + 断点续传。
set -euo pipefail
cd "$(dirname "$0")"

BASE="https://huggingface.co/Cactus-Compute/needle3/resolve/main"
PLATFORM="linux-x86_64"   # macos-arm64 / windows-x86_64 / linux-arm64 / …见仓库 tree

fetch() { # url out expected_size
    local url="$1" out="$2" expected="$3" size=0
    for _ in 1 2 3 4 5; do
        curl --http1.1 -L -m 600 -C - "$url" -o "$out" 2>/dev/null || true
        size=$(stat -c%s "$out" 2>/dev/null || echo 0)
        [ "$size" = "$expected" ] && { echo "OK  $out ($size bytes)"; return 0; }
        echo "retry $out ($size / $expected)"
    done
    echo "FAILED $out" >&2; return 1
}

# 库（1.2MB；wheel 内 needle/libneedle3.so——GitHub 上的 gen3 wheel 与 HF 同源，
# 直接用 HF 的 python/ wheel 解包，避免依赖 unzip）
fetch "$BASE/python/cactus_needle-3.0.1-py3-none-manylinux2014_x86_64.whl" cactus3.whl 536871
python3 -c "
import zipfile
z = zipfile.ZipFile('cactus3.whl')
open('libneedle3.so','wb').write(z.read('needle/libneedle3.so'))
"
rm cactus3.whl

# 头文件（1.2KB）
fetch "$BASE/linux-x86_64/needle.h" needle.h 1187

# 基础权重（35MB；不进 git）
fetch "$BASE/needle3.cact" needle3.cact 35335380

echo "needle3 产物就绪：$(pwd)/{libneedle3.so, needle.h, needle3.cact}"
