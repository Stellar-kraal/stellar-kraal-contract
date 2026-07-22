export interface AppConfig {
  /** How long an idempotency record (and its cached response) stays valid. */
  idempotencyTtlSeconds: number;
  /** Clock in epoch seconds. Injectable so tests can advance time deterministically. */
  now: () => number;

  // ── Indexer ────────────────────────────────────────────────────────────
  /** Soroban RPC endpoint URL (used by the real HTTP RPC client). */
  rpcUrl: string;
  /** Contract ID to index events from. */
  contractId: string;
  /** How often the indexer polls for new events (ms). */
  indexerPollIntervalMs: number;
  /** Max RPC call attempts before giving up on a tick. */
  indexerMaxRpcAttempts: number;

  // ── Webhook delivery ───────────────────────────────────────────────────
  /** Max delivery attempts before a delivery is marked permanently failed. */
  webhookMaxAttempts: number;
  /** Base backoff in seconds for webhook retry. */
  webhookBaseBackoffSeconds: number;
  /** Max backoff cap in seconds for webhook retry. */
  webhookMaxBackoffSeconds: number;
  /** How often the delivery drain loop runs (ms). */
  webhookPollIntervalMs: number;
}

export function loadConfig(overrides: Partial<AppConfig> = {}): AppConfig {
  return {
    idempotencyTtlSeconds: Number(process.env.IDEMPOTENCY_TTL_SECONDS ?? 86_400),
    now: () => Math.floor(Date.now() / 1000),
    rpcUrl: process.env.RPC_URL ?? 'https://soroban-testnet.stellar.org',
    contractId: process.env.CONTRACT_ID ?? '',
    indexerPollIntervalMs: Number(process.env.INDEXER_POLL_INTERVAL_MS ?? 5_000),
    indexerMaxRpcAttempts: Number(process.env.INDEXER_MAX_RPC_ATTEMPTS ?? 4),
    webhookMaxAttempts: Number(process.env.WEBHOOK_MAX_ATTEMPTS ?? 5),
    webhookBaseBackoffSeconds: Number(process.env.WEBHOOK_BASE_BACKOFF_SECONDS ?? 30),
    webhookMaxBackoffSeconds: Number(process.env.WEBHOOK_MAX_BACKOFF_SECONDS ?? 3_600),
    webhookPollIntervalMs: Number(process.env.WEBHOOK_POLL_INTERVAL_MS ?? 5_000),
    ...overrides,
  };
}