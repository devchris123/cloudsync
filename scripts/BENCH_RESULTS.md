# Upload Benchmark Results (2026-04-20)

Tested parallel chunk uploads against cloudsync.devchris.dev (Hetzner CX22 VPS).

## Setup

- Home connection: ~40 Mbps upload / 100 Mbps download
- CHUNK_SIZE: 4MB
- Server: Hetzner CX22, Ubuntu 24.04

## Results

### 100MB file (25 chunks)

| Batch size | Time  |
|------------|-------|
| 1          | 24s   |
| 5          | 21s   |

### 500MB file (125 chunks)

| Batch size | Time    |
|------------|---------|
| 1          | 1m 48s  |
| 5          | ~1m 44s |
| 20         | 1m 44s  |

## Analysis

Measured upload throughput: ~4.6 MB/s (~37 Mbps), which is ~92% of the 40 Mbps connection cap.

**Bandwidth is the bottleneck, not latency.** Parallel sends only eliminate idle time between server round-trips (the gap waiting for "chunk received" before sending the next). Once the pipe is saturated (~batch=5), adding more parallel streams doesn't help — chunks queue at the OS TCP buffer level and take turns on the wire.

## Why we still use batch=5

- Captures the small latency-gap savings (~3-4s on large files)
- Helps more on high-latency connections (e.g. overseas servers)
- No downside when bandwidth-limited — same total throughput
- The real value of chunked uploads is **resumability**, not speed
