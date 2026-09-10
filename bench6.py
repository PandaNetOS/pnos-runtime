#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
bench6.py —— pnos-runtime 企业级性能验证（B/C/D/E 落地验收）

自动拉起一个「低阈值」独立测试实例（不影响 8080 生产实例），
逐项验证：
  T1  性能埋点可见性（/api/v1/metrics 返回端点直方图）
  T2  列表分页正确性（注册 7 个组件，翻页切片无误 + 无 page 时兼容数组）
  T3  p99.9 延迟（本地实测 vs 指标端点上报交叉校验）
  T4  过载 - body 限制（>16MB 返回 413）
  T5  过载 - 限流（突发超 burst 返回 429）
  T6  过载 - 并发上限（并发超信号量返回 429）
  T7  /health 在过载下存活（liveness 不受 rate/concurrency/timeout 层影响）

退出码 0 = 全部通过；非 0 = 存在失败项。
"""

import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor

BASE = os.environ.get("BENCH_BASE", "http://127.0.0.1:8099")
_HERE = os.path.dirname(os.path.abspath(__file__))
_DEFAULT_BIN = os.path.join(_HERE, "target", "release", "pnos-runtime.exe")
if not os.path.exists(_DEFAULT_BIN):
    _DEFAULT_BIN = r"D:/PNOS/pnos-runtime/target/release/pnos-runtime.exe"
BIN = os.environ.get("PNOS_BIN", _DEFAULT_BIN)
TEST_PORT = 8099
LOW_BURST = 200     # 高于正常流量(T1~T3 约 113)但 T5 洪水(800)远超以触发 429
LOW_RATE = 150      # 每秒补充 150 个令牌
LOW_CONC = 8        # 并发信号量上限

results = []  # (name, passed:bool, detail:str)


def mark(name, passed, detail=""):
    results.append((name, passed, detail))
    status = "PASS" if passed else "FAIL"
    print(f"[{status}] {name}" + (f"  -- {detail}" if detail else ""))


def do_req(method, path, data=None, timeout=10, ctype="application/json"):
    url = BASE + path
    body = None
    headers = {}
    if data is not None:
        if isinstance(data, (bytes, bytearray)):
            body = data
        else:
            body = data.encode("utf-8")
        headers["Content-Type"] = ctype
    req = urllib.request.Request(url, data=body, method=method, headers=headers)
    t0 = time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            raw = resp.read().decode("utf-8", "replace")
            ms = (time.perf_counter() - t0) * 1000.0
            return resp.status, raw, ms
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", "replace")
        ms = (time.perf_counter() - t0) * 1000.0
        return e.code, raw, ms
    except Exception as e:  # noqa: BLE001
        ms = (time.perf_counter() - t0) * 1000.0
        return -1, str(e), ms


def percentile(values, q):
    if not values:
        return 0.0
    s = sorted(values)
    idx = min(len(s) - 1, int((len(s) * q)))
    return s[idx]


def wait_health(timeout=30):
    deadline = time.time() + timeout
    while time.time() < deadline:
        st, _, _ = do_req("GET", "/health", timeout=2)
        if st == 200:
            return True
        time.sleep(0.3)
    return False


def register_component(idx):
    payload = json.dumps({
        "id": f"bench-comp-{idx}",
        "name": f"BenchComp{idx}",
        "version": "1.0.0",
        "component_type": "app",
        "port": 9100 + idx,
        "capabilities": ["bench"],
    })
    return do_req("POST", "/api/v1/components/register", payload)


def main():
    if not os.path.exists(BIN):
        print(f"未找到二进制: {BIN}")
        sys.exit(2)

    tmp = tempfile.mkdtemp(prefix="bench6_")
    env = dict(os.environ)
    env["PNOS_PORT"] = str(TEST_PORT)
    env["PNOS_DATA_DIR"] = tmp
    env["PNOS_RATE_LIMIT_BURST"] = str(LOW_BURST)
    env["PNOS_RATE_LIMIT_RATE"] = str(LOW_RATE)
    env["PNOS_CONCURRENCY_LIMIT"] = str(LOW_CONC)
    env["PNOS_LOG_LEVEL"] = "warn"

    print(f"== 拉起测试实例 port={TEST_PORT} burst={LOW_BURST} rate={LOW_RATE} conc={LOW_CONC} ==")
    proc = subprocess.Popen([BIN], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        if not wait_health(30):
            mark("实例启动", False, "30s 内 /health 未就绪")
            sys.exit(1)
        mark("实例启动", True, f"pid={proc.pid} 监听 {TEST_PORT}")

        # ---------- T1 埋点可见性 ----------
        for _ in range(3):
            do_req("GET", "/api/v1/metrics")
        st, body, _ = do_req("GET", "/api/v1/metrics")
        try:
            snap = json.loads(body)
            eps = snap.get("endpoints", {})
            biz = snap.get("business", {})
            ok = st == 200 and len(eps) > 0 and "components" in biz
            mark("T1 埋点可见性", ok,
                 f"status={st} endpoints={len(eps)} keys={list(eps.keys())[:3]}")
        except Exception as e:  # noqa: BLE001
            mark("T1 埋点可见性", False, f"解析失败: {e}")

        # ---------- T2 分页正确性 ----------
        reg_ok = 0
        for i in range(1, 8):
            st, _, _ = register_component(i)
            if st == 200:
                reg_ok += 1
        st, body, _ = do_req("GET", "/api/v1/components?page=1&page_size=3")
        page1 = json.loads(body) if st == 200 else {}
        d1 = page1.get("data", {})
        ok_p1 = (d1.get("total") == 7 and d1.get("page") == 1
                 and d1.get("page_size") == 3 and len(d1.get("items", [])) == 3)
        mark("T2.1 首页切片", ok_p1,
             f"total={d1.get('total')} page={d1.get('page')} size={d1.get('page_size')} items={len(d1.get('items', []))}")

        st, body, _ = do_req("GET", "/api/v1/components?page=3&page_size=3")
        page3 = json.loads(body) if st == 200 else {}
        d3 = page3.get("data", {})
        ok_p3 = (d3.get("total") == 7 and d3.get("page") == 3
                 and len(d3.get("items", [])) == 1)
        mark("T2.2 末页切片", ok_p3,
             f"page={d3.get('page')} items={len(d3.get('items', []))} (期望 1)")

        st, body, _ = do_req("GET", "/api/v1/components")
        arr = json.loads(body).get("data", None) if st == 200 else None
        ok_arr = isinstance(arr, list) and len(arr) == 7
        mark("T2.3 无 page 兼容数组", ok_arr,
             f"data 类型={'array' if isinstance(arr, list) else type(arr).__name__} len={len(arr) if isinstance(arr, list) else 'NA'}")

        # ---------- T3 p99.9 延迟 ----------
        n = 100
        lat = []
        for _ in range(n):
            st, _, ms = do_req("GET", "/api/v1/metrics", timeout=15)
            if st == 200:
                lat.append(ms)
        p50, p95, p99, p999 = (percentile(lat, 0.5), percentile(lat, 0.95),
                               percentile(lat, 0.99), percentile(lat, 0.999))
        # 取指标快照（若恰逢 429 则重试，确保拿到 JSON）
        snap = None
        for _ in range(15):
            st, body, _ = do_req("GET", "/api/v1/metrics")
            if st == 200:
                try:
                    snap = json.loads(body)
                    break
                except Exception:  # noqa: BLE001
                    pass
            time.sleep(0.1)
        rep = (snap or {}).get("endpoints", {}).get("GET /api/v1/metrics", {})
        rep_p999 = rep.get("p999_ms", 0.0)
        ok_lat = len(lat) > 0 and p999 < 2000.0 and rep_p999 > 0 and abs(p999 - rep_p999) < 100.0
        mark("T3 p99.9 延迟", ok_lat,
             f"本地 n={len(lat)} p50={p50:.1f} p95={p95:.1f} p99={p99:.1f} p999={p999:.1f}ms | "
             f"端点上报 p999={rep_p999:.1f}ms")

        # ---------- T4 body 限制 413 ----------
        big = b'{"id":"x","name":"x","version":"1.0.0","component_type":"App","port":9999}' + b' ' * (20 * 1024 * 1024)
        st, _, _ = do_req("POST", "/api/v1/components/register", big, timeout=30)
        mark("T4 body 限制(413)", st == 413, f"大请求 status={st} (期望 413)")

        # ---------- T5 限流 429（低并发，避免触发并发上限） ----------
        def hit():
            s, _, _ = do_req("GET", "/api/v1/metrics", timeout=15)
            return s
        with ThreadPoolExecutor(max_workers=4) as ex:
            codes = list(ex.map(lambda _: hit(), range(800)))
        c429 = sum(1 for c in codes if c == 429)
        ok_rate = c429 > 0
        mark("T5 限流(429)", ok_rate, f"800 请求中 {c429} 个 429（burst={LOW_BURST}）")

        # ---------- T6 并发上限 429 ----------
        with ThreadPoolExecutor(max_workers=40) as ex:
            codes2 = list(ex.map(lambda _: hit(), range(40)))
        c429b = sum(1 for c in codes2 if c == 429)
        ok_conc = c429b > 0
        mark("T6 并发上限(429)", ok_conc, f"40 并发中 {c429b} 个 429（conc={LOW_CONC}）")

        # ---------- T7 /health 在过载下存活 ----------
        stop = threading.Event()
        health_codes = []

        def flood():
            while not stop.is_set():
                do_req("GET", "/api/v1/metrics", timeout=5)

        def probe_health():
            while not stop.is_set():
                s, _, _ = do_req("GET", "/health", timeout=2)
                health_codes.append(s)
                time.sleep(0.15)

        tf = threading.Thread(target=flood, daemon=True)
        th = threading.Thread(target=probe_health, daemon=True)
        tf.start(); th.start()
        time.sleep(3.0)
        stop.set()
        tf.join(); th.join()
        ok_health = len(health_codes) > 0 and all(c == 200 for c in health_codes)
        mark("T7 /health 过载存活", ok_health,
             f"探测 {len(health_codes)} 次，全部 200 = {all(c == 200 for c in health_codes)}")

    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except Exception:  # noqa: BLE001
            proc.kill()
        import shutil
        try:
            shutil.rmtree(tmp, ignore_errors=True)
        except Exception:  # noqa: BLE001
            pass

    passed = sum(1 for _, p, _ in results if p)
    total = len(results)
    print("\n================ 验收汇总 ================")
    for name, p, detail in results:
        print(f"  [{'PASS' if p else 'FAIL'}] {name}")
    print(f"\n结果: {passed}/{total} 通过")
    sys.exit(0 if passed == total else 1)


if __name__ == "__main__":
    main()
