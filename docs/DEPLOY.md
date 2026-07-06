# kiro-rs 服务器部署手册

本文档记录在服务器上从 fork 仓库拉取代码、构建 Docker 镜像并重启服务的标准流程。

> 约定：镜像 tag 用 `kiro-rs:liangcaijv-<分支>-<短commit>` 命名（如 `kiro-rs:liangcaijv-dev-9ee7eef`），
> 这样 `docker-compose.yml` 里能一眼看出当前跑的是哪个提交。

## 目录约定

| 路径 | 用途 |
|---|---|
| `/opt/kiro-rs-src-liangcaijv` | 源码目录（git 仓库，用于构建镜像） |
| `/opt/kiro-rs` | 运行目录（docker-compose、配置、启动脚本） |
| `/opt/kiro-rs/config` | 主实例配置（容器 `kiro-rs`，端口 8990） |
| `/opt/kiro-rs/config-fcc` | 副实例配置（容器 `kiro-rs-2`，端口 8991） |

---

## 部署步骤

### 1. 拉取最新代码

```bash
cd /opt/kiro-rs-src-liangcaijv

# 源码目录固定在 dev 分支，直接拉取即可
git pull origin dev

# 确认当前提交，记下短 commit id（下一步构建 tag 要用）
git status
git log --oneline -1
```

假设 `git log --oneline -1` 输出 `9ee7eef ...`，则本次短 commit id 为 `9ee7eef`。

### 2. 构建镜像

用「分支 + 短 commit」命名 tag，便于追溯：

```bash
cd /opt/kiro-rs-src-liangcaijv

docker build -t kiro-rs:liangcaijv-dev-9ee7eef .
```

> 把 `9ee7eef` 替换成上一步实际看到的 commit id。

构建完成后确认镜像存在：

```bash
docker images | grep kiro-rs
```

### 3. 更新 docker-compose 镜像 tag

编辑 `/opt/kiro-rs/docker-compose.yml`，把两个服务的 `image` 都指向新 tag：

```bash
cd /opt/kiro-rs

# 备份当前 compose 文件（带时间戳）
cp docker-compose.yml docker-compose.yml.bak.$(date +%Y-%m-%d-%H%M%S)

# 把旧 tag 批量替换为新 tag（示例：从 e58acdd 升级到 9ee7eef）
sed -i 's/kiro-rs:liangcaijv-dev-e58acdd/kiro-rs:liangcaijv-dev-9ee7eef/g' docker-compose.yml

# 确认替换结果
grep image docker-compose.yml
```

> 也可以直接用编辑器改 `image:` 那两行。两个容器（`kiro-rs` / `kiro-rs-2`）共用同一镜像，记得都改。

参考 `docker-compose.yml` 结构：

```yaml
services:
  kiro-rs:
    image: kiro-rs:liangcaijv-dev-9ee7eef   # ← 改这里
    container_name: kiro-rs
    restart: unless-stopped
    ports:
      - "127.0.0.1:8990:8990"
    volumes:
      - ./config:/app/config
    networks:
      - sub2api_net

  kiro-rs-2:
    image: kiro-rs:liangcaijv-dev-9ee7eef   # ← 改这里
    container_name: kiro-rs-2
    restart: unless-stopped
    ports:
      - "127.0.0.1:8991:8990"
    volumes:
      - ./config-fcc:/app/config
    networks:
      - sub2api_net

networks:
  sub2api_net:
    external: true
    name: sub2api_sub2api-network
```

### 4. 执行启动脚本

```bash
cd /opt/kiro-rs

./restart.sh        # 重启主实例
./restart-fcc.sh    # 重启副实例（如需要）
```

> 若启动脚本本质是 `docker compose up -d`，也可直接运行：
> ```bash
> docker compose up -d
> ```
> Compose 会检测到 image tag 变化并用新镜像重建容器。

---

## 部署后验证

```bash
# 容器状态应为 Up
docker ps | grep kiro-rs

# 确认容器实际使用的镜像 tag
docker inspect kiro-rs   --format '{{.Config.Image}}'
docker inspect kiro-rs-2 --format '{{.Config.Image}}'

# 查看启动日志有无报错
docker logs --tail 50 kiro-rs

# 本地探活（端口仅绑定 127.0.0.1）
curl -s http://127.0.0.1:8990/v1/models -H "x-api-key: <你的 apiKey>" | head
```

---

## 配置说明（与本次功能相关）

如需开启「模拟 prompt 缓存」，在对应实例的 `config/config.json` 中加入：

```json
{
  "simulateCache": true,
  "simulateCacheReadRatio": 0.8,
  "simulateCacheWriteRatio": 0.1
}
```

- 默认 `simulateCache: false`。开启后**每次请求**把估算总输入 token 按固定比例拆分：
  `cache_read = total × readRatio`（默认 0.8）、`cache_creation = total × writeRatio`
  （默认 0.1）、剩余计入正常 `input_tokens`。不看客户端 `cache_control` 断点、
  无跨请求状态。两比例之和应 ≤ 1（超出时读取优先、写入让位）。
- ⚠️ 该功能只是在响应 usage 中**伪造** `cache_creation_input_tokens` / `cache_read_input_tokens`，
  让 sub2api 等面板的缓存指标非 0、成本曲线接近真实 Anthropic。
  Kiro 后端**不支持**真实缓存，**不会**真的节省 token、额度或耗时。
- 三个字段均可在 Admin 控制台实时调整（`/admin` 顶栏「模拟缓存」按钮），**无需重启**，
  保存后对后续请求立即生效，并写回配置文件（重启后保持）。也可直接调 API：
  `GET/PUT /api/admin/config/simulate-cache`，body 如
  `{"enabled":true,"readRatio":0.8,"writeRatio":0.1}`（省略的字段保持当前值）。

---

## 回滚

镜像按 commit tag 保留，回滚只需把 `docker-compose.yml` 的 `image` 改回上一个 tag 再重启：

```bash
cd /opt/kiro-rs
sed -i 's/kiro-rs:liangcaijv-dev-9ee7eef/kiro-rs:liangcaijv-dev-e58acdd/g' docker-compose.yml
./restart.sh
```

旧镜像可用 `docker images | grep kiro-rs` 查看。确认稳定后再清理过期镜像：

```bash
docker image rm kiro-rs:liangcaijv-dev-<旧tag>
```
