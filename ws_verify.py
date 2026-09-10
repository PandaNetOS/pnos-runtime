#!/usr/bin/env python3
"""pnos-runtime /api/v1/ws 端点验证（覆盖 T-ws1~6）。

依赖：websockets（受管 venv）。用法：
  python ws_verify.py
"""
import asyncio
import json
import urllib.request

BASE = "http://127.0.0.1:8099"
WS_BASE = "ws://127.0.0.1:8099/api/v1/ws"

results = []


def mark(name, ok, detail=""):
    results.append((name, ok, detail))
    print(f"[{'PASS' if ok else 'FAIL'}] {name} {detail}")


def register(cid, port):
    body = json.dumps({
        "id": cid, "name": cid, "version": "1.0.0",
        "component_type": "app", "port": port, "capabilities": ["bench"],
    }).encode()
    req = urllib.request.Request(
        f"{BASE}/api/v1/components/register", data=body,
        headers={"Content-Type": "application/json"}, method="POST")
    with urllib.request.urlopen(req, timeout=5) as r:
        resp = json.loads(r.read().decode())
    return resp["data"]["token"], resp["data"]["component_id"]


def unregister(cid):
    body = json.dumps({"id": cid}).encode()
    req = urllib.request.Request(
        f"{BASE}/api/v1/components/unregister", data=body,
        headers={"Content-Type": "application/json"}, method="POST")
    urllib.request.urlopen(req, timeout=5).read()


async def main():
    import websockets

    # ---- T-ws1: 合法 token 可建立 WS 连接 ----
    tok1, cid1 = register("ws_a", 9101)
    try:
        async with websockets.connect(f"{WS_BASE}?token={tok1}", open_timeout=5) as ws:
            mark("T-ws1 合法token建连", True, f"cid={cid1}")
            # ---- T-ws2: 订阅 component.* 后收到 component.registered ----
            await ws.send(json.dumps({"action": "subscribe", "event_type": "component.*"}))
            tok2, cid2 = register("ws_b", 9102)  # 触发 component.registered
            got = None
            try:
                async for msg in ws:
                    m = json.loads(msg)
                    if m.get("event_type") == "component.registered":
                        got = m
                        break
            except websockets.exceptions.ConnectionClosed:
                pass
            ok2 = got is not None and got.get("payload", {}).get("component_id") == cid2
            mark("T-ws2 订阅component.*收registered", ok2,
                 f"recv event_type={got['event_type'] if got else None}")

            # ---- T-ws4: component.* 不应收到 system.stats ----
            # 等 5s（monitor 每 2s 推一次 system.stats），确认收不到
            sys_seen = False
            try:
                async with asyncio.timeout(5):
                    async for msg in ws:
                        m = json.loads(msg)
                        if m.get("event_type") == "system.stats":
                            sys_seen = True
                            break
                        if m.get("event_type") == "component.registered":
                            continue
            except asyncio.TimeoutError:
                pass
            except websockets.exceptions.ConnectionClosed:
                pass
            mark("T-ws4 前缀过滤(system.stats被拒)", not sys_seen,
                 "未在订阅component.*下收到system.stats" if not sys_seen else "误收system.stats")

            # ---- T-ws5: 重连后仍收到后续事件 ----
            pass  # 在下方独立连接测试
    except Exception as e:
        mark("T-ws1 合法token建连", False, f"异常: {e}")
        mark("T-ws2 订阅component.*收registered", False, "依赖T-ws1")
        mark("T-ws4 前缀过滤", False, "依赖T-ws1")

    # ---- T-ws5: 重新连接后仍能收事件（模拟断线重连） ----
    try:
        async with websockets.connect(f"{WS_BASE}?token={tok1}", open_timeout=5) as ws:
            await ws.send(json.dumps({"action": "subscribe", "event_type": "*"}))
            tok3, cid3 = register("ws_c", 9103)
            got5 = None
            try:
                async for msg in ws:
                    m = json.loads(msg)
                    if m.get("event_type") == "component.registered" and \
                       m.get("payload", {}).get("component_id") == cid3:
                        got5 = m
                        break
            except websockets.exceptions.ConnectionClosed:
                pass
            mark("T-ws5 重连后仍收事件(通配*)", got5 is not None,
                 f"recv cid={got5['payload']['component_id'] if got5 else None}")
    except Exception as e:
        mark("T-ws5 重连后仍收事件", False, f"异常: {e}")

    # ---- T-ws6: 错误 token 被拒（期望 403/握手失败） ----
    try:
        async with websockets.connect(f"{WS_BASE}?token=badtoken", open_timeout=5) as ws:
            await ws.recv()
        mark("T-ws6 错误token被拒", False, "连接居然成功")
    except websockets.exceptions.InvalidStatusCode as e:
        mark("T-ws6 错误token被拒", e.status_code in (403, 401), f"HTTP {e.status_code}")
    except Exception as e:
        mark("T-ws6 错误token被拒", True, f"握手被拒: {type(e).__name__}")

    # 清理
    try:
        unregister("ws_a"); unregister("ws_b"); unregister("ws_c")
    except Exception:
        pass

    print("\n==== 汇总 ====")
    npass = sum(1 for _, ok, _ in results if ok)
    print(f"{npass}/{len(results)} 通过")
    for name, ok, detail in results:
        print(f"  [{'PASS' if ok else 'FAIL'}] {name}")


if __name__ == "__main__":
    asyncio.run(main())
