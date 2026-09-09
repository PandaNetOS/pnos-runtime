# pnos-runtime 自愈与高可用设计

> 状态：草案（2026-09）
> 范围：pnos-runtime 作为系统级运行时/内核核心的「永远在线」与「自我修复」能力设计
> 关联：`pnos-spec`（协议/类型）、`pnos-sdk`（Agent 自动注册与心跳）

## 1. 背景与现状诊断

pnos-runtime 是 PandaNetOS 生态的系统级运行时，所有 Agent（pk/pdc/spde）通过 pnos-sdk 自动注册到此。它承担组件注册中心、服务发现、反向代理、应用管理、系统监控等职责，属于**内核级核心组件**，其可用性直接决定整个生态是否可用。

当前实现**尚未具备自愈能力**，是一个纯内存、单进程、单点故障的设计：

| 模块 | 当前实现 | 风险 |
|---|---|---|
| `src/registry.rs` | 组件注册表为 `Arc<RwLock<HashMap>>`，纯内存 | 进程崩溃后所有注册信息丢失 |
| `src/app_manager.rs` | 运行进程表 `processes: RwLock<HashMap<String, RunningApp>>` 纯内存 | 运行时崩溃后，已启动的应用子进程成为孤儿，无法被回收/接管 |
| `src/service/store.rs` | 商店清单缓存 `apps` 纯内存 | 重启后必须重新拉取外部 CDN，断网时无缓存可用 |
| `src/main.rs` | `axum::serve(...).await?` 只执行一次，无重启循环/panic 兜底/信号处理 | 监听层出错即退出；无优雅退出，退出前不落盘 |
| 部署 | 仓库内无 supervisor 配置（无 systemd unit / NSSM / docker-compose） | 进程死后无人拉起 |

**已有的自愈雏形**（可在此基础上扩展）：

- `registry.rs::start_heartbeat_checker`：后台每 10s 检查心跳超时，自动把组件标记为离线；
- Agent 侧 pnos-sdk 自动注册 + 心跳：运行时重启后 Agent 会自动重注册；
- axum/tower 默认用 `catch_unwind` 隔离单个 handler 的 panic，不会拖垮整个进程。

**关键认知**：因为 Agent 会通过 pnos-sdk 自动重注册，自愈的核心命题不是「让状态永远不丢」，而是「**让状态能秒级重建 + 让进程被秒级拉起**」。

## 2. 设计目标

「永远在线」可拆解为三个由低到高的层次：

1. **进程存活**：进程崩溃后能自动拉起，不长期停机。
2. **状态恢复**：重启后以最短时间恢复对外服务能力，注册表/应用进程/商店缓存可重建。
3. **无单点（远期）**：多实例故障转移，实现真正 0 停机。

非功能性要求：

- **可观测**：崩溃/重启/离线等事件必须可见、可告警；
- **渐进式**：先以最小成本消除「裸奔单点」，再逐步演进到 HA；
- **简单优先**：在满足目标的前提下，不引入超出当前规模所需的复杂度（如分布式一致性）。

## 3. 分层方案

自愈能力分五层叠加，每层解决一类问题：

### L1 进程守护 + 秒级拉起（外部 supervisor）

由系统级组件保证「进程死了立即重启」，而非依赖进程自身（进程崩溃时自己无能为力）。

- **Linux**：systemd unit，`Restart=always` + `RestartSec=1`；
- **Windows**（本项目实际运行环境）：NSSM / WinSW，或注册为 Windows 服务；
- **容器**：`docker run --restart=unless-stopped`，复用现有 `Dockerfile`。

分工：supervisor 负责「拉起」，进程内负责「优雅退出 + 留痕」。

### L2 进程内 panic 隔离 + 重启循环（兜底）

即便有 supervisor，也应把故障影响面在进程内缩到最小：

1. **serve 循环**：把 `axum::serve` 包进 `loop`，监听层返回 `Err` 时不直接 return，而是记录日志、退避 1s 后重试重绑。避免监听层偶发错误导致整个进程退出。
2. **panic 隔离**：确保所有 `tokio::spawn` 的后台任务（心跳检查、商店刷新等）各自包裹 `catch_unwind`，单个任务 panic 不拖垮整个 runtime。
3. **优雅退出**：监听 SIGTERM/SIGINT，收到信号后先落盘状态、回收子进程，再退出，保证状态可恢复。
4. **崩溃循环检测**：记录连续崩溃次数，超过阈值（如 1 分钟 5 次）则退避重试 + 告警，避免「崩溃-重启-再崩溃」空转。

### L3 状态持久化 + 启动自愈（最关键）

这是「自愈」与「只是重启」的分水岭。重启后需快速恢复三类状态：

1. **注册表快照**：`Registry` 增加持久化——定期把注册信息落盘（JSON/SQLite 快照或 WAL），启动时先回放快照，再靠 Agent 心跳重注册把状态刷新到最新。重启窗口从「全部 Agent 重连」缩短为「快照秒回 + 增量心跳」。
2. **孤儿进程接管**：`AppManager` 把每个应用的 `app_id` 与真实 PID 落盘。重启后扫描磁盘记录的 PID：
   - 存活 → **接管**（重新纳入 `processes` 管理，而非杀掉重开）；
   - 已死 → 按需重启。
3. **商店缓存落盘**：`StoreService` 的 `apps` 缓存落盘，重启后优先读缓存，避免依赖外部 CDN（断网时仍可用缓存目录）。

### L4 可观测性（没有观测就没有「修复」）

- 把现有 `GET /health` 升级为分级健康检查：`/health/live`（进程存活）+ `/health/ready`（注册表/依赖就绪，可对外服务）；
- 增加 `/metrics`（Prometheus 格式），暴露崩溃次数、重启次数、组件离线数、心跳超时数等指标；
- panic hook 打印 backtrace + 写 crash 日志，便于事后定位根因。

### L5 高可用 / 消除单点（真正的「永远在线」）

L1–L4 解决「崩溃后快速恢复」，但仍存在秒级到分钟级的停机窗口。要 0 停机必须多实例：

- **主备模式**：主实例 + 备实例，用租约/心跳做**选主**（轻量方案），备实例只读同步状态；主挂后备接管，通过虚拟 IP（keepalived）或前置反代切换；
- **SDK failover**：pnos-sdk 支持配置多个 runtime 地址，主地址不可达时自动切备，Agent 无感知；
- 代价：需要引入共享状态（或复制日志），复杂度明显上升，建议最后实施。

## 4. 与现有代码的对应改动

| 文件 | 改造点 |
|---|---|
| `src/main.rs` | serve 循环 + 优雅退出（信号处理）+ 启动时状态回放 + 崩溃循环检测 |
| `src/registry.rs` | 注册表快照持久化（落盘/回放）+ 心跳检查任务 panic 隔离 |
| `src/app_manager.rs` | PID 落盘 + 孤儿进程接管 + 子进程回收 |
| `src/service/store.rs` | 商店清单缓存落盘，启动时优先读缓存 |
| `src/config.rs` | 新增持久化路径、恢复开关等配置项 |
| 新增（部署） | systemd unit / NSSM 配置 / docker-compose（supervisor） |

关键接口示意（serve 循环 + 优雅退出）：

```rust
// 概念示意，非最终实现
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 启动时回放注册表快照、接管孤儿进程、读商店缓存
    state.recover().await?;

    // 优雅退出：收到信号后先落盘再退出
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        if let Ok(()) = shutdown_signal().await {
            let _ = tx.send(());
        }
    });

    // serve 循环：监听层出错不退出，退避后重试
    let mut attempt = 0;
    loop {
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => { attempt += 1; backoff(attempt).await; continue; }
        };
        tokio::select! {
            _ = rx => { state.persist().await?; break; }
            r = axum::serve(listener, app.clone()) => {
                if let Err(e) = r { tracing::error!("serve 错误: {e}，1s 后重试"); }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
    Ok(())
}
```

## 5. 落地路线图

| 级别 | 内容 | 收益 | 复杂度 |
|---|---|---|---|
| **P0** | supervisor 重启 + serve 循环 + 优雅退出（先落盘再退出）+ 注册表快照 + 孤儿进程接管 | 崩溃后**秒级恢复**，状态不丢 | 低～中 |
| **P1** | 崩溃循环检测 + `/health/ready` 分级 + `/metrics` + panic hook | 可观测、可告警、防空转 | 低 |
| **P2** | 多实例 + 选主 + VIP + pnos-sdk 多地址 failover | 真正 0 停机 | 高 |

**建议顺序**：P0 先行——用最小成本把「会丢状态的裸奔单进程」变成「可自愈的运行时」；P1 紧随其后补齐观测；P2 在明确需要 0 停机、且生态规模达到多机部署时再启动。

## 6. 验收标准

- **P0**：kill 掉 pnos-runtime 进程后，supervisor 在 ≤2s 内拉起；重启后注册表在 ≤5s 内恢复（快照回放 + Agent 心跳）；崩溃前已启动的应用子进程重启后仍被正确管理（接管或重启）。
- **P1**：`/health/ready` 能在依赖未就绪时返回非 200；`/metrics` 能反映崩溃次数与离线组件数；连续崩溃时能触发退避与告警。
- **P2**：主实例宕机后，备实例在 ≤1s 内接管，Agent 无感知（SDK 自动 failover）。

## 7. 风险与取舍

- **持久化引入一致性问题**：快照可能与实时状态短暂不一致。取舍：以快照为「兜底」，以 Agent 心跳重注册为「最终正确」，接受重启后极短窗口内状态略有滞后。
- **HA 的复杂度**：选主/复制日志会显著增加实现与运维成本。取舍：在单机部署阶段，L1–L4 已能满足「崩溃自愈」，HA 仅作为远期演进，不提前引入。
- **跨平台**：本项目需兼顾 Windows 与 Linux，supervisor 选型（NSSM vs systemd）需分别提供方案，但进程内逻辑（serve 循环、快照、PID 接管）应保持平台无关。
