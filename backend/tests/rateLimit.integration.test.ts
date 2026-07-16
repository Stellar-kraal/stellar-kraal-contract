import request from 'supertest';
import { createApp } from '../src/app';
import { RedisLike } from '../src/common/rateLimit/store';
import { RateLimitConfig } from '../src/config/rateLimits';
import { Store } from '../src/db/database';

/** Small limits so tests exercise boundaries quickly. */
const TEST_LIMITS: RateLimitConfig = {
  tiers: {
    ipBurst: { limit: 5, windowSeconds: 10 },
    userSustained: { limit: 8, windowSeconds: 60 },
  },
  endpoints: {
    'GET /marketplace/prices': { limit: 3, windowSeconds: 60 },
  },
  exemptPaths: ['/health', '/metrics'],
};

function makeTestApp(overrides: { redis?: RedisLike } = {}) {
  let clock = 1_700_000_000;
  const config = { idempotencyTtlSeconds: 3600, now: () => clock };
  const built = createApp({
    store: new Store(':memory:'),
    config,
    rateLimitConfig: TEST_LIMITS,
    trustProxy: true,
    redis: overrides.redis,
  });
  return { ...built, advance: (seconds: number) => (clock += seconds) };
}

/** Unsigned JWT with the given subject — identity only, never verified. */
function bearerFor(sub: string): string {
  const b64 = (o: object) => Buffer.from(JSON.stringify(o)).toString('base64url');
  return `Bearer ${b64({ alg: 'none', typ: 'JWT' })}.${b64({ sub })}.sig`;
}

describe('per-IP burst tier', () => {
  test('requests beyond the limit get 429; another IP is unaffected', async () => {
    const { app } = makeTestApp();

    for (let i = 0; i < 5; i++) {
      const res = await request(app)
        .get('/marketplace/listings')
        .set('X-Forwarded-For', '10.0.0.1');
      expect(res.status).toBe(200);
    }

    const blocked = await request(app)
      .get('/marketplace/listings')
      .set('X-Forwarded-For', '10.0.0.1');
    expect(blocked.status).toBe(429);

    const otherIp = await request(app)
      .get('/marketplace/listings')
      .set('X-Forwarded-For', '10.0.0.2');
    expect(otherIp.status).toBe(200);
  });

  test('window slides: allowance returns after the window passes', async () => {
    const { app, advance } = makeTestApp();

    for (let i = 0; i < 6; i++) {
      await request(app).get('/marketplace/listings').set('X-Forwarded-For', '10.0.0.3');
    }
    const blocked = await request(app)
      .get('/marketplace/listings')
      .set('X-Forwarded-For', '10.0.0.3');
    expect(blocked.status).toBe(429);

    advance(2 * TEST_LIMITS.tiers.ipBurst.windowSeconds);

    const recovered = await request(app)
      .get('/marketplace/listings')
      .set('X-Forwarded-For', '10.0.0.3');
    expect(recovered.status).toBe(200);
  });
});

describe('per-user sustained tier', () => {
  test('a JWT identity is limited across source IPs', async () => {
    const { app } = makeTestApp();
    const auth = bearerFor('user-42');

    // Spread requests over many IPs so the ip-burst tier never trips;
    // the user-sustained tier (8/60s) must still catch them.
    let blocked: request.Response | undefined;
    for (let i = 0; i < 10; i++) {
      const res = await request(app)
        .get('/marketplace/listings')
        .set('Authorization', auth)
        .set('X-Forwarded-For', `10.1.0.${i}`);
      if (res.status === 429) {
        blocked = res;
        break;
      }
    }
    expect(blocked).toBeDefined();
    expect(blocked!.body.detail).toMatch(/user-sustained/);
  });

  test('anonymous requests are not subject to the user tier', async () => {
    const { app } = makeTestApp();
    // 8 requests from 8 different IPs: user tier would trip at >8 if it
    // (wrongly) applied; each IP is far under the ip-burst limit.
    for (let i = 0; i < 8; i++) {
      const res = await request(app)
        .get('/marketplace/listings')
        .set('X-Forwarded-For', `10.2.0.${i}`);
      expect(res.status).toBe(200);
    }
  });
});

describe('per-endpoint tier for expensive operations', () => {
  test('the oracle-read endpoint has its own tighter limit', async () => {
    const { app } = makeTestApp();

    for (let i = 0; i < 3; i++) {
      const res = await request(app)
        .get('/marketplace/prices')
        .set('X-Forwarded-For', '10.0.1.1');
      expect(res.status).toBe(200);
    }

    const blocked = await request(app)
      .get('/marketplace/prices')
      .set('X-Forwarded-For', '10.0.1.1');
    expect(blocked.status).toBe(429);
    expect(blocked.body.detail).toMatch(/endpoint limit of 3/);

    // The same client can still use endpoints without a tight per-endpoint
    // limit — the ip-burst budget (5) is not yet exhausted.
    const other = await request(app)
      .get('/marketplace/listings')
      .set('X-Forwarded-For', '10.0.1.1');
    expect(other.status).toBe(200);
  });
});

describe('429 response contract', () => {
  async function trigger429(app: Parameters<typeof request>[0]) {
    let last: request.Response | undefined;
    for (let i = 0; i < 7; i++) {
      last = await request(app).get('/marketplace/listings').set('X-Forwarded-For', '10.0.2.1');
    }
    return last!;
  }

  test('RFC 7807 problem+json body with Retry-After and X-RateLimit-* headers', async () => {
    const { app } = makeTestApp();
    const res = await trigger429(app);

    expect(res.status).toBe(429);
    expect(res.headers['content-type']).toMatch(/application\/problem\+json/);
    expect(res.body).toMatchObject({
      type: 'https://stellarkraal.dev/problems/rate-limit-exceeded',
      title: 'Too Many Requests',
      status: 429,
      instance: '/marketplace/listings',
    });
    expect(res.body.detail).toMatch(/ip-burst limit of 5/);

    const retryAfter = Number(res.headers['retry-after']);
    expect(retryAfter).toBeGreaterThanOrEqual(1);
    expect(retryAfter).toBeLessThanOrEqual(TEST_LIMITS.tiers.ipBurst.windowSeconds);
    expect(res.body.retryAfterSeconds).toBe(retryAfter);

    expect(res.headers['x-ratelimit-limit']).toBe('5');
    expect(res.headers['x-ratelimit-remaining']).toBe('0');
    expect(Number(res.headers['x-ratelimit-reset'])).toBeGreaterThan(1_700_000_000);
  });

  test('successful responses advertise the remaining budget', async () => {
    const { app } = makeTestApp();

    const first = await request(app)
      .get('/marketplace/listings')
      .set('X-Forwarded-For', '10.0.3.1');
    expect(first.headers['x-ratelimit-limit']).toBe('5');
    expect(first.headers['x-ratelimit-remaining']).toBe('4');

    const second = await request(app)
      .get('/marketplace/listings')
      .set('X-Forwarded-For', '10.0.3.1');
    expect(second.headers['x-ratelimit-remaining']).toBe('3');
  });

  test('health and metrics endpoints are exempt', async () => {
    const { app } = makeTestApp();
    for (let i = 0; i < 20; i++) {
      const res = await request(app).get('/health');
      expect(res.status).toBe(200);
      expect(res.headers['x-ratelimit-limit']).toBeUndefined();
    }
  });
});

describe('Redis store and in-memory failover', () => {
  class StubRedis implements RedisLike {
    counters = new Map<string, number>();
    calls = 0;
    failing = false;

    async incr(key: string): Promise<number> {
      this.calls += 1;
      if (this.failing) throw new Error('redis connection lost');
      const next = (this.counters.get(key) ?? 0) + 1;
      this.counters.set(key, next);
      return next;
    }
    async expire(): Promise<unknown> {
      if (this.failing) throw new Error('redis connection lost');
      return 1;
    }
    async get(key: string): Promise<string | null> {
      if (this.failing) throw new Error('redis connection lost');
      const v = this.counters.get(key);
      return v === undefined ? null : String(v);
    }
  }

  test('limits are enforced through Redis when it is healthy', async () => {
    const redis = new StubRedis();
    const { app } = makeTestApp({ redis });

    for (let i = 0; i < 5; i++) {
      await request(app).get('/marketplace/listings').set('X-Forwarded-For', '10.0.4.1');
    }
    const blocked = await request(app)
      .get('/marketplace/listings')
      .set('X-Forwarded-For', '10.0.4.1');
    expect(blocked.status).toBe(429);
    expect(redis.calls).toBeGreaterThan(0);
  });

  test('when Redis fails, enforcement continues via the in-memory fallback', async () => {
    const redis = new StubRedis();
    const { app, metrics } = makeTestApp({ redis });

    // Warm up through Redis, then kill it.
    await request(app).get('/marketplace/listings').set('X-Forwarded-For', '10.0.5.1');
    redis.failing = true;

    // No 500s, and the limiter still enforces: the in-memory fallback
    // counts these 6 requests and rejects the overflow.
    const statuses: number[] = [];
    for (let i = 0; i < 6; i++) {
      const res = await request(app)
        .get('/marketplace/listings')
        .set('X-Forwarded-For', '10.0.5.1');
      statuses.push(res.status);
    }
    expect(statuses).not.toContain(500);
    expect(statuses[statuses.length - 1]).toBe(429);
    expect(metrics.snapshot().storeFailovers).toBeGreaterThanOrEqual(1);

    // Redis recovers: the failover store resumes using it automatically.
    redis.failing = false;
    const callsBefore = redis.calls;
    await request(app).get('/marketplace/listings').set('X-Forwarded-For', '10.0.6.1');
    expect(redis.calls).toBeGreaterThan(callsBefore);
  });
});

describe('observability', () => {
  test('rejected requests are counted by tier and endpoint in /metrics', async () => {
    const { app } = makeTestApp();

    for (let i = 0; i < 8; i++) {
      await request(app).get('/marketplace/listings').set('X-Forwarded-For', '10.0.7.1');
    }

    const metricsRes = await request(app).get('/metrics');
    expect(metricsRes.status).toBe(200);
    const snapshot = metricsRes.body.rateLimit;
    expect(snapshot.rejectedTotal).toBeGreaterThanOrEqual(2);
    expect(snapshot.rejectedByTier['ip-burst']).toBeGreaterThanOrEqual(2);
    expect(snapshot.rejectedByEndpoint['GET /marketplace/listings']).toBeGreaterThanOrEqual(2);
    expect(snapshot.allowedTotal).toBeGreaterThanOrEqual(5);
  });
});
