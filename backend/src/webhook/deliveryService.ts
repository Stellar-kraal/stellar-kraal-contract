/**
 * Webhook delivery service.
 *
 * Drains the `webhook_deliveries` queue:
 *   - Signs each payload with HMAC-SHA256 → X-StellarKraal-Signature header.
 *   - Retries failed deliveries with exponential backoff.
 *   - Marks deliveries as 'delivered' on 2xx, 'failed' otherwise.
 *   - Stops retrying after maxAttempts.
 *
 * At-least-once semantics: a delivery is retried until it succeeds or
 * exceeds maxAttempts.  Duplicate suppression is on the indexer side
 * (deduplication by contract_id/topic/transaction_hash).
 */

import { createHmac } from 'crypto';
import { Store } from '../db/database';
import { nextAttemptAt } from '../common/retry';

export interface WebhookDeliveryOptions {
  maxAttempts?: number;
  baseBackoffSeconds?: number;
  maxBackoffSeconds?: number;
  pollIntervalMs?: number;
  now?: () => number;
  /** Injectable HTTP poster for tests. Default: real fetch. */
  poster?: HttpPoster;
}

export interface HttpPoster {
  post(
    url: string,
    body: string,
    headers: Record<string, string>,
  ): Promise<{ status: number; ok: boolean }>;
}

export interface WebhookQueueStatus {
  pendingCount: number;
}

/**
 * Sign a payload string with HMAC-SHA256, returning the hex digest prefixed
 * by "sha256=".  Matches the de-facto GitHub/Stripe convention.
 */
export function signPayload(secret: string, payload: string): string {
  return 'sha256=' + createHmac('sha256', secret).update(payload, 'utf8').digest('hex');
}

export class WebhookDeliveryService {
  private running = false;
  private timer: ReturnType<typeof setTimeout> | null = null;

  constructor(
    private readonly store: Store,
    private readonly options: WebhookDeliveryOptions = {},
  ) {}

  private get now(): number {
    return this.options.now ? this.options.now() : Math.floor(Date.now() / 1000);
  }

  private get maxAttempts(): number {
    return this.options.maxAttempts ?? 5;
  }

  private get poster(): HttpPoster {
    return this.options.poster ?? fetchPoster;
  }

  start(): void {
    if (this.running) return;
    this.running = true;
    this.scheduleDrain();
  }

  stop(): void {
    this.running = false;
    if (this.timer) {
      clearTimeout(this.timer);
      this.timer = null;
    }
  }

  queueStatus(): WebhookQueueStatus {
    return { pendingCount: this.store.countPendingDeliveries() };
  }

  /** Process all due deliveries. Exposed for testing. */
  async drain(): Promise<void> {
    const now = this.now;
    const due = this.store.getPendingDeliveries(now);

    for (const delivery of due) {
      const webhook = this.store.getWebhook(delivery.webhook_id);
      if (!webhook || !webhook.active) {
        // Webhook was deleted/disabled — cancel delivery
        this.store.updateDelivery(delivery.id, { status: 'failed', error_message: 'webhook inactive' });
        continue;
      }

      const event = this.store.getIndexedEvent(delivery.event_id);
      if (!event) {
        this.store.updateDelivery(delivery.id, { status: 'failed', error_message: 'event not found' });
        continue;
      }

      const payloadObj = {
        webhookId: webhook.id,
        eventType: webhook.event_type,
        contractId: event.contract_id,
        topic: event.topic,
        transactionHash: event.transaction_hash,
        ledger: event.ledger,
        data: JSON.parse(event.payload) as unknown,
        indexedAt: event.indexed_at,
      };
      const payloadStr = JSON.stringify(payloadObj);
      const signature = signPayload(webhook.secret, payloadStr);

      const attemptCount = delivery.attempt_count + 1;
      const attemptedAt = now;

      try {
        const result = await this.poster.post(webhook.url, payloadStr, {
          'Content-Type': 'application/json',
          'X-StellarKraal-Signature': signature,
        });

        if (result.ok) {
          this.store.updateDelivery(delivery.id, {
            status: 'delivered',
            attempt_count: attemptCount,
            last_attempt_at: attemptedAt,
            response_status: result.status,
            error_message: null,
          });
        } else {
          this.handleFailure(delivery.id, attemptCount, attemptedAt, result.status, `HTTP ${result.status}`);
        }
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        this.handleFailure(delivery.id, attemptCount, attemptedAt, null, msg);
      }
    }
  }

  private handleFailure(
    deliveryId: number,
    attemptCount: number,
    attemptedAt: number,
    responseStatus: number | null,
    errorMessage: string,
  ): void {
    const failed = attemptCount >= this.maxAttempts;
    const nextAt = failed
      ? attemptedAt // won't be retried — value doesn't matter
      : nextAttemptAt(
          attemptCount,
          attemptedAt,
          this.options.baseBackoffSeconds ?? 30,
          this.options.maxBackoffSeconds ?? 3_600,
        );

    this.store.updateDelivery(deliveryId, {
      status: failed ? 'failed' : 'pending',
      attempt_count: attemptCount,
      last_attempt_at: attemptedAt,
      next_attempt_at: nextAt,
      response_status: responseStatus ?? undefined,
      error_message: errorMessage,
    });
  }

  private scheduleDrain(): void {
    if (!this.running) return;
    const interval = this.options.pollIntervalMs ?? 5_000;
    this.timer = setTimeout(() => {
      void this.drain()
        .catch((err) => {
          // eslint-disable-next-line no-console
          console.error('[webhook] drain error:', err);
        })
        .finally(() => this.scheduleDrain());
    }, interval);
  }
}

// ── Real HTTP poster using Node built-in fetch ──────────────────────────────

const fetchPoster: HttpPoster = {
  async post(url, body, headers) {
    const res = await fetch(url, {
      method: 'POST',
      body,
      headers,
      signal: AbortSignal.timeout(10_000),
    });
    return { status: res.status, ok: res.ok };
  },
};
