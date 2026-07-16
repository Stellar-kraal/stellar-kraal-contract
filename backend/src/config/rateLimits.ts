/**
 * Single source of truth for all rate limits on the public marketplace API.
 *
 * Three tiers are enforced independently; a request is rejected as soon as
 * any applicable tier is exhausted:
 *
 *  - `ipBurst`        — short-window burst protection, keyed per client IP.
 *  - `userSustained`  — longer-window sustained limit, keyed per JWT
 *                       identity (`sub` claim); only applies to
 *                       authenticated requests.
 *  - `endpoints`      — per-endpoint limits for expensive operations
 *                       (on-chain queries, oracle reads), keyed per client
 *                       (user identity when present, IP otherwise).
 */

export interface WindowLimit {
  /** Maximum requests allowed within the sliding window. */
  limit: number;
  windowSeconds: number;
}

export interface RateLimitConfig {
  tiers: {
    ipBurst: WindowLimit;
    userSustained: WindowLimit;
  };
  /** Keys are `"METHOD /path"` as routed, e.g. `"GET /marketplace/prices"`. */
  endpoints: Record<string, WindowLimit>;
  /** Paths never rate limited (health probes, metrics scrapes). */
  exemptPaths: string[];
}

export const RATE_LIMIT_CONFIG: RateLimitConfig = {
  tiers: {
    ipBurst: { limit: 20, windowSeconds: 10 },
    userSustained: { limit: 300, windowSeconds: 60 },
  },
  endpoints: {
    // Oracle read — most expensive public operation.
    'GET /marketplace/prices': { limit: 10, windowSeconds: 60 },
    // Listing browse/scrape target.
    'GET /marketplace/listings': { limit: 60, windowSeconds: 60 },
    // Purchase initiation triggers an on-chain submission.
    'POST /marketplace/purchases': { limit: 10, windowSeconds: 60 },
    'POST /marketplace/listings': { limit: 20, windowSeconds: 60 },
    'POST /credits/retire': { limit: 20, windowSeconds: 60 },
  },
  exemptPaths: ['/health', '/metrics'],
};
