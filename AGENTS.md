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
| HTTP 监听 | 8080 | 运行时 API 端口 |
| 环境变量 | `PNOS_RUNTIME_URL` | Agent 连接运行时的地址 |

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
