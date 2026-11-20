#!/usr/bin/env python3
"""Targeted cost check of the read-only inspection routes.

Protocol and pass criteria: docs/console-inspection-costs.md (fixed before
measuring). Standard library only. Starts and stops its own knotrel-server
processes on free loopback ports; never touches an existing server.

Usage: python3 scripts/measure-inspection-costs.py target/release/knotrel-server [--quick]
"""
import http.client
import json
import os
import platform
import random
import re
import socket
import statistics
import subprocess
import sys
import threading
import time

SEED = 20261120
QUICK = "--quick" in sys.argv
NODES = 50_000 if QUICK else 500_000
EDGES = 150_000 if QUICK else 1_500_000
HUB_DEGREE = 2_000 if QUICK else 20_000
PAGES = 60 if QUICK else 300
OPS = 600 if QUICK else 3_000
M3_OPS = 20_000 if QUICK else 100_000
BATCH = 1024


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Server:
    def __init__(self, binary, inspection):
        self.port = free_port()
        env = dict(os.environ)
        env.update({
            "KNOTREL_ADDR": f"127.0.0.1:{self.port}",
            "KNOTREL_ENGINE": "compact-bfs",
            "KNOTREL_TOPOLOGY_LIFECYCLE": "true",
            "KNOTREL_MAX_NODES": "unlimited",
            "KNOTREL_MAX_EDGES": "unlimited",
            "KNOTREL_METRICS_ENABLED": "true",
            "KNOTREL_INSPECTION": "true" if inspection else "false",
            "KNOTREL_REQUEST_TIMEOUT_MS": "120000",
        })
        self.proc = subprocess.Popen([binary], env=env, stdout=subprocess.DEVNULL,
                                     stderr=subprocess.DEVNULL)
        deadline = time.time() + 20
        while time.time() < deadline:
            try:
                if Client(self.port).get("/health")[0] == 200:
                    return
            except OSError:
                time.sleep(0.05)
        raise RuntimeError("server did not start")

    def rss_bytes(self):
        out = subprocess.check_output(["ps", "-o", "rss=", "-p", str(self.proc.pid)])
        return int(out.strip()) * 1024

    def stop(self):
        self.proc.terminate()
        try:
            self.proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            self.proc.kill()


class Client:
    def __init__(self, port):
        self.conn = http.client.HTTPConnection("127.0.0.1", port, timeout=120)

    def get(self, path):
        self.conn.request("GET", path)
        r = self.conn.getresponse()
        return r.status, r.read()

    def post(self, path, body):
        self.conn.request("POST", path, body=json.dumps(body),
                          headers={"content-type": "application/json"})
        r = self.conn.getresponse()
        return r.status, r.read()

    def timed_get(self, path):
        t = time.perf_counter()
        status, body = self.get(path)
        return (time.perf_counter() - t) * 1000, status, body

    def timed_post(self, path, body):
        t = time.perf_counter()
        status, data = self.post(path, body)
        return (time.perf_counter() - t) * 1000, status, data


def batches(ops):
    for i in range(0, len(ops), BATCH):
        yield {"operations": ops[i:i + BATCH]}


def load_graph(client, rng):
    ids = rng.sample(range(1, 2**63), NODES)
    ids[0] = 2**64 - 1  # include the u64 maximum
    for batch in batches([{"op": "add_node", "node": str(n)} for n in ids]):
        assert client.post("/v1/batch", batch)[0] == 200
    edges = []
    seen = set()
    while len(edges) < EDGES:
        a, b = rng.randrange(NODES), rng.randrange(NODES)
        if a != b and (a, b) not in seen and (b, a) not in seen:
            seen.add((a, b))
            edges.append({"op": "link", "source": str(ids[a]), "target": str(ids[b])})
    hub = ids[1]
    for j in range(2, 2 + HUB_DEGREE):
        edges.append({"op": "link", "source": str(hub), "target": str(ids[j])})
    for batch in batches(edges):
        assert client.post("/v1/batch", batch)[0] == 200
    inactive = rng.sample(ids, NODES // 100)
    for batch in batches([{"op": "set_node_active", "node": str(n), "active": False}
                          for n in inactive]):
        assert client.post("/v1/batch", batch)[0] == 200
    disabled = [e for e in rng.sample(edges, 50)]
    for batch in batches([{"op": "set_edge_enabled", "source": e["source"], "target": e["target"],
                           "enabled": False} for e in disabled]):
        client.post("/v1/batch", batch)
    return ids, hub


HIST = re.compile(r'^knotrel_graph_job_execution_seconds_bucket\{kind="inspect",le="([^"]+)"\} (\d+)')


def inspect_buckets(client):
    _, body = client.get("/metrics")
    buckets = {}
    for line in body.decode().splitlines():
        m = HIST.match(line)
        if m:
            buckets[m.group(1)] = int(m.group(2))
    return buckets


def bucket_quantile(before, after, q):
    """Upper bound of the bucket holding quantile q of the observations
    between two scrapes (cumulative buckets). Returns seconds or None."""
    items = sorted(((float("inf") if k == "+Inf" else float(k)), after[k] - before.get(k, 0))
                   for k in after)
    total = items[-1][1] if items else 0
    if total == 0:
        return None
    for bound, count in items:
        if count >= q * total:
            return bound
    return None


def pct(values, q):
    values = sorted(values)
    return values[min(len(values) - 1, int(q * len(values)))]


def summary(values):
    return {"p50": round(pct(values, 0.5), 3), "p95": round(pct(values, 0.95), 3),
            "max": round(max(values), 3), "n": len(values)}


def m1(client, ids, hub, rng):
    results = {}
    sorted_ids = sorted(ids)
    hub_neighbors = []
    after = None
    while True:
        path = f"/v1/nodes/{hub}/edges?limit=1000" + (f"&after={after}" if after else "")
        _, body = client.get(path)
        page = json.loads(body)
        hub_neighbors += [e["neighbor"] for e in page["edges"]]
        after = page["next_after"]
        if after is None:
            break
    kinds = {
        "nodes_limit_1000": lambda: f"/v1/nodes?limit=1000&after={rng.choice(sorted_ids)}",
        "edges_limit_1000": lambda: f"/v1/edges?limit=1000&after_source={rng.choice(sorted_ids)}",
        "hub_incident_limit_1000":
            lambda: f"/v1/nodes/{hub}/edges?limit=1000&after={rng.choice(hub_neighbors)}",
        "nodes_inactive_budget_bound":
            lambda: f"/v1/nodes?state=inactive&limit=1000&after={rng.choice(sorted_ids)}",
    }
    for name, make in kinds.items():
        before = inspect_buckets(client)
        lat = []
        for _ in range(PAGES):
            ms, status, _ = client.timed_get(make())
            assert status == 200, status
            lat.append(ms)
        after_b = inspect_buckets(client)
        results[name] = {"client_ms": summary(lat),
                         "server_exec_p95_bucket_s": bucket_quantile(before, after_b, 0.95),
                         "server_exec_max_bucket_s": bucket_quantile(before, after_b, 1.0)}
    return results


def m2(port, ids, rng):
    def mutation_load(client, local_rng):
        lat = []
        for i in range(OPS):
            n = str(local_rng.choice(ids))
            if i % 2:
                body = {"op": "set_node_active", "node": n, "active": bool(i % 4 == 1)}
            else:
                body = {"op": "link", "source": n, "target": str(local_rng.choice(ids))}
            ms, status, _ = client.timed_post("/v1/operations", body)
            lat.append(ms)
        return lat

    def query_load(client, local_rng):
        lat = []
        for _ in range(OPS):
            body = {"op": "node_state", "node": str(local_rng.choice(ids))}
            ms, status, _ = client.timed_post("/v1/operations", body)
            assert status == 200
            lat.append(ms)
        return lat

    out = {}
    for name, load in (("mutations", mutation_load), ("queries", query_load)):
        alone = load(Client(port), random.Random(SEED + 1))
        stop = threading.Event()
        pages = [0]

        def browse():
            c = Client(port)
            src, tgt = None, None
            while not stop.is_set():
                path = "/v1/edges?state=disabled&limit=1000"
                if src:
                    path += f"&after_source={src}"
                if tgt:
                    path += f"&after_target={tgt}"
                _, body = c.get(path)
                page = json.loads(body)
                pages[0] += 1
                src, tgt = page["next_after_source"], page["next_after_target"]
        browsers = [threading.Thread(target=browse) for _ in range(2)]
        for t in browsers:
            t.start()
        time.sleep(0.2)
        together = load(Client(port), random.Random(SEED + 1))
        stop.set()
        for t in browsers:
            t.join()
        out[name] = {"alone_ms": summary(alone), "with_browsing_ms": summary(together),
                     "browse_pages": pages[0]}
    return out


def m3(binary):
    runs = {"on": {"add_node": [], "link_new_endpoint": []},
            "off": {"add_node": [], "link_new_endpoint": []}}
    for _ in range(3):
        for mode in ("on", "off"):
            server = Server(binary, inspection=(mode == "on"))
            try:
                c = Client(server.port)
                ops = [{"op": "add_node", "node": str(10**12 + i)} for i in range(M3_OPS)]
                t = time.perf_counter()
                for b in batches(ops):
                    c.post("/v1/batch", b)
                runs[mode]["add_node"].append(M3_OPS / (time.perf_counter() - t))
                ops = [{"op": "link", "source": str(10**12 + (i % M3_OPS)),
                        "target": str(2 * 10**12 + i)} for i in range(M3_OPS)]
                t = time.perf_counter()
                for b in batches(ops):
                    c.post("/v1/batch", b)
                runs[mode]["link_new_endpoint"].append(M3_OPS / (time.perf_counter() - t))
            finally:
                server.stop()
    medians = {mode: {k: round(statistics.median(v)) for k, v in d.items()}
               for mode, d in runs.items()}
    ratio = {k: round(medians["on"][k] / medians["off"][k], 3) for k in medians["on"]}
    return {"ops_per_s_median": medians, "on_over_off": ratio, "runs": runs}


def main():
    binary = sys.argv[1]
    rng = random.Random(SEED)
    report = {"machine": f"{platform.platform()} {platform.machine()} cpus={os.cpu_count()}",
              "nodes": NODES, "edges": EDGES, "hub_degree": HUB_DEGREE, "quick": QUICK}
    rss = {}
    for mode in ("off", "on"):
        server = Server(binary, inspection=(mode == "on"))
        try:
            client = Client(server.port)
            t = time.perf_counter()
            ids, hub = load_graph(client, random.Random(SEED))
            report[f"load_seconds_{mode}"] = round(time.perf_counter() - t, 1)
            time.sleep(1)
            rss[mode] = server.rss_bytes()
            if mode == "on":
                report["M1"] = m1(client, ids, hub, rng)
                report["M2"] = m2(server.port, ids, rng)
        finally:
            server.stop()
    report["M4"] = {"rss_off": rss["off"], "rss_on": rss["on"],
                    "bytes_per_node": round((rss["on"] - rss["off"]) / NODES, 1)}
    report["M3"] = m3(binary)
    c1 = all((r["server_exec_p95_bucket_s"] or 0) <= 0.010 and r["client_ms"]["p95"] <= 25
             for r in report["M1"].values())
    c2 = all(v["with_browsing_ms"]["p95"] <= v["alone_ms"]["p95"] + 5
             for v in report["M2"].values())
    c3 = all(v >= 0.85 for v in report["M3"]["on_over_off"].values())
    c4 = report["M4"]["bytes_per_node"] <= 40
    report["criteria"] = {"C1": c1, "C2": c2, "C3": c3, "C4": c4}
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
