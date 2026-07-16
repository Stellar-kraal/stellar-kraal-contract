/**
 * Integration tests for Issue #71:
 *   - Cursor-based event indexer
 *   - Restart from persisted cursor
 *   - Pagination across multiple RPC pages
 *   - Duplicate event prevention
 *   - Webhook registration, update, secret rotation
 *   - HMAC signature generation
 *   - Signed webhook delivery
 *   - Retry with exponential backoff
 *   - No duplicate deliveries after restart
 *   - No missing events after restart
 *   - Health endpoint reporting indexer + queue status
 */

import request from 'supertest';
import { createApp } from '../src/app';
import { Store } from '../src/db/database';
import { StubSorobanRpcClient, RpcEvent } from '../src/chain/rpcClient';
import { EventIndexer } from '../src/indexer/eventIndexer';
import { WebhookDeliveryService, signPayload, HttpPoster } from '../src/webhook/deliveryService';

// ── Shared test harness ───────────────────────────────────────────────────

function makeTestEnv() {
  let clock = 1_700_000_000;
  const now = () => clock;
  const advance = (seconds: number) => { clock += seconds; };
  const config = {
    idempotencyTtlSeconds: 3600,
    now,
    rpcUrl: 'http://stub',
    contractId: 'CONTRACT_A',
    indexerPollIntervalMs: 999_999, // don't auto-poll in tests
    indexerMaxRpcAttempts: 3,
    webhookMaxAttempts: 3,
    webhookBaseBackoffSeconds: 30,
    webhookMaxBackoffSeconds: 3_600,
    webhookPollIntervalMs: 999_999,
  };
  const store = new Store(':memory:');
  const rpc = new StubSorobanRpcClient();

  const { app, indexer, webhookDelivery } = createApp({
    store,
    config,
    rpcClient: rpc,
    indexerTargets: [{ contractId: 'CONTRACT_A', eventType: 'listing_created' }],
  });

  return { app, store, rpc, indexer, webhookDelivery, config, advance, now };
}

function makeEvent(overrides: Partial<RpcEvent> = {}): RpcEvent {
  return {
    contractId: 'CONTRACT_A',
    topic: 'listing_created',
    transactionHash: 'txhash_' + Math.random().toString(36).slice(2),
    ledger: 100,
    payload: { amount: 42 },
    ...overrides,
  };
}

// ── HMAC signature ────────────────────────────────────────────────────────

describe('signPayload', () => {
  test('produces sha256= prefixed HMAC hex', () => {
    const sig = signPayload('mysecret', '{"foo":"bar"}');
    expect(sig).toMatch(/^sha256=[0-9a-f]{64}$/);
  });

  test('different secrets produce different signatures', () => {
    const payload = '{"event":"test"}';
    const sig1 = signPayload('secret1', payload);
    const sig2 = signPayload('secret2', payload);
    expect(sig1).not.toEqual(sig2);
  });

  test('same secret + payload always produces same signature (deterministic)', () => {
    const sig1 = signPayload('abc', 'hello');
    const sig2 = signPayload('abc', 'hello');
    expect(sig1).toEqual(sig2);
  });
});

// ── Cursor-based indexer ──────────────────────────────────────────────────

describe('EventIndexer – cursor persistence', () => {
  test('starts from ledger 0 when no cursor exists', async () => {
    const { store, rpc, indexer } = makeTestEnv();

    rpc.addPage('CONTRACT_A', 'listing_created', 0, {
      events: [makeEvent({ ledger: 50, transactionHash: 'tx1' })],
      nextLedger: undefined,
    });

    await indexer.tick();

    const cursor = store.getCursor('listing_created');
    expect(cursor?.last_ledger).toBe(50);
  });

  test('resumes from persisted cursor after restart', async () => {
    const { store, rpc, indexer } = makeTestEnv();

    // Simulate prior run: cursor already at ledger 50
    store.upsertCursor('listing_created', 50, 1_700_000_000);

    // Next tick should start from ledger 51
    rpc.addPage('CONTRACT_A', 'listing_created', 51, {
      events: [makeEvent({ ledger: 75, transactionHash: 'tx_new' })],
      nextLedger: undefined,
    });

    await indexer.tick();

    const cursor = store.getCursor('listing_created');
    expect(cursor?.last_ledger).toBe(75);
  });

  test('no events does not reset cursor', async () => {
    const { store, rpc, indexer } = makeTestEnv();
    store.upsertCursor('listing_created', 99, 1_700_000_000);

    rpc.addPage('CONTRACT_A', 'listing_created', 100, {
      events: [],
      nextLedger: undefined,
    });

    await indexer.tick();

    expect(store.getCursor('listing_created')?.last_ledger).toBe(99);
  });
});

describe('EventIndexer – pagination', () => {
  test('consumes multiple pages and advances cursor to highest ledger', async () => {
    const { store, rpc, indexer } = makeTestEnv();

    // Page 1: events at ledger 10, next page starts at 11
    rpc.addPage('CONTRACT_A', 'listing_created', 0, {
      events: [
        makeEvent({ ledger: 10, transactionHash: 'tx10a' }),
        makeEvent({ ledger: 10, transactionHash: 'tx10b' }),
      ],
      nextLedger: 11,
    });
    // Page 2: events at ledger 20, no more pages
    rpc.addPage('CONTRACT_A', 'listing_created', 11, {
      events: [makeEvent({ ledger: 20, transactionHash: 'tx20' })],
      nextLedger: undefined,
    });

    await indexer.tick();

    expect(store.getCursor('listing_created')?.last_ledger).toBe(20);

    // All three events should be indexed
    const conn = (store as unknown as { db: import('better-sqlite3').Database }).db;
    const rows = conn.prepare('SELECT * FROM indexed_events').all();
    expect(rows).toHaveLength(3);
  });
});

// ── Deduplication ──────────────────────────────────────────────────────────

describe('EventIndexer – deduplication', () => {
  test('duplicate event (same contract_id+topic+tx_hash) is not re-inserted', async () => {
    const { store, rpc, indexer } = makeTestEnv();

    const evt = makeEvent({ ledger: 100, transactionHash: 'dup_tx' });

    // First tick sees the event
    rpc.addPage('CONTRACT_A', 'listing_created', 0, {
      events: [evt],
      nextLedger: undefined,
    });
    await indexer.tick();

    const conn = (store as unknown as { db: import('better-sqlite3').Database }).db;
    expect((conn.prepare('SELECT COUNT(*) AS n FROM indexed_events').get() as { n: number }).n).toBe(1);

    // Cursor reset to simulate seeing the same event again (e.g. RPC returns overlap)
    store.upsertCursor('listing_created', 99, 1_700_000_000);
    rpc.addPage('CONTRACT_A', 'listing_created', 100, {
      events: [evt],
      nextLedger: undefined,
    });
    await indexer.tick();

    expect((conn.prepare('SELECT COUNT(*) AS n FROM indexed_events').get() as { n: number }).n).toBe(1);
  });

  test('duplicate event does not enqueue duplicate webhook delivery', async () => {
    const { store, rpc, indexer, app } = makeTestEnv();

    // Register a webhook
    const whRes = await request(app)
      .post('/webhooks')
      .send({ url: 'https://example.com/hook', eventType: 'listing_created' });
    expect(whRes.status).toBe(201);

    const evt = makeEvent({ ledger: 100, transactionHash: 'dup_tx2' });

    rpc.addPage('CONTRACT_A', 'listing_created', 0, {
      events: [evt],
      nextLedger: undefined,
    });
    await indexer.tick();

    const conn = (store as unknown as { db: import('better-sqlite3').Database }).db;
    expect((conn.prepare('SELECT COUNT(*) AS n FROM webhook_deliveries').get() as { n: number }).n).toBe(1);

    // See same event again
    store.upsertCursor('listing_created', 99, 1_700_000_000);
    rpc.addPage('CONTRACT_A', 'listing_created', 100, {
      events: [evt],
      nextLedger: undefined,
    });
    await indexer.tick();

    expect((conn.prepare('SELECT COUNT(*) AS n FROM webhook_deliveries').get() as { n: number }).n).toBe(1);
  });
});

// ── No missing events after restart ──────────────────────────────────────

describe('EventIndexer – no missing events after restart', () => {
  test('second indexer instance on same store picks up where first left off', async () => {
    const { store, rpc, config } = makeTestEnv();

    // First indexer run: processes events up to ledger 50
    const indexer1 = new EventIndexer(store, rpc, [{ contractId: 'CONTRACT_A', eventType: 'listing_created' }], { now: config.now });

    rpc.addPage('CONTRACT_A', 'listing_created', 0, {
      events: [makeEvent({ ledger: 50, transactionHash: 'first_run_tx' })],
      nextLedger: undefined,
    });
    await indexer1.tick();
    expect(store.getCursor('listing_created')?.last_ledger).toBe(50);

    // New indexer instance (simulating restart)
    const indexer2 = new EventIndexer(store, rpc, [{ contractId: 'CONTRACT_A', eventType: 'listing_created' }], { now: config.now });

    // Only events after ledger 50 should be fetched
    rpc.addPage('CONTRACT_A', 'listing_created', 51, {
      events: [makeEvent({ ledger: 60, transactionHash: 'second_run_tx' })],
      nextLedger: undefined,
    });
    await indexer2.tick();

    const conn = (store as unknown as { db: import('better-sqlite3').Database }).db;
    const rows = conn.prepare('SELECT * FROM indexed_events ORDER BY ledger').all() as Array<{ ledger: number }>;
    expect(rows).toHaveLength(2);
    expect(rows[0].ledger).toBe(50);
    expect(rows[1].ledger).toBe(60);
  });
});

// ── Webhook registration API ──────────────────────────────────────────────

describe('Webhook registration', () => {
  test('POST /webhooks creates a webhook and returns secret once', async () => {
    const { app } = makeTestEnv();
    const res = await request(app).post('/webhooks').send({
      url: 'https://example.com/hook',
      eventType: 'listing_created',
    });
    expect(res.status).toBe(201);
    expect(res.body.id).toBeDefined();
    expect(res.body.secret).toMatch(/^[0-9a-f]{64}$/);
    expect(res.body.eventType).toBe('listing_created');
    expect(res.body.active).toBe(1);
  });

  test('POST /webhooks rejects invalid URL', async () => {
    const { app } = makeTestEnv();
    const res = await request(app).post('/webhooks').send({
      url: 'not-a-url',
      eventType: 'listing_created',
    });
    expect(res.status).toBe(400);
  });

  test('GET /webhooks lists registered webhooks without secrets', async () => {
    const { app } = makeTestEnv();
    await request(app).post('/webhooks').send({ url: 'https://a.com', eventType: 'ev1' });
    await request(app).post('/webhooks').send({ url: 'https://b.com', eventType: 'ev2' });

    const res = await request(app).get('/webhooks');
    expect(res.status).toBe(200);
    expect(res.body.webhooks).toHaveLength(2);
    for (const wh of res.body.webhooks) {
      expect(wh.secret).toBeUndefined();
    }
  });

  test('PATCH /webhooks/:id updates url and active', async () => {
    const { app } = makeTestEnv();
    const created = await request(app).post('/webhooks').send({ url: 'https://old.com', eventType: 'ev' });
    const id = created.body.id as string;

    const res = await request(app)
      .patch(`/webhooks/${id}`)
      .send({ url: 'https://new.com', active: false });
    expect(res.status).toBe(200);
    expect(res.body.url).toBe('https://new.com');
    expect(res.body.active).toBe(0);
  });

  test('POST /webhooks/:id/rotate-secret returns new secret', async () => {
    const { app } = makeTestEnv();
    const created = await request(app).post('/webhooks').send({ url: 'https://x.com', eventType: 'ev' });
    const id = created.body.id as string;
    const oldSecret = created.body.secret as string;

    const res = await request(app).post(`/webhooks/${id}/rotate-secret`);
    expect(res.status).toBe(200);
    expect(res.body.secret).toMatch(/^[0-9a-f]{64}$/);
    expect(res.body.secret).not.toBe(oldSecret);
  });

  test('DELETE /webhooks/:id deactivates the webhook', async () => {
    const { app, store } = makeTestEnv();
    const created = await request(app).post('/webhooks').send({ url: 'https://x.com', eventType: 'ev' });
    const id = created.body.id as string;

    await request(app).delete(`/webhooks/${id}`);

    const row = store.getWebhook(id);
    expect(row?.active).toBe(0);
  });
});

// ── Webhook delivery ──────────────────────────────────────────────────────

describe('Webhook delivery', () => {
  test('delivers event to registered webhook with correct HMAC signature', async () => {
    const received: Array<{ body: string; sig: string }> = [];
    const poster: HttpPoster = {
      async post(url, body, headers) {
        received.push({ body, sig: headers['X-StellarKraal-Signature'] });
        return { status: 200, ok: true };
      },
    };

    const { app, store, rpc, indexer, config } = makeTestEnv();
    const deliveryService = new WebhookDeliveryService(store, {
      ...config,
      poster,
      now: config.now,
    });

    // Register webhook
    const whRes = await request(app).post('/webhooks').send({
      url: 'https://hooks.example.com/recv',
      eventType: 'listing_created',
    });
    const webhookSecret = whRes.body.secret as string;

    // Index an event
    rpc.addPage('CONTRACT_A', 'listing_created', 0, {
      events: [makeEvent({ ledger: 10, transactionHash: 'sig_test_tx' })],
      nextLedger: undefined,
    });
    await indexer.tick();

    // Drain deliveries
    await deliveryService.drain();

    expect(received).toHaveLength(1);
    const parsed = JSON.parse(received[0].body) as { transactionHash: string };
    expect(parsed.transactionHash).toBe('sig_test_tx');

    // Verify HMAC
    const expected = signPayload(webhookSecret, received[0].body);
    expect(received[0].sig).toBe(expected);
  });

  test('marks delivery as delivered on 2xx response', async () => {
    const poster: HttpPoster = {
      async post() { return { status: 200, ok: true }; },
    };
    const { app, store, rpc, indexer, config } = makeTestEnv();
    const deliveryService = new WebhookDeliveryService(store, { ...config, poster, now: config.now });

    await request(app).post('/webhooks').send({ url: 'https://ok.com', eventType: 'listing_created' });

    rpc.addPage('CONTRACT_A', 'listing_created', 0, {
      events: [makeEvent({ ledger: 5, transactionHash: 'ok_tx' })],
      nextLedger: undefined,
    });
    await indexer.tick();
    await deliveryService.drain();

    const conn = (store as unknown as { db: import('better-sqlite3').Database }).db;
    const row = conn.prepare('SELECT * FROM webhook_deliveries').get() as { status: string; attempt_count: number };
    expect(row.status).toBe('delivered');
    expect(row.attempt_count).toBe(1);
  });

  test('retries on non-2xx with exponential backoff', async () => {
    let callCount = 0;
    const poster: HttpPoster = {
      async post() {
        callCount++;
        return { status: 500, ok: false };
      },
    };
    const { app, store, rpc, indexer, config, advance } = makeTestEnv();
    const deliveryService = new WebhookDeliveryService(store, {
      ...config,
      poster,
      now: config.now,
      maxAttempts: 3,
      baseBackoffSeconds: 30,
    });

    await request(app).post('/webhooks').send({ url: 'https://fail.com', eventType: 'listing_created' });

    rpc.addPage('CONTRACT_A', 'listing_created', 0, {
      events: [makeEvent({ ledger: 5, transactionHash: 'retry_tx' })],
      nextLedger: undefined,
    });
    await indexer.tick();

    // Attempt 1
    await deliveryService.drain();
    expect(callCount).toBe(1);

    const conn = (store as unknown as { db: import('better-sqlite3').Database }).db;
    let row = conn.prepare('SELECT * FROM webhook_deliveries').get() as { status: string; attempt_count: number; next_attempt_at: number };
    expect(row.status).toBe('pending');
    expect(row.attempt_count).toBe(1);
    expect(row.next_attempt_at).toBeGreaterThan(config.now());

    // Advance past next_attempt_at (base 30s, attempt 1 → 30s delay)
    advance(60);

    // Attempt 2
    await deliveryService.drain();
    expect(callCount).toBe(2);
    row = conn.prepare('SELECT * FROM webhook_deliveries').get() as typeof row;
    expect(row.attempt_count).toBe(2);

    // Advance past attempt 3's next_attempt_at (30 * 2^1 = 60s)
    advance(120);

    // Attempt 3 — last attempt, should mark as 'failed'
    await deliveryService.drain();
    expect(callCount).toBe(3);
    row = conn.prepare('SELECT * FROM webhook_deliveries').get() as typeof row;
    expect(row.status).toBe('failed');
    expect(row.attempt_count).toBe(3);
  });

  test('stops retrying after success', async () => {
    let callCount = 0;
    const poster: HttpPoster = {
      async post() {
        callCount++;
        return callCount === 1 ? { status: 500, ok: false } : { status: 200, ok: true };
      },
    };
    const { app, store, rpc, indexer, config, advance } = makeTestEnv();
    const deliveryService = new WebhookDeliveryService(store, {
      ...config, poster, now: config.now, maxAttempts: 5, baseBackoffSeconds: 10,
    });

    await request(app).post('/webhooks').send({ url: 'https://mixed.com', eventType: 'listing_created' });

    rpc.addPage('CONTRACT_A', 'listing_created', 0, {
      events: [makeEvent({ ledger: 5, transactionHash: 'mixed_tx' })],
      nextLedger: undefined,
    });
    await indexer.tick();

    // Attempt 1 fails
    await deliveryService.drain();
    advance(20);

    // Attempt 2 succeeds
    await deliveryService.drain();

    const conn = (store as unknown as { db: import('better-sqlite3').Database }).db;
    const row = conn.prepare('SELECT * FROM webhook_deliveries').get() as { status: string; attempt_count: number };
    expect(row.status).toBe('delivered');
    expect(row.attempt_count).toBe(2);
    expect(callCount).toBe(2);

    // Further drains should not call poster again
    advance(60);
    await deliveryService.drain();
    expect(callCount).toBe(2);
  });

  test('no duplicate deliveries after restart', async () => {
    let callCount = 0;
    const poster: HttpPoster = {
      async post() { callCount++; return { status: 200, ok: true }; },
    };
    const { app, store, rpc, indexer, config } = makeTestEnv();

    await request(app).post('/webhooks').send({ url: 'https://nodupe.com', eventType: 'listing_created' });

    rpc.addPage('CONTRACT_A', 'listing_created', 0, {
      events: [makeEvent({ ledger: 10, transactionHash: 'nodupe_tx' })],
      nextLedger: undefined,
    });
    await indexer.tick();

    // First delivery service run — succeeds
    const svc1 = new WebhookDeliveryService(store, { ...config, poster, now: config.now });
    await svc1.drain();
    expect(callCount).toBe(1);

    // Simulate restart: new delivery service instance
    const svc2 = new WebhookDeliveryService(store, { ...config, poster, now: config.now });
    await svc2.drain();
    // Already delivered — should not re-deliver
    expect(callCount).toBe(1);
  });
});

// ── Health endpoint ───────────────────────────────────────────────────────

describe('Health endpoint', () => {
  test('returns indexer and webhook queue status', async () => {
    const { app } = makeTestEnv();
    const res = await request(app).get('/health');
    expect(res.status).toBe(200);
    expect(res.body.status).toBe('ok');
    expect(res.body.indexer).toMatchObject({
      running: expect.any(Boolean),
      lastProcessedLedger: expect.any(Object),
      lastTickAt: null,
      lastError: null,
    });
    expect(res.body.webhookQueue).toMatchObject({
      pendingCount: expect.any(Number),
    });
  });

  test('reports correct pending count after indexing events', async () => {
    const { app, store, rpc, indexer } = makeTestEnv();

    // Register two webhooks
    await request(app).post('/webhooks').send({ url: 'https://a.com', eventType: 'listing_created' });
    await request(app).post('/webhooks').send({ url: 'https://b.com', eventType: 'listing_created' });

    rpc.addPage('CONTRACT_A', 'listing_created', 0, {
      events: [makeEvent({ ledger: 10, transactionHash: 'health_tx' })],
      nextLedger: undefined,
    });
    await indexer.tick();

    const res = await request(app).get('/health');
    // 1 event × 2 webhooks = 2 pending deliveries
    expect(res.body.webhookQueue.pendingCount).toBe(2);
    expect(res.body.indexer.lastProcessedLedger['listing_created']).toBe(10);
  });

  test('health reports lastTickAt after indexer runs', async () => {
    const { app, rpc, indexer } = makeTestEnv();

    rpc.addPage('CONTRACT_A', 'listing_created', 0, { events: [], nextLedger: undefined });
    await indexer.tick();

    const res = await request(app).get('/health');
    expect(res.body.indexer.lastTickAt).not.toBeNull();
  });
});
