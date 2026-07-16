import express, { Express } from 'express';
import { ChainClient, SimulatedChainClient } from './chain/chainClient';
import { RateLimitMetrics } from './common/metrics';
import { rateLimit } from './common/middleware/rateLimit';
import {
  FailoverRateLimitStore,
  InMemoryRateLimitStore,
  RateLimitStore,
  RedisLike,
  RedisRateLimitStore,
} from './common/rateLimit/store';
import { AppConfig, loadConfig } from './config';
import { RATE_LIMIT_CONFIG, RateLimitConfig } from './config/rateLimits';
import { creditsRoutes } from './credits/routes';
import { Store } from './db/database';
import { marketplaceRoutes } from './marketplace/routes';

export interface AppDeps {
  store: Store;
  chain: ChainClient;
  config: AppConfig;
  rateLimitConfig: RateLimitConfig;
  /** Redis client for distributed rate limiting; in-memory fallback wraps it. */
  redis: RedisLike;
  /** Honor X-Forwarded-For for client IPs (behind a trusted proxy only). */
  trustProxy: boolean;
}

export interface App {
  app: Express;
  store: Store;
  chain: ChainClient;
  config: AppConfig;
  metrics: RateLimitMetrics;
  rateLimitStore: RateLimitStore;
}

function defaultRedis(): RedisLike | undefined {
  const url = process.env.REDIS_URL;
  if (!url) return undefined;
  // Lazy import so deployments without Redis never load the client.
  // eslint-disable-next-line @typescript-eslint/no-var-requires
  const IORedis = require('ioredis') as new (u: string) => RedisLike;
  return new IORedis(url);
}

export function createApp(deps: Partial<AppDeps> = {}): App {
  const config = deps.config ?? loadConfig();
  const store = deps.store ?? new Store(process.env.DATABASE_PATH ?? 'stellarkraal.db');
  const chain = deps.chain ?? new SimulatedChainClient(store, config.now);
  const rateLimitConfig = deps.rateLimitConfig ?? RATE_LIMIT_CONFIG;
  const metrics = new RateLimitMetrics();

  const memoryStore = new InMemoryRateLimitStore();
  const redis = deps.redis ?? defaultRedis();
  const rateLimitStore: RateLimitStore = redis
    ? new FailoverRateLimitStore(new RedisRateLimitStore(redis), memoryStore, (err) => {
        metrics.recordStoreFailover();
        // eslint-disable-next-line no-console
        console.warn('rate limit store failing over to memory:', err);
      })
    : memoryStore;

  const app = express();
  if (deps.trustProxy ?? process.env.TRUST_PROXY === '1') {
    app.set('trust proxy', true);
  }

  // Rate limiting runs before body parsing so floods are rejected cheaply.
  app.use(rateLimit({ store: rateLimitStore, config, limits: rateLimitConfig, metrics }));
  app.use(express.json());

  app.get('/health', (_req, res) => {
    res.json({ status: 'ok' });
  });

  app.get('/metrics', (_req, res) => {
    res.json({ rateLimit: metrics.snapshot() });
  });

  const routeDeps = { store, chain, config };
  app.use('/marketplace', marketplaceRoutes(routeDeps));
  app.use('/credits', creditsRoutes(routeDeps));

  return { app, store, chain, config, metrics, rateLimitStore };
}
