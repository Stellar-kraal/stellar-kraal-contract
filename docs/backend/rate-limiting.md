# Rate Limiting, Request Throttling, and DDoS Resilience

The public marketplace API (price queries, listing browsing, purchase
initiation) is protected by a multi-tier sliding-window rate limiter
applied to every public route. WAF-layer mitigation, CDN integration, and
bot detection are intentionally out of scope here — this layer makes the
application itself resilient to scraping and request flooding.

## Tiers

All limits live in a single config file:
[`backend/src/config/rateLimits.ts`](../../backend/src/config/rateLimits.ts).

| Tier | Key | Default | Purpose |
|---|---|---|---|
| `ip-burst` | client IP | 20 req / 10 s | Short-window flood protection for every request |
| `user-sustained` | JWT `sub` claim | 300 req / 60 s | Sustained budget per authenticated identity, across IPs |
| `endpoint` | endpoint + client | per endpoint (below) | Tighter budgets for expensive operations |

Per-endpoint defaults:

| Endpoint | Limit | Rationale |
|---|---|---|
| `GET /marketplace/prices` | 10 / 60 s | Oracle read — most expensive public query |
| `GET /marketplace/listings` | 60 / 60 s | Primary scraping target |
| `POST /marketplace/purchases` | 10 / 60 s | Triggers an on-chain submission |
| `POST /marketplace/listings` | 20 / 60 s | On-chain submission |
| `POST /credits/retire` | 20 / 60 s | On-chain submission |

A request is rejected as soon as **any** applicable tier is exhausted.
Every arrival is counted in every applicable tier — including arrivals
that get rejected — so a flood cannot reset its own window. `/health` and
`/metrics` are exempt.

The per-user tier extracts the `sub` claim from a Bearer JWT **without
verifying the signature** — the limiter only needs a stable bucketing key
and must not act as a second authenticator; verification belongs to the
auth middleware. Anonymous requests are bucketed by IP only.

## Algorithm: sliding window counter

Requests are counted into fixed windows and the effective rate is the
two-bucket sliding-window estimate:

```
estimated = current + previous × (1 − elapsed/window)
```

This bounds boundary bursts (the classic fixed-window flaw) while storing
only two counters per bucket.

## Storage: Redis with in-memory failover

The counter store is pluggable (`backend/src/common/rateLimit/store.ts`):

- **`InMemoryRateLimitStore`** — default for single-instance deployments;
  zero dependencies, idle buckets are swept automatically.
- **`RedisRateLimitStore`** — used when `REDIS_URL` is set, so limits are
  shared across instances. Counter keys are `rl:{bucket}:{windowIndex}`
  with a two-window expiry.
- **`FailoverRateLimitStore`** — wraps Redis with the in-memory store: if
  a Redis operation fails, enforcement continues locally (never a 500,
  never unprotected) and Redis is retried on subsequent requests, so
  recovery is automatic. Failovers are counted in `/metrics`.

If the limiter itself throws unexpectedly it **fails open** (the request
proceeds): availability is preferred over strictness, and Redis outages
are already handled by the failover store before this can matter.

## Response contract

Successful responses advertise the most constrained applicable budget:

```
X-RateLimit-Limit: 20
X-RateLimit-Remaining: 13
X-RateLimit-Reset: 1700000010        (epoch seconds)
```

Throttled requests get `429 Too Many Requests` with `Retry-After` and an
[RFC 7807](https://www.rfc-editor.org/rfc/rfc7807) `application/problem+json`
body:

```http
HTTP/1.1 429 Too Many Requests
Retry-After: 7
X-RateLimit-Limit: 20
X-RateLimit-Remaining: 0
X-RateLimit-Reset: 1700000010
Content-Type: application/problem+json

{
  "type": "https://stellarkraal.dev/problems/rate-limit-exceeded",
  "title": "Too Many Requests",
  "status": 429,
  "detail": "ip-burst limit of 20 requests per 10s exceeded; retry after 7s",
  "instance": "/marketplace/listings",
  "retryAfterSeconds": 7
}
```

## Observability

`GET /metrics` exposes limiter counters as JSON — allowed total, rejected
total, rejections by tier and by endpoint, and store failovers. The shape
maps 1:1 onto Prometheus counters if an exporter is added later.

## Configuration

| Env var | Effect |
|---|---|
| `REDIS_URL` | Enables the Redis-backed store (with in-memory failover) |
| `TRUST_PROXY=1` | Honor `X-Forwarded-For` for client IPs — set **only** behind a trusted reverse proxy |

Limit values are code-reviewed configuration in
`backend/src/config/rateLimits.ts`, not env vars, so every change is
visible in history.

## Tests

```bash
cd backend
npm test           # integration tests (includes rate limiting suite)
npm run loadtest   # autocannon load test, boots the API on an ephemeral port
```

The integration suite (`backend/tests/rateLimit.integration.test.ts`)
covers per-IP enforcement and isolation between IPs, sliding-window
recovery, per-user limits across source IPs, per-endpoint limits,
RFC 7807 body and header correctness, exempt paths, Redis-backed
enforcement, failover to memory mid-flight (and automatic recovery), and
rejected-request metrics.

The load test runs a flooder (~100 req/s) and a compliant client
(1 req/s) concurrently for 10 s and asserts the flooder is throttled to
the configured budget while the compliant client sees zero 429s. Sample
run:

```
flooder:   1012 sent, 20 allowed, 992 throttled (429)
compliant: 10 sent, 10 allowed, 0 throttled (429)
PASS: flooder throttled to budget, compliant client unaffected
```
