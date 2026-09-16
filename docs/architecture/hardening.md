# pnos-runtime 运行时内核加固设计

> 状态：**待评审**（2026-09）
> 范围：**仅 pnos-runtime 运行时内核**（进程模型 / 状态模型 / 并发与资源 / 隔离与安全 / 契约 / 可观测 / 配置）
> 非目标：多实例 HA 与选主（见 `self-healing.md` L5）、TLS 与全量审计、pnos-web 前端改造、pk/pdc/spde Agent 侧加固
> 关系：本文**继承并落实** `self-healing.md` 的 P0/P1，**收口** `performance-slo.md` 中未闭环的 P9/P8 尾项，并补充该文档未覆盖的安全与契约维度
> 证据基线：commit `2374082`（2026-09-14），`cargo check --all-targets` 通过（11 warning），`cargo clippy --all-targets -- -D warnings` **FAIL（23 error）**，`src` 下单元测试 **0 个**

---

## 1. 现状架构快照

### 1.1 模块职责与状态持有

| 模块 | 职责 | 状态持有方式 | 重启后结果 |
|---|---|---|---|
| `src/registry.rs` | 组件注册/心跳/发现 | `Arc<RwLock<HashMap<String, RegisteredComponent>>>`（纯内存） | **全部丢失** |
| `src/agent/mod.rs` | Agent 子进程生命周期 | `Arc<RwLock<HashMap<String, AgentHandle>>>` + `Child`（纯内存） | **全部丢失**；子进程成孤儿或已被 `kill_on_drop` 杀死 |
| `src/install/mod.rs` | 包安装/蓝绿升级/卸载 | `installed: RwLock<HashMap>` + `progress: Mutex<HashMap>`（纯内存） | **全部丢失**（已安装列表为空） |
| `src/service/store.rs` | 商店清单缓存 | `RwLock<HashMap<String, AppManifest>>`（纯内存） | **全部丢失**，必须重连外部 CDN |
| `src/app_manager.rs` | 应用下载/启停（遗留路径） | `RwLock<HashMap<String, RunningApp>>`（纯内存） | **全部丢失** |
| `src/metrics.rs` | 端点/业务/任务指标 | `Mutex<HashMap<String, EndpointStat>>`（纯内存） | 计数归零（可接受） |
| `src/rate_limit.rs` | 限流/并发/超时 | `OnceLock` 全局单例 | 正常 |
| `src/ws.rs` | 事件总线 + WS 端点 | `OnceLock<broadcast::Sender>` | 正常 |

**结论**：运行时是**纯内存、单进程、单点**结构，四类关键状态（注册表、应用与 PID、安装记录、商店缓存）无一落盘。这与 `self-healing.md` §1 的诊断一致，且该文档发布后**未有任何一项 P0 被实施**。

### 1.2 请求链路

```
Client → CORS(permissive) → TraceLayer
       → DefaultBodyLimit::disable()        ← 对代理路径生效，无上限
       → proxy_timeout(120s)
       → [ /api/v1/* ] DefaultBodyLimit::max(16MB)
                       → concurrency_middleware(信号量 1024, 超 429)
                       → timeout_middleware(30s, 超 504)
                       → rate_limit_middleware(令牌桶 20000/s, 超 429)
                       → metrics_middleware
                       → handler
       → [ /app/:id/* ] proxy_handler（全量 buffer）
       → [ /api/v1/ws ] ws_handler（在超时层之外）
       → [ /health ] 恒返回 "ok"
       → fallback: ServeDir("/var/www/pnos-web")  ← 硬编码
```

### 1.3 绑定与暴露面

- 绑定 `0.0.0.0:8080`（`src/main.rs:109`），**无鉴权**
- CORS `permissive`（`src/main.rs:105`）
- 控制面同时承载**可导致任意代码执行**的操作：`/installed/:id/install` 会下载并执行远程二进制（`src/install/mod.rs:427`）

---

## 2. 失效模式清单（FMEA）

等级：**S1** 可致安全事件/RCE ｜ **S2** 可致整体不可用 ｜ **S3** 状态丢失/不可观测 ｜ **S4** 规范与契约偏离

### 2.1 安全与隔离（S1）

| ID | 失效场景 | 当前行为 | 代码定位 |
|---|---|---|---|
| F-01 | 任意网络对端可注册/注销组件 | `/components/unregister`、`/apps/unregister` **不校验 token**；`Registry::verify_token` 是死代码（clippy 报 never used） | `src/api/apps.rs:77-90`、`src/registry.rs:167-174` |
| F-02 | 任意网页可跨站调用本机控制面 | CORS permissive + 绑定 0.0.0.0 + 无鉴权 → 用户浏览任意站点即可触发注册/注销/**安装并执行二进制** | `src/main.rs:105,109`、`src/api/store.rs:106` |
| F-03 | 供应链：商店索引与包来源可被第三方镜像替换 | 默认商店源硬编码为第三方镜像 `ghfast.top`；`sha256` 校验为可选（None/空串跳过） | `src/main.rs:44-45`、`src/install/mod.rs:471-484` |
| F-04 | 代理转发未清洗跳转头 | 除 `Host` 外**原样转发全部请求头**（含 `Connection`/`Transfer-Encoding` 等 hop-by-hop），未注入 `X-Forwarded-*` | `src/proxy.rs:65-69` |
| F-05 | 代理上行 body 无上限 | 外层 `DefaultBodyLimit::disable()` 对 `/app/:id/*` 生效，且 handler 用 `Bytes` 全量缓冲 → 超大请求体 OOM | `src/main.rs:98`、`src/proxy.rs:21,76` |

### 2.2 进程与状态（S2/S3）

| ID | 失效场景 | 当前行为 | 代码定位 |
|---|---|---|---|
| F-06 | 监听层出错 → 进程直接退出 | 单次 `axum::serve(...).await?`，无重试循环、无看护 | `src/main.rs:113` |
| F-07 | 无优雅关停 → 状态不落盘 | 无信号处理（grep `signal`/`ctrl_c` 零命中）；`SIGTERM` 即终止 | `src/main.rs` 全文 |
| F-08 | 子进程与 runtime 同生共死，或成孤儿 | `kill_on_drop(true)`：正常退出时子进程被杀；硬崩溃（abort/SIGKILL）时 PID 未落盘，**无法接管** | `src/agent/mod.rs:184`、`src/app_manager.rs:28` |
| F-09 | 重启后已安装应用列表为空 | `installed` 纯内存，`list_installed()` 只读内存表 | `src/install/mod.rs:139-146,397-417` |
| F-10 | 重启后注册表为空 | 无快照回放，只能等全部 Agent 心跳重注册 | `src/registry.rs:88-91` |
| F-11 | 断网重启后商店不可用 | 无缓存落盘，启动即拉外部 CDN（失败仅 warn） | `src/service/store.rs:37-91` |
| F-12 | 崩溃自动重启**退避永不升级**，`Error` 状态不可达 | `mark_crash()` 自增 `crash_count` 后，`start()` 又将其**重置为 0** → 计数恒为 1，固定 2s 退避，永远到不了阈值 5 | `src/agent/mod.rs:195` vs `314-323`、`406-426` |
| F-13 | Agent 连续不健康 → 无任何动作 | `update_health()` 仅置 `healthy=false`，无重启/告警/降级 | `src/agent/mod.rs:338-351` |
| F-14 | 并发 `start` 可能启动两个进程 | `start()` 释放写锁后 spawn，再重新加锁写状态，中间有竞态窗口；无 per-agent 启动互斥 | `src/agent/mod.rs:159-201` |

### 2.3 并发与资源（S2/S3）

| ID | 失效场景 | 当前行为 | 代码定位 |
|---|---|---|---|
| F-15 | 指标 key 基数是**无界**的 | 中间件在无 `MatchedPath` 时用**原始 URI path** 作 key → 任意 404 路径都会新增一条记录并永久保留 | `src/metrics.rs:206-209` |
| F-16 | `/metrics` 读取路径 O(n log n) 持锁 | 每次请求对每个端点把 4096 个样本**排序**求分位；F-15 放大后直接拖慢控制面 | `src/metrics.rs:57-66,241-260` |
| F-17 | 进程指标未真正采集 | `collect_process()` 未填充 `thread_count`/`open_files`，恒为 0 / -1 → P8「句柄/FD 可采」未达标 | `src/metrics.rs:166-183` |
| F-18 | 代理大响应全量缓冲 → OOM | `resp.bytes().await` 全量入内存，文档 P9 已标注待修但未修 | `src/proxy.rs:76`、`src/main.rs:88-89` |
| F-19 | 包下载无超时/无上限（遗留路径） | `reqwest::get()` **无超时**，`resp.bytes()` 全量缓冲；与流式安装路径能力不一致 | `src/app_manager.rs:51,64` |
| F-20 | 端口分配 TOCTOU | `is_port_free()` 仅做 `bind` 探测后**立即释放**，随后才由子进程绑定 → 并发安装可撞端口 | `src/install/mod.rs:534-556` |
| F-21 | 持锁跨 await | `list_installed()` 持有 `installed` 读锁期间循环 `await` 查询 Agent 状态，写者（安装/卸载）可能长时间饥饿 | `src/install/mod.rs:398-416` |
| F-22 | 429 未带 `Retry-After` | 限流/过载响应为纯文本，无重试提示，与 `performance-slo.md` 的声明不符 | `src/rate_limit.rs:98,116` |
| F-23 | 解压无资源护栏 | 未限制解压总字节/条目数（zip bomb）、未显式校验路径逃逸与绝对路径条目 | `src/install/mod.rs:490-497`、`src/app_manager.rs:94-139` |

### 2.4 契约与规范（S4）

| ID | 失效场景 | 说明 |
|---|---|---|
| F-24 | 三方契约不一致 | 见 §6 对齐矩阵：`pnos-spec/src/protocol.rs` 规定的规范路径、runtime 实际路由、pnos-web 实际调用互不重合 |
| F-25 | 规范端点缺失 | spec 定义了 `/system/health`、`/component/{id}/*`、`/store/apps/:id/install|start|stop|restart|logs`，runtime **均未实现** |
| F-26 | 分页响应未复用 `PageResult` | runtime 手搓 JSON（缺 `total_pages`），而 spec 已提供 `pnos::response::PageResult` | 
| F-27 | 配置与工作目录规范未落实 | AGENTS.md 要求的 `WorkDir` / `config.yaml` / CLI `--config`+`--work-dir` 全未实现；且 **`pnos-spec` 中根本没有 `workdir` 模块**（依赖缺口，见 §8） |
| F-28 | 硬编码可配置参数 | 见 §4 H7.2 配置化改造清单 |
| F-29 | 后台定时任务自跑 | `registry.start_heartbeat_checker`、`AgentManager::start_monitor`、`MonitorService::start` 均 `tokio::spawn` + `sleep/interval`，未接入统一调度（与 AGENTS.md「禁止自跑定时」冲突） |
| F-30 | 文档与实现脱节 | `README.md` 仍描述 bollard 容器管理（代码已改为二进制应用管理，依赖中无 bollard）；`Cargo.toml` 版本仍 `0.1.0` 而提交称 v0.2/v0.3；`performance-slo.md` 引用的 `enterprise-roadmap.md` **全仓库不存在** |
| F-31 | 门禁形同虚设 | 合规检查第 2 项（clippy `-D warnings`）当前 **23 error FAIL**；第 3 项（`cargo test`）因 **0 个测试**而平凡通过 |

---

## 3. 加固目标与原则

### 3.1 目标（可判定）

1. **进程可自愈**：崩溃后秒级拉起；监听层异常不致命；关停可预期、状态不丢。
2. **状态可重建**：重启后注册表、已安装应用与端口、商店缓存均可恢复；子进程按显式策略接管或回收。
3. **故障有边界**：任一慢调用、大请求、异常对端都不能拖垮控制面；资源占用有上限。
4. **控制面可信**：默认不对外暴露；敏感操作需要凭据；浏览器跨站无法驱动本机执行代码。
5. **契约单一事实源**：路径、响应、分页、错误码以 `pnos-spec` 为准，兼容期显式且有时限。
6. **可观测可告警**：SLI 可持续量化，后台任务静默死亡可被发现。

### 3.2 原则

- **状态：快照兜底 + 心跳为真相**。接受重启后极短窗口内状态滞后，不为强一致引入分布式复杂度。
- **策略与执行分离**：子进程存活策略、落盘周期、退避曲线、限流阈值一律可配，不写死在模块内。
- **默认安全**：默认绑定回环、默认拒绝跨源、默认要求凭据；放宽必须显式配置。
- **失败可解释**：每个降级/拒绝路径返回明确错误码与可重试提示，并计入指标。
- **兼容优先**：已上线的前端调用路径不得破坏，变更走"新增别名 → 标注废弃 → 到期摘除"。
- **一次只加一层**：每项加固独立可回滚，配置开关默认取安全值但行为可退。

---

## 4. 分层加固设计

### H1 进程生命周期加固（对应 F-06 ~ F-08、F-12 ~ F-14）

**H1.1 serve 循环 + 退避重绑 + 崩溃熔断**
- `axum::serve` 包入 `loop`；监听层返回 `Err` 时记录、退避（1s → 2s → … 上限 30s）后重绑，不再直接 `return`。
- 连续失败计数超阈值（默认 1 分钟 5 次）→ 发 `runtime.degraded` 事件 + 指标置位；仍不退化为无限空转。
- 绑定前探测端口占用并给出明确错误（而非 panic 式退出）。

**H1.2 panic 隔离与任务存活感知**
- 所有后台任务统一用包装函数 spawn：捕获 panic → 记录 → 发事件 → 由 supervisor 层决定是否重建。
- 每个后台任务在循环末尾 `mark_task(name)`（已有机制），新增**任务静默判死**：超 3 个周期未 tick → 指标置 1 + 事件告警。

**H1.3 优雅关停（有序、可配、可观测）**
- 信号：Unix `SIGTERM`/`SIGINT`；Windows 控制台 `CTRL_CLOSE`/`CTRL_SHUTDOWN` 与 `ctrl_c`。
- 关停序列：①停止接收新连接（axum graceful shutdown，超时可配）→ ②落盘三类状态 → ③按 `on_runtime_exit` 策略处理子进程 → ④刷出指标、退出码 0。
- 新增配置 `shutdown_grace_secs`（默认 10）、`child_policy_on_exit`（`stop_all` | `detach`，默认 `stop_all`）。

**H1.4 子进程与重启的确定语义**
- 显式化 `kill_on_drop` 语义，改为由 `child_policy_on_exit` 决定，避免"正常退出杀子进程 / 硬崩溃留孤儿"的双重不确定。
- 每个子进程记录 `{app_id, pid, port, active_color, started_at}` 并落盘（见 H2.2）。
- 启动接管流程：读盘 → 逐条 `pid` 存活探测 → 存活且 `detach` 策略则**纳管**（重建句柄，不重启）；不存活则按策略重启或标记 `stopped`。
- 修复 **F-12**：`crash_count` 只由 `mark_crash`/`reset_crash_count`（显式人工干预）修改，`start()` **不得清零**；退避曲线改为可配（`base=1s, factor=2, max=16s, error_threshold=5`）。
- 修复 **F-14**：为每个 agent 增加启动互斥（`Mutex`/状态机 `Starting` 占位 + 超时回收），消除双启动窗口。
- 修复 **F-13**：连续不健康达阈值（默认 3 次）→ 按策略重启 + 计数 + 事件；策略可配（`restart` | `notify_only`）。

### H2 状态加固（对应 F-09 ~ F-11、F-23 部分）

**H2.1 三类状态与落盘目标**

| 状态 | 内容 | 落盘形态 | 写时机 | 恢复语义 |
|---|---|---|---|---|
| 注册表快照 | `ComponentInfo` + token + 最后心跳时间 | JSON 快照（版本化 schema） | 周期（默认 30s）+ 关停时 | 回放后置为"待确认"，等心跳刷新；超时未确认按离线处理 |
| 应用状态 | 已安装清单、`active_color`、版本、端口、PID、策略 | JSON/SQLite（单文件） | 状态跃迁即写（装/升/启/停/卸） | 启动接管（H1.4） |
| 商店缓存 | 索引 + 各 `app.yml` + `ETag`/时间戳 | 磁盘缓存目录 | 每次刷新成功 | 先读缓存立即可用，后台按条件刷新；失败则降级使用缓存 |

**H2.2 恢复顺序（启动自愈）**

```
加载配置 → 读取应用状态 → 读取商店缓存（服务即可降级可用）
        → 回放注册表快照 → 绑定端口（失败按 H1.1 退避）
        → 子进程接管/回收 → 启动后台任务与监控
        → 异步刷新商店、等待 Agent 心跳收敛
```

**H2.3 一致性与兼容**
- 快照为兜底，心跳为最终正确；冲突时以心跳为准。
- 所有落盘结构带 `schema_version`；新增字段一律 `#[serde(default)]` + 默认值函数。
- 写入采用"临时文件 + 原子替换"；跨平台注意 Windows 覆盖语义与杀软占用重试。
- 快照损坏时不得导致启动失败：解析失败 → 记录 + 忽略该快照 + 以降级模式启动。

### H3 并发与资源加固（对应 F-15 ~ F-23）

**H3.1 指标基数与读取路径**
- **F-15 修复**：无 `MatchedPath` 时统一归并为固定 key（如 `"unmatched"`），并对端点表设上限（默认 ≤ 256）+ 淘汰策略；原始路径只入日志不入指标。
- **F-16 修复**：分位改用**固定桶直方图**（如对数桶）替代"存样本 + 每次排序"：记录 O(1)、读取 O(桶数)，读取时不持长锁（快照拷贝后计算）。
- **F-17 修复**：补采线程数（`sysinfo` 进程线程数）与句柄数（Unix `/proc/<pid>/fd` 计数；Windows 用系统 API 或显式标注"不可用"而非返回假值）。

**H3.2 请求侧资源边界**
- 代理上行 body 设硬上限（默认 16MB，可配）并返回 413；不再对代理路径 `disable` body limit。
- 429/504 响应补 `Retry-After`（**F-22**）；限流阈值、并发上限、超时全部改为配置项（当前 `rate_limit.rs:35-36,105,128,136` 为常量）。
- 预留按来源维度的限流键（当前为全局单桶），本期只留扩展位不实现。

**H3.3 流式化（**F-18**、**F-19**）**
- 代理改为流式：`Body::from_stream(reqwest::Response::bytes_stream())`；请求侧同样流式转发，内存占用与文件大小解耦。
- 剥离 hop-by-hop 头（`Connection`、`Keep-Alive`、`Transfer-Encoding`、`Upgrade`、`TE`、`Trailer`、`Proxy-*`），注入 `X-Forwarded-For/Proto/Host`。
- 目标地址只允许注册表中已注册组件的回环端口（防 SSRF）。
- 遗留 `app_manager` 下载路径复用 install 的流式客户端（连接超时 + 总超时 + 最大包体 + 磁盘余量预检），消除 `reqwest::get()` 无超时问题。

**H3.4 安装原子性**
- **F-20 修复**：端口改为**预占式分配**（分配器持有租约表；探测与占用同一临界区，或 bind 后保持 listener 直到子进程接管）。
- **F-21 修复**：`list_installed()` 先取内存快照再释放锁，然后异步补充运行时状态。
- **F-23 修复**：解压前限制总解压字节与条目数，拒绝绝对路径/`..` 逃逸条目与设备文件，超限即失败并回滚。
- 安装步骤幂等化 + 可中断：失败时清理半成品目录与临时文件，不留"装了一半"的状态。

### H4 隔离与安全加固（对应 F-01 ~ F-05）

**H4.1 控制面凭据**
- 敏感操作（`unregister` / `install` / `upgrade` / `uninstall` / `start` / `stop` / `restart`）要求凭据：
  - 组件自身操作：使用注册时下发的 `token`（`verify_token` 从死代码变为强校验路径）；
  - 管理面操作（前端）：使用独立**管理令牌**（启动时生成并写 `data/admin_token`，或从配置/环境变量读取）。
- 无凭据返回 `401`；凭据不匹配返回 `403`；均计入指标。

**H4.2 默认收敛的网络暴露**
- 默认绑定 `127.0.0.1`（新增 `bind_addr`，可由 `--bind`/环境变量覆盖）；对外暴露必须显式配置。
- CORS 默认关闭跨源（或白名单化，配置 `cors_allow_origins`）；不再 `permissive`。
- 保留 `/health/live` 与 `/api/v1/metrics` 可无鉴权访问（供探针/采集），其余端点默认受保护。

**H4.3 供应链**
- `sha256` 改为**强制**（缺失即拒绝安装，或需显式配置 `allow_unverified=true` 才放行）。
- 商店索引支持签名校验（spec 侧定义签名格式）；默认商店源不硬编码第三方镜像——默认直连官方源，镜像作为可配置项并标注信任影响。
- 记录安装来源（源 URL、hash、时间）以便审计追溯。

### H5 契约对齐（对应 F-24 ~ F-26）

见 §6 矩阵。原则：**`pnos-spec/src/protocol.rs` 是唯一事实源**，runtime 补规范路径别名，前端在用的非规范路径保留为兼容别名并标注废弃窗口。

### H6 可观测与自愈联动（对应 F-17、F-22、F-31）

- 健康检查分级：
  - `/health/live`：进程存活，无依赖，恒 200；
  - `/health/ready`：状态已回放 + 注册表可用 + 依赖就绪（商店缓存缺失时**可降级为 ready**，需显式声明策略）；
  - 保留 `/health` 作为 liveness 别名，保持向后兼容。
- 指标补全：任务心跳年龄、快照落盘耗时/失败计数、代理流式字节与背压、限流丢弃数、鉴权拒绝数、子进程重启/接管数。
- 事件补全：`runtime.started` / `runtime.recovered` / `runtime.degraded` / `runtime.shutting_down` / `component.recovered` / `child.taken_over`（复用现有 WS 总线）。
- SLO 回归：把 `performance-slo.md` 的 P3/P6/P9/P10/P11 纳入可复跑的基准脚本，结果归档并在 CI 中防退化。

### H7 配置与部署加固（对应 F-27 ~ F-29）

**H7.1 配置分层与优先级**

```
内置默认 < 配置文件 < 环境变量 < 命令行
```

- runtime 引入自有 `RuntimeConfig`（承载 bind/port/限流/超时/退避/落盘周期/策略/子进程策略等），
  与 `pnos::config::PnosConfig`（承载 data/media/app 目录与商店源）**分层组合**，避免直接改语义。
- 注意 `PnosConfig::load()` 默认读 `/etc/pnos/config.yml`（POSIX 假设），需支持 `PNOS_CONFIG` 或经 CLI 显式指定。
- CLI：`--config <path>`、`--work-dir <dir>`、`--bind <addr>`、`--port <n>`、`--log-level <lv>`（合规 #20）。

**H7.2 配置化改造清单（F-28）**

| 位置 | 当前硬编码 | 目标 |
|---|---|---|
| `src/rate_limit.rs:35-36` | 20000 / 20000 | 配置项（保留 env 覆盖） |
| `src/rate_limit.rs:105` | 并发 1024 | 配置项 |
| `src/rate_limit.rs:128,136` | 30s / 120s | 配置项 |
| `src/main.rs:86` | body 16MB | 配置项 |
| `src/main.rs:104` | `/var/www/pnos-web` | 配置项（且需处理目录不存在） |
| `src/main.rs:44-47` | 商店源、app_data_dir 拼接 | 移入配置，不写死镜像 |
| `src/agent/mod.rs:356,414,438` | 5s 轮询 / 退避 16 上限 / 5s 健康起点 | 配置项 |
| `src/agent/mod.rs:314` | 崩溃阈值 5 | 配置项 |
| `src/install/mod.rs:143-144` | 30s / 600s | 配置项 |
| `src/install/mod.rs:537-539` | 端口段 9000..10000 | 配置项 |
| `src/install/mod.rs:569` | 健康轮询 2s | 配置项 |
| `src/service/monitor.rs:25-27` | 2s / 30s TTL | 配置项（interval 已有 env，统一之） |
| `src/metrics.rs:20` | 样本 4096 | 由 H3.1 直方图桶替代 |
| `src/ws.rs:23` | 总线容量 1024 | 配置项 |

**H7.3 后台任务统一化（F-29）**
- 现状：三处 `tokio::spawn` + `sleep/interval` 自跑。
- 目标：引入 runtime 内部的 `TaskRegistry`（注册名称、周期、策略：`once`/`interval`、可暂停/可触发执行一次、健康上报）。
- **依赖缺口**：`pnos-spec::task` 只有任务协议类型，**没有 TaskScheduler 实现**（见 §8），故本期先在 runtime 内实现 `TaskRegistry` 并保持接口形态可迁移。

**H7.4 部署交付物**
- 提供 `deploy/` 三种看护方案：`systemd` unit（Restart=always, RestartSec=1）、Windows WinSW/NSSM 配置、容器 `restart: unless-stopped`（已有 `PNOS-docker`）。
- 探针指向 `/health/live`（liveness）与 `/health/ready`（readiness）。
- 关停超时与 `shutdown_grace_secs` 对齐。

---

## 5. 关键接口与模型变更（汇总）

| 类型 | 名称 | 说明 |
|---|---|---|
| 新增配置 | `RuntimeConfig` | bind/port/限流/超时/退避/落盘/策略/子进程策略 |
| 新增配置 | `child_policy_on_exit` | `stop_all` \| `detach` |
| 新增配置 | `state_persist_interval_secs` | 快照周期 |
| 新增配置 | `auth_mode` / `admin_token_file` | 控制面鉴权 |
| 新增配置 | `cors_allow_origins` | 跨源白名单 |
| 新增配置 | `allow_unverified_install` | 供应链逃生阀（默认 false） |
| 新增端点 | `GET /health/live`、`GET /health/ready` | 分级健康检查 |
| 新增端点 | `POST /api/v1/store/apps/:id/restart`、`GET .../logs` | 补齐 spec 定义 |
| 新增端点 | `/component/:id/*` | 统一组件代理前缀（spec 已定义） |
| 新增端点 | `GET /api/v1/system/health` | 补齐 spec 定义 |
| 新增状态文件 | `data/runtime/registry.snapshot.json` | 注册表快照 |
| 新增状态文件 | `data/runtime/apps.json`（或 `apps.db`） | 应用+PID+端口 |
| 新增状态文件 | `data/runtime/store-cache/` | 商店缓存（含 ETag） |
| 新增状态文件 | `data/runtime/admin_token` | 管理令牌（权限收敛） |
| 事件新增 | `runtime.started/recovered/degraded/shutting_down`、`child.taken_over` | 自愈可观测 |
| 契约收敛 | 分页统一 `pnos::response::PageResult` | 补 `total_pages`，不传 `page` 仍返回全量 |
| 契约收敛 | 错误响应统一 `ApiResponse` + `ErrorCode` + `request_id` | 便于链路追踪 |

---

## 6. 三方契约对齐矩阵（F-24/F-25）

| 能力 | pnos-spec 规范路径 | runtime 当前实现 | pnos-web 当前调用 | 处置建议 |
|---|---|---|---|---|
| 系统信息 | `/system/info` | ✅ 一致 | ✅ | 保持 |
| 系统指标 | `/system/stats` | ✅ 一致 | ✅ | 保持 |
| 系统健康 | `/system/health` | ❌ 缺（仅 `/health`） | — | runtime 补齐 + 保留 `/health` 别名 |
| 组件列表 | `/components` | ✅（含分页扩展） | ✅ | 保持；分页改 `PageResult` |
| 组件详情 | `/components/:id` | ✅ | ✅ | 保持 |
| 组件注销 | `/components/unregister` | ✅（但无鉴权） | 用 `/apps/unregister` | 补鉴权；前端迁移后废弃 `/apps/*` |
| 应用安装 | `/store/apps/:id/install` | ❌（实为 `/installed/:id/install`） | `/installed/:id/install` | runtime 增规范别名，`/installed/*` 标废弃 |
| 应用启停 | `/store/apps/:id/start|stop|restart` | ❌（`/installed/*` 有 start/stop，无 restart） | `/installed/:id/start|stop` | 同上 + 补 restart |
| 应用日志 | `/store/apps/:id/logs` | ❌ | — | 补齐（子进程 stdout/stderr 已 pipe，未消费） |
| 商店源 | `/store/sources`、`/store/sources/:id/refresh` | ✅ | ✅ | 保持 |
| 商店应用 | `/store/apps`、`/store/apps/:id` | ✅ | ✅ | 保持 |
| 安装进度 | （spec 无） | ✅ `/installed/:id/progress` | ✅ | 建议 spec 收录为规范端点 |
| 代理前缀 | `/app/{id}` + `/component/{id}` | 仅 `/app/:id/*` | iframe 直接用 `serve_url` | 补 `/component/{id}/*` |
| WebSocket | `/ws` | `/api/v1/ws` | — | 明确规范前缀，或 spec 修订为 `/api/v1/ws` |
| Agent 管理 | （spec 无） | ✅ `/agents/*` | — | 建议 spec 收录 |

> 说明：`/installed/*` 与 `/apps/unregister` 属**事实标准**（前端已上线），加固期间必须保留可用；规范化的方式是**新增别名 + 标注废弃窗口**，不得直接切换。

---

## 7. 验收标准

### 7.1 进程与状态（H1/H2）

1. `kill -9` runtime → supervisor ≤ 2s 拉起；重启后 ≤ 5s 内 `/health/ready` 返回 200。
2. 重启后已安装应用列表、版本、`active_color`、端口与崩溃前一致；子进程按 `child_policy_on_exit` 被**接管或回收**，无孤儿、无双实例。
3. `SIGTERM` → ≤ `shutdown_grace_secs + 5s` 内退出，退出码 0，且三类状态已落盘（校验文件存在与内容完整）。
4. 快照文件被人为损坏 → 进程仍能启动（降级模式），且发出 `runtime.degraded` 事件。
5. 连续崩溃 5 次 → 进入 `Error` 且**退避确实递增**（该行为当前不可达，须有测试覆盖）。

### 7.2 安全（H4）

6. 无凭据调用 `POST /components/unregister`、`POST /installed/:id/install` → `401`；错误凭据 → `403`。
7. 默认配置下监听 `127.0.0.1`；`curl` 从非回环地址不可达。
8. 跨源请求默认被拒（`Access-Control-Allow-Origin` 不返回 `*`）。
9. `sha256` 缺失的包在默认配置下**拒绝安装**。

### 7.3 资源与稳定（H3）

10. 代理 1GB 文件：runtime RSS 增长 < 32MB，且不随文件大小线性增长。
11. 随机 404 路径压测 10 万次后，指标端点 key 数有界（≤ 路由数 + 固定 key）。
12. `/api/v1/metrics` 在 10 万样本下 p99 < 5ms（不再排序持锁）。
13. 并发安装同一应用不产生端口冲突、不产生半成品目录。
14. 超限请求返回 413/429 且带 `Retry-After`。

### 7.4 门禁与规范（H5/H7）

15. `cargo clippy --all-targets -- -D warnings` **0 error**（当前 23 error）。
16. `cargo test --all` 有实质用例：注册/心跳/分页/限流/鉴权/快照回放/接管/崩溃退避/蓝绿回滚，覆盖率不低于约定门槛。
17. `check-compliance.ps1` 26 项全 PASS。
18. 配置化清单（§4 H7.2 表）逐项落实，`--config`/`--work-dir` 可用且优先级正确。
19. `/health/live` 在依赖未就绪时仍 200；`/health/ready` 在状态未回放时非 200。

---

## 8. 依赖与阻塞项（跨仓库）

| 阻塞 | 说明 | 影响 | 建议 |
|---|---|---|---|
| **B-1** | `pnos-spec` **没有 `workdir` 模块**（grep 零命中），但 AGENTS.md 强制要求 `pnos::workdir::WorkDir` | runtime 无法合规落地工作目录规范（F-27） | 二选一：①在 `pnos-spec` 实现 `WorkDir`；②修订 AGENTS.md 撤回该要求。**需先决策** |
| **B-2** | `pnos-spec::task` 只有协议类型，**无 TaskScheduler 实现** | 「禁止自跑定时，统一注册到调度器」对 runtime 不可执行（F-29） | 本期先在 runtime 内实现 `TaskRegistry` 并保持可迁移形态 |
| **B-3** | 契约三方不一致（§6），spec 未收录 `/installed/*`、`/agents/*`、`/api/v1/metrics` | 规范与事实标准分裂 | spec 修订收录事实标准，或 runtime 补规范别名（建议后者优先，避免破坏前端） |
| **B-4** | 合规门禁当前 FAIL（clippy 23 error）+ 0 测试 | 无法通过 `check-compliance.ps1` 提交 | 加固 P0 首项即清零 clippy error，并同步补测试 |
| **B-5** | `performance-slo.md` 引用 `enterprise-roadmap.md`，该文件全仓库不存在 | 文档断链，无法作为验收依据 | 本文 §4/§7 已覆盖其安全/韧性条目；建议删除断链引用并指向本文 |
| **B-6** | `README.md`（bollard 容器管理）与 `Cargo.toml`（版本 0.1.0）与实际实现不一致 | 新成员误导 | 随 P0 一并校正 |

---

## 9. 落地顺序与分期

| 期 | 项 | 内容 | 价值 | 风险 |
|---|---|---|---|---|
| **P0-1** | H4.1/H4.2 | 敏感操作鉴权 + 默认回环绑定 + CORS 收敛 | 消除 S1 级风险（跨站驱动本机执行代码） | 前端需带令牌 → 需同步改 `pnos-web` 请求拦截器（已预留注入点） |
| **P0-2** | H1.1/H1.3 | serve 循环 + 优雅关停 + 子进程策略显式化 | 进程不再"一错即死"，关停可预期 | 低 |
| **P0-3** | H2.1/H2.2 | 三类状态落盘 + 启动恢复 + 孤儿接管 | 重启后状态可重建 | 中（落盘结构与原子写） |
| **P0-4** | 缺陷修复 + 门禁 | F-12 崩溃退避失效、F-14 双启动、F-22 Retry-After；清零 clippy error；补核心测试 | 让 `check-compliance.ps1` 可过 | 低 |
| **P1-1** | H3.1 | 指标基数收敛 + 直方图化 + 进程指标补全 | 防 `/metrics` 自身成为故障源 | 低 |
| **P1-2** | H3.3 | 代理/下载流式化 + 头部清洗 + SSRF 白名单 + body 上限 | 解锁 P9，消除 OOM | 中（流式与超时交互） |
| **P1-3** | H3.4 | 端口预占 + 解压护栏 + 安装幂等 | 消除竞态与资源耗尽 | 中 |
| **P1-4** | H6 | `/health/live`+`/health/ready`、指标/事件补全、任务静默判死 | 可观测闭环 | 低 |
| **P1-5** | H7.1/H7.2/H7.3 | 配置分层 + CLI + 全量去硬编码 + TaskRegistry | 满足合规 #9/#15/#20 | 中（配置面较大，需逐项回归） |
| **P2-1** | H5 | 契约对齐（别名 + 废弃窗口 + `PageResult` + `request_id`） | 单一事实源 | 中（需与 pnos-web 联调） |
| **P2-2** | H4.3 | 供应链签名与来源审计 | 安装链路可信 | 高（依赖 spec 定义签名格式） |
| **P2-3** | H7.4 | systemd/WinSW/docker 交付物 + 探针 | 崩溃自动拉起 | 低 |

**建议**：P0 四项合并为一个加固批次（可独立评审、可灰度、可回滚），完成后运行 `check-compliance.ps1` 与 §7 验收清单；P1、P2 各自成批。

**文档同步**：本批次落地后需同步更新 `README.md`（能力现状）、`Cargo.toml`（版本）、`self-healing.md`（标记 P0 完成情况）、`performance-slo.md`（解除断链引用），并按 AGENTS.md 第 24 项评估是否需要更新生态级 AGENTS.md。

---

## 附：本次核查的命令与结论

| 检查 | 命令 | 结果 |
|---|---|---|
| 编译 | `cargo check --all-targets` | ✅ 通过，11 warning |
| 静态分析 | `cargo clippy --all-targets -- -D warnings` | ❌ **23 error，exit 101** |
| 单元测试 | `src` 内 `#[test]`/`#[tokio::test]` 检索 | **0 个** |
| 信号处理 | grep `signal`/`ctrl_c`/`SIGTERM` | 仅注释，无实现 |
| WorkDir 依赖 | `pnos-spec/src` grep `workdir` | **零命中（标准库无此模块）** |
| 定时任务 | grep `tokio::spawn` + `interval`/`sleep` | 3 处模块内自跑 |
