# Soak test

Shows that a long-running native Fennec server doesn't leak. A small HTTP API with `shared` state, SQLite and file writes runs under steady load for a set time while some requests crash and others fail. The test passes when:

- memory stays flat after warm-up;
- latency is steady, minute by minute;
- no file descriptors leak;
- the server never stops, and each crash ends only its own request.

It isn't a required check. Run it by hand, under the watchdog, after changes to the runtime, the scheduler or how native code lets go of what it made:

```bash
cargo build -p fennec -p fennec-runtime
./target/debug/fennec build bench/soak/server.fen -o target/soak-server
perl watch.pl 700 4096 python3 -I bench/soak/soak.py target/soak-server --minutes 10 --out soak.json
```

## What it sends

Eight clients, each on a kept-alive connection, send a weighted mix:

| Request | Share | What it does |
|---|---:|---|
| `GET /count` | 40% | counts the request in a `shared` value |
| `POST /note?n=N` | 22% | a SQLite transaction: insert note N, delete notes 2000 older |
| `POST /note?n=-1` | 3% | the same note again: fails while it's there, answered 500 |
| `POST /file?n=N` | 20% | writes the body to one of 64 files, then reads it back |
| `GET /stats` | 10% | reads the `shared` value under its lock |
| `GET /crash?n=N` | 1% | overflows an `int`: crashes its request, which closes its connection |
| not HTTP | 2% | answered 400 without the handler running |
| half a request | 2% | the client hangs up partway |

`--only count,file` sends only those kinds, to find which one leaks.

## What it measures

Every 5 seconds it takes the server's RSS and its open descriptors (from `lsof`). It also records each request's latency, the throughput, and every outcome. The report is JSON, with every sample.

**Warm-up:** each worker's caches fill up to a bound as it serves: the runtime keeps up to 8 MB of freed blocks for reuse, and the allocator keeps its pages. One worker reaches its bound after about 40,000 requests. The test therefore runs the server on 4 workers (`--threads`), so each reaches its bound within the run, and only judges memory after the first 40% of it. On every core, with 18 workers sharing the load, a worker's warm-up alone would take longer than 10 minutes.

## Results

First runs, 2026-11-08, on an 18-core M5 MacBook Pro, the native server on 4 workers:

| Run | Requests | Throughput | p50 | p99 | Descriptors | RSS |
|---|---:|---:|---:|---:|---:|---|
| 10 min, full mix | 442,317 | 737/s | 0.57 ms | 16.5 ms | 20–30 | 17 → 86 MB, still growing |
| 10 min, no crashes | 590,871 | 985/s | 0.32 ms | 12.4 ms | 20–29 | 21 → 66 MB |
| 30 min, no crashes | 1,640,645 | 912/s | 0.31 ms | 14.0 ms | 21–31 | 14 → 66 MB by minute 12, then 76 MB by minute 30 |
| 10 min, no crashes, after #981 | 357,790 | 596/s | 0.38 ms | 30.3 ms | 21–30 | 22 → 61 MB by minute 5.6, then 62.6 MB by minute 10 |
| 30 min, full mix, 2026-11-09 | 2,849,438 | 1583/s | 0.27 ms | 6.2 ms | 20–30 | 30 → 128 MB, 1.2 MB a minute through the last 10 |
| 30 min, full mix, pages given back | 3,574,266 | 1986/s | 0.24 ms | 3.9 ms | 21–30 | 51 → 84 MB by minute 20, then 85.8 MB by minute 30 |

In every run the server never stopped, each crash ended only its own request and connection, and every other request was answered.

**Where it stands:**
- **No leak is left that the runtime can count.** `live::a_server_holds_no_more_after_thousands_of_requests_of_each_kind` drives each kind of request 1,500 more times after a warm-up and requires the runtime's live count to be the same. Crashes are among them. That count, `FENNEC_LIVE_REPORT`, covers live blocks by size, temporaries, inputs, cached texts, names, parked tasks and connections. Unlike RSS, it doesn't move with caches filling.
- **Crashes:** what looked like a leak of 1.8 KB a crash was the temporaries leak #981 fixed, since a crash leaves its function without truncating. On main, 90,074 crashes in a minute hold RSS at 7.8 MB.
- **What RSS still does:** it rises for the first minutes as each worker's cache of freed blocks fills, up to 8 MB a thread, and the allocator's pages with it. The live report shows both on its `cached` lines. The slow rise that was still going at 30 minutes was each worker's table of blocks: it kept a 64 KB page for every place a block had ever been, and file writes' 8 KB bodies land somewhere new each time. Now the table gives back pages with no block on them, and its pages hold at about 240. What's left over the last 10 minutes, 0.18 MB a minute, is the cache of freed blocks still filling, at under half its bound. The live count is the test of a leak; RSS isn't.

Bisecting, on one worker for 6 minutes each, before #981:

| Request | Requests | RSS |
|---|---:|---|
| `/note` (SQLite) | 175,559 | flat at 16 MB |
| `/stats` | 3,269,325 | flat at 6 MB |
| `/file` | 620,984 | 16 → 44 MB |
| not HTTP | 2,682,260 | 6 → 51 MB |
| half a request | 4,206,872 | 10 → 110 MB |

`soak.py` judges memory by RSS, so on a machine shared with other work it can still report growth from caches filling. The live-count test above is the one that says whether anything leaks.
