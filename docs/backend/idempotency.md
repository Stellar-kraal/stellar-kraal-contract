# Idempotency Layer for Critical Financial Endpoints

The marketplace purchase, credit retirement, and listing creation endpoints
execute an on-chain transaction and then persist backend state. A client
retry after a network timeout must never double-execute either side. The
backend therefore enforces a server-side idempotency key protocol on:

| Endpoint | Operation |
|---|---|
| `POST /marketplace/listings` | Listing creation |
| `POST /marketplace/purchases` | Marketplace purchase |
| `POST /credits/retire` | Credit retirement |

## Protocol

1. Clients MUST send an `Idempotency-Key` header (1–255 visible ASCII
   characters, e.g. a UUID). A missing or malformed key is rejected with
   `400` before any execution.
2. The backend fingerprints each request (SHA-256 over method, path, and
   the canonicalized JSON body) and stores it with the key.
3. A duplicate request — same key, same fingerprint — arriving within the
   configurable TTL window returns `200` with the **original response
   body** and the header `Idempotent-Replayed: true`, without re-executing
   anything (no second on-chain submission, no second database write).
4. Reusing a key with a **different** payload is rejected with `422`.
5. A request whose first attempt is still executing returns `409`.
6. After the TTL expires, the key is forgotten and a duplicate is treated
   as a brand-new request.

Only successful (2xx) responses are cached. Business rejections (e.g.
purchasing more than a listing's remaining quantity) and validation
failures are never cached, so the same key can be retried with a corrected
payload.

## Idempotency record schema

Records live in the `idempotency_records` table (SQLite):

| Column | Type | Description |
|---|---|---|
| `key` | `TEXT` (PK) | The client-supplied `Idempotency-Key` |
| `fingerprint` | `TEXT` | SHA-256 of `method path \n canonical-JSON-body` |
| `endpoint` | `TEXT` | Stable endpoint id, e.g. `POST /marketplace/purchases` |
| `status` | `TEXT` | `in_progress` while executing, then `completed` |
| `response_status` | `INTEGER` | Original HTTP status (populated when completed) |
| `response_body` | `TEXT` | Original JSON response body (populated when completed) |
| `created_at` | `INTEGER` | Epoch seconds |
| `expires_at` | `INTEGER` | `created_at + IDEMPOTENCY_TTL_SECONDS`; record is ignored and purged after this |

The TTL window is configured with the `IDEMPOTENCY_TTL_SECONDS`
environment variable (default `86400`, i.e. 24 h).

## Partial-failure reconciliation

The dangerous failure mode is: **the on-chain transaction succeeded, but
the backend database write failed** before the domain row and cached
response could be stored. Re-executing on retry would double-spend on
chain; doing nothing would leave the backend permanently out of sync.

Every on-chain submission carries the request's idempotency key as its
deduplication id, so emitted chain events are addressable by key. On each
incoming request, after the record checks, the middleware queries the
chain for an event carrying the request's key:

- **Event found (within TTL), no completed record** → partial failure
  detected. The endpoint's *reconciler* replays the event: domain state is
  rebuilt idempotently from the event payload (entity ids are derived
  deterministically from the key, and purchase fills are only applied if
  the purchase row is absent), the response is reconstructed and cached,
  and the client receives `200` with `Idempotent-Replayed: true` and
  `Idempotency-Reconciled: true`. No second on-chain submission occurs.
- **No event found** → the first attempt failed before reaching the
  chain; the request executes normally.

The flow, end to end:

```
request ──► key present? ──► record fresh + completed? ──► replay cache (200)
                │                    │
                │ 400                │ fingerprint mismatch → 422
                │                    │ in flight, nothing on chain → 409
                ▼                    ▼
              reject          chain event for key within TTL?
                                     │
                        yes ─────────┴───────── no
                         │                      │
              reconcile from event      validate → execute:
              (idempotent replay,       1. submit on-chain (dedup = key)
               no re-execution)         2. tx: domain write + cache response
                                        failure after 1 → 500, safe to retry
```

## Running the tests

```bash
cd backend
npm install
npm test
```

The integration suite (`backend/tests/idempotency.integration.test.ts`)
covers the happy path for all three endpoints, duplicate replay (including
double-decrement protection on purchases), key-reuse conflicts, in-flight
conflicts, partial-failure recovery via on-chain event replay, TTL
expiration on both sides of the window boundary, and non-caching of
business rejections. The suite uses an in-memory database, a simulated
chain client, and an injected clock, so it is fully deterministic.
