#!/usr/bin/env bash
# 构建 kiro-rs 镜像（在源码目录 /opt/kiro-rs-src-liangcaijv 下执行）
#
# 用法：
#   ./build-image.sh          # 拉取并构建 dev 分支最新代码
#   ./build-image.sh <分支>   # 构建指定分支
#
# 镜像 tag 规范：kiro-rs:liangcaijv-<分支>-<短commit>（便于回滚/追溯）
# 构建成功后会打印后续部署命令（改 /opt/kiro-rs/docker-compose.yml + restart）。

set -euo pipefail

# 始终在脚本所在目录（源码目录）执行
cd "$(cd "$(dirname "$0")" && pwd)"

if [[ ! -f Dockerfile ]]; then
    echo "错误: 当前目录没有 Dockerfile，请把本脚本放在源码目录（如 /opt/kiro-rs-src-liangcaijv）" >&2
    exit 1
fi

BRANCH="${1:-dev}"

echo "==> 更新源码: $BRANCH"
git fetch origin
git checkout "$BRANCH"
# --ff-only: 本地有意外提交时报错退出，避免悄悄产生合并提交
git pull --ff-only origin "$BRANCH"

SHA="$(git rev-parse --short HEAD)"
BRANCH_SLUG="${BRANCH//\//-}"
TAG="kiro-rs:liangcaijv-${BRANCH_SLUG}-${SHA}"

echo "==> 当前提交:"
git log --oneline -1

echo "==> 构建镜像: $TAG"
docker build -t "$TAG" .

echo ""
echo "==> 构建成功。把下面这行镜像名称填到 /opt/kiro-rs/docker-compose.yml 的 image: 字段:"
echo ""
echo "$TAG"
echo ""
echo "改完后执行 ./restart.sh（主实例）或 ./restart-fcc.sh（副实例），"
echo "再用 docker inspect kiro-rs --format '{{.Config.Image}}' 确认已是新 tag。"
