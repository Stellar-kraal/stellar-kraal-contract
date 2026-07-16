/**
 * Sliding-window counter storage.
 *
 * The estimator is the classic two-bucket approximation: requests are
 * counted into fixed windows, and the effective count is
 *
 *   current + previous * (1 - elapsedFractionOfWindow)
 *
 * which bounds bursts at window boundaries without storing per-request
 * timestamps.
 */

export interface WindowCounts {
  currentCount: number;
  previousCount: number;
  /** Epoch seconds at which the current fixed window began. */
  windowStart: number;
}

export interface RateLimitStore {
  readonly name: string;
  /** Record one arrival for `bucket` and return the window counters. */
  increment(bucket: string, windowSeconds: number, now: number): Promise<WindowCounts>;
}

export function slidingWindowCount(counts: WindowCounts, windowSeconds: number, now: number): number {
  const elapsed = now - counts.windowStart;
  const previousWeight = Math.max(0, 1 - elapsed / windowSeconds);
  return counts.currentCount + counts.previousCount * previousWeight;
}

// ── In-memory implementation ─────────────────────────────────────────────

interface MemoryEntry {
  windowIndex: number;
  currentCount: number;
  previousCount: number;
  lastSeen: number;
}

const SWEEP_THRESHOLD = 5000;
const SWEEP_IDLE_SECONDS = 300;

export class InMemoryRateLimitStore implements RateLimitStore {
  readonly name = 'memory';
  private readonly entries = new Map<string, MemoryEntry>();

  async increment(bucket: string, windowSeconds: number, now: number): Promise<WindowCounts> {
    const windowIndex = Math.floor(now / windowSeconds);
    const entry = this.entries.get(bucket);

    let next: MemoryEntry;
    if (!entry || entry.windowIndex < windowIndex - 1) {
      next = { windowIndex, currentCount: 1, previousCount: 0, lastSeen: now };
    } else if (entry.windowIndex === windowIndex - 1) {
      next = { windowIndex, currentCount: 1, previousCount: entry.currentCount, lastSeen: now };
    } else {
      next = { ...entry, currentCount: entry.currentCount + 1, lastSeen: now };
    }
    this.entries.set(bucket, next);

    if (this.entries.size > SWEEP_THRESHOLD) {
      this.sweep(now);
    }

    return {
      currentCount: next.currentCount,
      previousCount: next.previousCount,
      windowStart: windowIndex * windowSeconds,
    };
  }

  private sweep(now: number): void {
    for (const [bucket, entry] of this.entries) {
      if (now - entry.lastSeen > SWEEP_IDLE_SECONDS) {
        this.entries.delete(bucket);
      }
    }
  }
}

// ── Redis implementation ─────────────────────────────────────────────────

/**
 * Minimal command surface needed from a Redis client. Both `ioredis` and
 * `node-redis` (v4, via a thin adapter) satisfy it, and tests inject a
 * scriptable stub.
 */
export interface RedisLike {
  incr(key: string): Promise<number>;
  expire(key: string, seconds: number): Promise<unknown>;
  get(key: string): Promise<string | null>;
}

export class RedisRateLimitStore implements RateLimitStore {
  readonly name = 'redis';

  constructor(private readonly redis: RedisLike) {}

  async increment(bucket: string, windowSeconds: number, now: number): Promise<WindowCounts> {
    const windowIndex = Math.floor(now / windowSeconds);
    const currentKey = `rl:${bucket}:${windowIndex}`;
    const previousKey = `rl:${bucket}:${windowIndex - 1}`;

    const currentCount = await this.redis.incr(currentKey);
    if (currentCount === 1) {
      // First hit in this window: bound the key's lifetime to two windows.
      await this.redis.expire(currentKey, windowSeconds * 2);
    }
    const previous = await this.redis.get(previousKey);

    return {
      currentCount,
      previousCount: previous ? Number(previous) : 0,
      windowStart: windowIndex * windowSeconds,
    };
  }
}

// ── Failover wrapper ─────────────────────────────────────────────────────

/**
 * Prefers Redis (multi-instance accuracy) and falls back to the in-memory
 * store whenever a Redis operation fails, so a Redis outage degrades to
 * single-instance rate limiting instead of taking the API down or
 * disabling protection. Redis is retried on every request, so recovery is
 * automatic.
 */
export class FailoverRateLimitStore implements RateLimitStore {
  readonly name = 'redis+memory-failover';
  private lastUsed: 'redis' | 'memory' = 'redis';

  constructor(
    private readonly redis: RateLimitStore,
    private readonly fallback: RateLimitStore,
    private readonly onFailover?: (error: unknown) => void,
  ) {}

  usingFallback(): boolean {
    return this.lastUsed === 'memory';
  }

  async increment(bucket: string, windowSeconds: number, now: number): Promise<WindowCounts> {
    try {
      const counts = await this.redis.increment(bucket, windowSeconds, now);
      this.lastUsed = 'redis';
      return counts;
    } catch (err) {
      if (this.lastUsed !== 'memory') {
        this.onFailover?.(err);
      }
      this.lastUsed = 'memory';
      return this.fallback.increment(bucket, windowSeconds, now);
    }
  }
}
