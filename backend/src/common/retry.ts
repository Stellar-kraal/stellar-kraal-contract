/**
 * Retry a function with exponential backoff on transient failures.
 *
 * @param fn         Async function to attempt.
 * @param maxAttempts Maximum number of total attempts (default 4).
 * @param baseMs      Initial delay in milliseconds (default 200).
 * @param maxMs       Cap on delay in milliseconds (default 10 000).
 * @param jitter      Add ±20 % random jitter to each delay (default true).
 * @param isRetryable Optional predicate — returning false aborts immediately.
 */
export async function withRetry<T>(
  fn: () => Promise<T>,
  options: {
    maxAttempts?: number;
    baseMs?: number;
    maxMs?: number;
    jitter?: boolean;
    isRetryable?: (err: unknown) => boolean;
  } = {},
): Promise<T> {
  const {
    maxAttempts = 4,
    baseMs = 200,
    maxMs = 10_000,
    jitter = true,
    isRetryable = () => true,
  } = options;

  let lastError: unknown;
  for (let attempt = 1; attempt <= maxAttempts; attempt++) {
    try {
      return await fn();
    } catch (err) {
      lastError = err;
      if (attempt === maxAttempts || !isRetryable(err)) {
        throw err;
      }
      const exp = Math.min(baseMs * 2 ** (attempt - 1), maxMs);
      const delay = jitter ? exp * (0.8 + Math.random() * 0.4) : exp;
      await sleep(delay);
    }
  }
  throw lastError;
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * Compute the next-attempt epoch-second timestamp using exponential backoff.
 * Used by the webhook delivery scheduler.
 *
 * @param attemptCount Number of attempts already made (0-based on first call).
 * @param baseSeconds  Initial delay in seconds (default 30).
 * @param maxSeconds   Ceiling for delay in seconds (default 3600).
 * @param nowSeconds   Current epoch seconds.
 */
export function nextAttemptAt(
  attemptCount: number,
  nowSeconds: number,
  baseSeconds = 30,
  maxSeconds = 3_600,
): number {
  const delay = Math.min(baseSeconds * 2 ** attemptCount, maxSeconds);
  return nowSeconds + delay;
}
