# pnos-runtime AGENTS.md

> 本文件是 AI 代理进入 pnos-runtime 仓库时的首读指南。
> 生态级全局约束请参考 [根目录 AGENTS.md](../AGENTS.md)。

## 仓库定位

pnos-runtime 是 PandaNetOS 生态的**系统级运行时**，所有 Agent 注册到此处。负责组件注册中心、服务发现、事件总线、反向代理、应用管理、系统监控。

## 架构概览

```
pnos-runtime/
├── src/           # 运行时核心
│   ├── main.rs    # 入口，serve 命令
│   └── ...
├── docs/          # 文档
├── examples/      # 示例
├── logs/          # 日志
└── Cargo.toml
```

## 目录结构

```
pnos-runtime/
├── src/           # Rust 源代码
├── docs/          # 文档
├── examples/      # 示例代码
├── logs/          # 运行日志
└── Cargo.toml
```

## 构建与测试

| 命令 | 说明 |
|---|---|
| `cargo build --release` | Release 构建 |
| `cargo test --all` | 运行所有测试 |
| `./target/release/pnos-runtime serve` | 启动运行时 |

## 关键配置

| 配置项 | 默认值 | 说明 |
|---|---|---|
| HTTP 监听 | 8080 | 运行时 API 端口（`PNOS_PORT` 可覆盖；pnos-spec 默认 80） |
| `PNOS_DATA_DIR` | `/pnos/data` | 数据根目录（应用装到 `<data_dir>/apps`） |
| `PNOS_CONFIG` | `/etc/pnos/config.yml` | 配置文件路径，不存在时用 pnos-spec 默认值 |
| `PNOS_STORE_URL` | ghfast.top 镜像 | 商店源地址；优先级 `PNOS_STORE_URL` > 配置文件 `default_store_url` > 内置镜像 |
| `PNOS_WEB_DIR` | `<data_dir>/web` | pnos-web 静态文件根目录；容器部署须指向镜像内的静态目录（`ENV PNOS_WEB_DIR`），见 PNOS-docker |
| 环境变量 | `PNOS_RUNTIME_URL` | Agent 连接运行时的地址 |

### 运行时可调参数（均有代码内默认值，环境变量覆盖）

| 环境变量 | 默认值 | 说明 |
|---|---|---|
| `--config <path>` / `--work-dir <dir>` | — | 启动参数；优先级 `--config` > `--work-dir` > 环境变量 > 默认 |
| `PNOS_HTTP_TIMEOUT_SECS` | 30 | 对外 HTTP 客户端超时 |
| `PNOS_API_TIMEOUT_SECS` / `PNOS_PROXY_TIMEOUT_SECS` | 30 / 120 | 管理面 / 反代中间件超时 |
| `PNOS_AGENT_HEALTH_TIMEOUT_SECS` | 3 | Agent 健康检查超时 |
| `PNOS_INSTALL_CONNECT_TIMEOUT_SECS` / `PNOS_INSTALL_TIMEOUT_SECS` | 30 / 600 | 安装连接 / 整体超时 |
| `PNOS_INSTALL_SETTLE_INTERVAL_SECS` | 2 | 启动后就绪轮询间隔 |
| `PNOS_METRICS_CACHE_TTL_SECS` | 1 | 进程指标快照缓存 |
| `PNOS_HEARTBEAT_CHECK_INTERVAL_SECS` | 10 | 心跳超时巡检周期 |
| `PNOS_AGENT_MONITOR_INTERVAL_SECS` | 5 | Agent 巡检周期 |

> 目录骨架（`config`/`data`/`logs`）在启动时由 `config::ensure_dirs()` 创建；
> 后台循环统一订阅 `shutdown` 广播，收到 Ctrl+C 后在循环边界退出（优雅关闭）。
>
> **WorkDir**：规范要求用 `pnos::workdir::WorkDir`，但 pnos-spec 尚未提供该模块；
> `src/workdir.rs` 先落地同语义实现（`WorkDir::new/auto_detect/ensure_dirs` 及标准路径），
> 待 pnos-spec 补齐后切换为直接依赖并删除该模块。

## 对外接口（Web UI / Agent 依赖）

| 接口 | 说明 |
|---|---|
| `GET /api/v1/system/info` | 主机静态信息 |
| `GET /api/v1/system/stats` | CPU/内存/磁盘/网络实时指标 |
| `GET /api/v1/system/config` | **只读**运行时生效配置：`port` / `proxy_prefix` / `cors_origins`，供 Web UI 展示真实值，禁止前端写死 |
| `GET /api/v1/components` | 组件注册表（Agent 与应用） |
| `GET /api/v1/agents` | Agent 列表（含 start/stop/restart） |
| `GET/POST /api/v1/store/*`、`/api/v1/installed/*` | 商店浏览、安装、启停、卸载、安装进度 |
| `/api/v1/ws` | 事件推送（长连接，位于超时中间件之外） |
| `/app/:id`、`/app/:id/*path` | 应用反向代理，前缀取值 `pnos::protocol::APP_PROXY_PREFIX` |

> **商店刷新是"合并"而非"整体替换"**：单个 app.yml 拉取失败（代理站限流/抖动很常见）时保留上一次的清单，
> 只清理已从 index 移除的应用；清单同时落盘 `<data_dir>/store-cache.json`，重启后先加载缓存再刷新，
> 避免"越刷新应用越少"或"一重启商店就缺应用"。
>
> 商店源当前仍是**单个源**（`GET /store/sources` 由配置拼出，`refresh_source` 忽略 id），增删/启停需先补接口与持久化。

## 依赖关系

- **依赖**：`pnos`（git，pnos-spec）、`pnos-comm`（git，pnos-sdk）
- **被依赖**：所有 Agent（pdc/pk/spde）通过 pnos-sdk 注册到运行时

## 注意事项

1. pnos-runtime 必须先于所有 Agent 启动
2. 所有 Agent 通过 pnos-sdk 自动注册到 pnos-runtime，无需手动指定 master 地址
3. 事件总线是跨 Agent 通信的核心通道

## 变更历史

| 日期 | 版本 | 变更内容 |
|---|---|---|
| 2026-09-16 | v1.0 | 初始版本 |
| 2026-09-18 | v1.1 | 新增只读 `GET /api/v1/system/config`（端口/反代前缀/CORS 生效值）；反代路由前缀改取 `pnos::protocol::APP_PROXY_PREFIX`，消除与 Web UI 展示的漂移 |
| 2026-09-18 | v1.2 | 商店刷新改为合并语义 + `<data_dir>/store-cache.json` 磁盘缓存（修"越刷越少/重启缺应用"）；商店源支持 `PNOS_STORE_URL` 与配置文件覆盖，不再无条件硬编码 ghfast.top |
| 2026-09-18 | v1.3 | 静态托管补齐上线必需能力：SPA 深链回落（`not_found_service` + 404→200 归一化，仅对无扩展名路由生效）、`CompressionLayer` gzip（大 chunk 1.57MB→441KB）、`/assets/*` 强缓存与入口 `no-cache`、`/api/*` 不参与回落（写错路径保持 404）、静态根目录支持 `PNOS_WEB_DIR` |
| 2026-09-18 | v1.4 | 合规 26/26 全绿：clippy 21→0（死代码标注理由、`map`-返回-`()` 改 `if let`）；9 处硬编码超时/周期改为 `Settings`（环境变量可覆盖，见上表）；`AgentConfig` 全字段带 `serde(default)`；启动参数 `--config`/`--work-dir`；新增 `workdir.rs` 目录规范与 `ensure_dirs`；新增 `shutdown.rs` 优雅关闭；静态根目录默认改为 `<data_dir>/web`（不再硬编码绝对路径） |
