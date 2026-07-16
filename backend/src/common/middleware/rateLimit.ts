import { NextFunction, Request, RequestHandler, Response } from 'express';
import { AppConfig } from '../../config';
import { RateLimitConfig, WindowLimit } from '../../config/rateLimits';
import { RateLimitMetrics } from '../metrics';
import { RateLimitStore, slidingWindowCount } from '../rateLimit/store';

export interface RateLimitDeps {
  store: RateLimitStore;
  config: AppConfig;
  limits: RateLimitConfig;
  metrics: RateLimitMetrics;
}

interface TierCheck {
  tier: 'endpoint' | 'ip-burst' | 'user-sustained';
  bucket: string;
  limit: WindowLimit;
}

interface TierResult extends TierCheck {
  estimated: number;
  remaining: number;
  /** Epoch seconds when the current fixed window ends. */
  reset: number;
}

/**
 * Extract a stable identity from a Bearer JWT payload WITHOUT verifying the
 * signature. This is deliberate: the rate limiter only needs a stable
 * bucketing key and must not turn into a second authenticator — signature
 * verification stays in the auth middleware. Unverifiable or absent tokens
 * simply mean the per-user tier does not apply.
 */
export function jwtIdentity(authorizationHeader: string | undefined): string | undefined {
  if (!authorizationHeader?.startsWith('Bearer ')) return undefined;
  const parts = authorizationHeader.slice('Bearer '.length).split('.');
  if (parts.length !== 3) return undefined;
  try {
    const payload = JSON.parse(Buffer.from(parts[1], 'base64url').toString('utf8')) as Record<
      string,
      unknown
    >;
    const sub = payload.sub ?? payload.userId;
    return typeof sub === 'string' && sub.length > 0 ? sub : undefined;
  } catch {
    return undefined;
  }
}

function endpointId(req: Request): string {
  const path = req.path.length > 1 && req.path.endsWith('/') ? req.path.slice(0, -1) : req.path;
  return `${req.method} ${path}`;
}

function reject(res: Response, violated: TierResult, retryAfter: number, instance: string): void {
  res
    .status(429)
    .set('Retry-After', String(retryAfter))
    .set('X-RateLimit-Limit', String(violated.limit.limit))
    .set('X-RateLimit-Remaining', '0')
    .set('X-RateLimit-Reset', String(violated.reset))
    .type('application/problem+json')
    .json({
      type: 'https://stellarkraal.dev/problems/rate-limit-exceeded',
      title: 'Too Many Requests',
      status: 429,
      detail:
        `${violated.tier} limit of ${violated.limit.limit} requests per ` +
        `${violated.limit.windowSeconds}s exceeded; retry after ${retryAfter}s`,
      instance,
      retryAfterSeconds: retryAfter,
    });
}

/**
 * Multi-tier sliding-window rate limiter for all public API routes.
 *
 * Tier evaluation order: per-endpoint (expensive operations), per-IP burst,
 * per-user sustained. Every arrival is counted in every applicable tier —
 * including arrivals that end up rejected — so a flood cannot reset its own
 * window by being rejected.
 *
 * Fail-open by design: an unexpected limiter error must not take the API
 * down (Redis outages degrade through FailoverRateLimitStore first).
 */
export function rateLimit(deps: RateLimitDeps): RequestHandler {
  const { store, config, limits, metrics } = deps;

  return (req: Request, res: Response, next: NextFunction) => {
    if (limits.exemptPaths.includes(req.path)) {
      next();
      return;
    }

    void (async () => {
      const now = config.now();
      const endpoint = endpointId(req);
      const ip = req.ip ?? 'unknown';
      const user = jwtIdentity(req.header('Authorization'));
      const clientId = user ? `user:${user}` : `ip:${ip}`;

      const checks: TierCheck[] = [];
      const endpointLimit = limits.endpoints[endpoint];
      if (endpointLimit) {
        checks.push({ tier: 'endpoint', bucket: `ep:${endpoint}:${clientId}`, limit: endpointLimit });
      }
      checks.push({ tier: 'ip-burst', bucket: `ip:${ip}`, limit: limits.tiers.ipBurst });
      if (user) {
        checks.push({
          tier: 'user-sustained',
          bucket: `user:${user}`,
          limit: limits.tiers.userSustained,
        });
      }

      const results: TierResult[] = [];
      for (const check of checks) {
        const counts = await store.increment(check.bucket, check.limit.windowSeconds, now);
        const estimated = slidingWindowCount(counts, check.limit.windowSeconds, now);
        results.push({
          ...check,
          estimated,
          remaining: Math.max(0, Math.floor(check.limit.limit - estimated)),
          reset: counts.windowStart + check.limit.windowSeconds,
        });
      }

      const violated = results.find((r) => r.estimated > r.limit.limit);
      if (violated) {
        const retryAfter = Math.max(1, violated.reset - now);
        metrics.recordRejected(violated.tier, endpoint);
        reject(res, violated, retryAfter, req.originalUrl);
        return;
      }

      // Advertise the most constrained applicable tier on success.
      const tightest = results.reduce((a, b) => (b.remaining < a.remaining ? b : a));
      res
        .set('X-RateLimit-Limit', String(tightest.limit.limit))
        .set('X-RateLimit-Remaining', String(tightest.remaining))
        .set('X-RateLimit-Reset', String(tightest.reset));
      metrics.recordAllowed();
      next();
    })().catch((err) => {
      // eslint-disable-next-line no-console
      console.error('rate limiter error (failing open):', err);
      next();
    });
  };
}
