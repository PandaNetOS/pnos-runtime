# pnos-runtime

pnos 系统运行时。提供容器管理、应用商店引擎、系统监控、反向代理等系统级能力。

## 功能

- **容器管理**：列表、详情、启动、停止、重启、删除、日志（基于 bollard）
- **应用商店**：多源拉取、应用目录、安装/卸载（自动拉镜像、创建容器、健康检查）
- **系统监控**：CPU、内存、磁盘、负载实时采集（基于 sysinfo）
- **反向代理**：`/app/{id}/*` 自动代理到对应容器
- **静态托管**：托管 pnos-web 前端文件

## 技术栈

- Rust + Axum + Tokio
- bollard（Docker API）
- sysinfo（系统监控）
- reqwest（商店源拉取）

## 构建

```bash
cargo build --release
```

## 运行

```bash
# 需要挂载 Docker socket
docker run -d \
  --name pnos \
  -p 80:80 \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v /volume1/pnos:/data \
  -v /volume1/media:/media \
  pandanetos/pnos-runtime:latest
```

## 环境变量

| 变量 | 默认值 | 说明 |
|------|--------|------|
| PNOS_PORT | 80 | 监听端口 |
| PNOS_DATA_DIR | /data | 数据目录 |
| PNOS_MEDIA_DIR | /media | 媒体目录 |
| PNOS_LOG_LEVEL | info | 日志级别 |

## API

详见 `pnos-spec` 仓库的协议定义。

## 许可证

MIT
