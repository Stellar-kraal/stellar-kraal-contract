# Backend Graceful Shutdown & Health Checks

## Overview

The backend container exposes a `/health` endpoint and a `HEALTHCHECK` instruction
so container orchestrators (Docker Compose, Kubernetes, etc.) can tell when the
service is ready to serve traffic and when it has stopped. The Node.js process
also handles `SIGTERM` (and `SIGINT`) to shut down gracefully, draining
in-flight work before the process exits.

## Health Check

Defined in `backend/Dockerfile`:

```dockerfile
HEALTHCHECK --interval=10s --timeout=5s --start-period=15s --retries=3 \
  CMD curl -f http://localhost:3001/health
```

- **Interval**: probe every 10s.
- **Timeout**: each probe must return within 5s.
- **Start period**: 15s grace after container start before failures count.
- **Retries**: mark unhealthy after 3 consecutive failures.

The `/health` endpoint (in `backend/src/app.ts`) always returns `200 OK`
(structured JSON with indexer + webhook queue status), so the container is
considered healthy as soon as the HTTP server is bound.

## Graceful Shutdown (SIGTERM / SIGINT)

Implemented in `backend/src/index.ts`. On receipt of `SIGTERM` (or `SIGINT`):

1. A re-entrancy guard prevents double shutdown.
2. `indexer.stop()` halts the event-indexer poll loop so no new indexing ticks
   are scheduled. Any tick already in flight is allowed to complete.
3. `webhookDelivery.stop()` halts the webhook drain loop so no new delivery
   batches are scheduled. Any in-flight drain is allowed to complete.
4. `server.close()` stops accepting new connections while allowing existing
   in-flight HTTP requests to finish.
5. Once the server is fully closed the process exits with code `0`.
6. A 30s safety timeout force-exits with code `1` if shutdown stalls (e.g.
   hung keep-alive connections).

### Why this matters

Without `SIGTERM` handling the container runtime kills the process immediately
(`SIGKILL` after the default grace period), cutting off:

- **Indexer ticks** — a half-finished ledger batch would be abandoned, though
  the cursor is only advanced per page so at most one page is replayed on
  restart.
- **Webhook drains** — deliveries in flight would be interrupted; they are
  retried on the next start (at-least-once), but graceful shutdown lets the
  current attempt complete cleanly.

Graceful shutdown avoids abrupt connection resets for clients and lets the
background services finish their current unit of work.

## Local verification

```bash
# Build the image (requires Docker)
docker build -t stellarkraal-backend ./backend

# Inspect health status after starting the container
docker ps   # STATUS column shows (healthy)/(unhealthy)

# Trigger a graceful shutdown and watch the logs
docker stop <container_id>
```
