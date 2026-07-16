export interface AppConfig {
  /** How long an idempotency record (and its cached response) stays valid. */
  idempotencyTtlSeconds: number;
  /** Clock in epoch seconds. Injectable so tests can advance time deterministically. */
  now: () => number;
}

export function loadConfig(overrides: Partial<AppConfig> = {}): AppConfig {
  return {
    idempotencyTtlSeconds: Number(process.env.IDEMPOTENCY_TTL_SECONDS ?? 86_400),
    now: () => Math.floor(Date.now() / 1000),
    ...overrides,
  };
}
