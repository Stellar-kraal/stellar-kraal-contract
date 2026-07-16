/**
 * Cursor-based on-chain event indexer.
 *
 * For each (contractId, eventType) pair it:
 *   1. Loads the last processed ledger from the persistent `ledger_cursors` table.
 *   2. Fetches all pages from the Soroban RPC starting at that cursor.
 *   3. Deduplicates by (contract_id, topic, transaction_hash) before storing.
 *   4. Advances the cursor atomically with each batch write.
 *   5. Enqueues a webhook delivery for every newly indexed event.
 *
 * The indexer is deliberately stateless between ticks — all state lives in
 * SQLite so it survives restarts without replaying events.
 */

import { withRetry } from '../common/retry';
import { Store } from '../db/database';
import { paginateEvents, SorobanRpcClient } from '../chain/rpcClient';

export interface IndexerTarget {
  contractId: string;
  eventType: string;
}

export interface IndexerStatus {
  running: boolean;
  lastProcessedLedger: Record<string, number>;
  lastTickAt: number | null;
  lastError: string | null;
}

export class EventIndexer {
  private running = false;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private lastTickAt: number | null = null;
  private lastError: string | null = null;

  constructor(
    private readonly store: Store,
    private readonly rpc: SorobanRpcClient,
    private readonly targets: IndexerTarget[],
    private readonly options: {
      pollIntervalMs?: number;
      pageSize?: number;
      maxRpcAttempts?: number;
      now?: () => number;
    } = {},
  ) {}

  private get now(): number {
    return this.options.now ? this.options.now() : Math.floor(Date.now() / 1000);
  }

  start(): void {
    if (this.running) return;
    this.running = true;
    this.scheduleTick();
  }

  stop(): void {
    this.running = false;
    if (this.timer) {
      clearTimeout(this.timer);
      this.timer = null;
    }
  }

  status(): IndexerStatus {
    const lastProcessedLedger: Record<string, number> = {};
    for (const t of this.targets) {
      const cursor = this.store.getCursor(t.eventType);
      lastProcessedLedger[t.eventType] = cursor?.last_ledger ?? 0;
    }
    return {
      running: this.running,
      lastProcessedLedger,
      lastTickAt: this.lastTickAt,
      lastError: this.lastError,
    };
  }

  /** Run one indexing tick for all targets. Exposed for testing. */
  async tick(): Promise<void> {
    for (const target of this.targets) {
      await this.indexTarget(target);
    }
    this.lastTickAt = this.now;
  }

  private scheduleTick(): void {
    if (!this.running) return;
    const interval = this.options.pollIntervalMs ?? 5_000;
    this.timer = setTimeout(() => {
      void (async () => {
        try {
          await this.tick();
          this.lastError = null;
        } catch (err) {
          this.lastError = err instanceof Error ? err.message : String(err);
          // eslint-disable-next-line no-console
          console.error('[indexer] tick error:', err);
        }
        this.scheduleTick();
      })();
    }, interval);
  }

  private async indexTarget(target: IndexerTarget): Promise<void> {
    const cursor = this.store.getCursor(target.eventType);
    const fromLedger = cursor ? cursor.last_ledger + 1 : 0;

    let highWaterLedger = cursor?.last_ledger ?? 0;

    const generator = paginateEvents(
      {
        getEvents: (cid, et, sl, limit) =>
          withRetry(() => this.rpc.getEvents(cid, et, sl, limit), {
            maxAttempts: this.options.maxRpcAttempts ?? 4,
            isRetryable: isTransient,
          }),
        getLatestLedger: () => this.rpc.getLatestLedger(),
      },
      target.contractId,
      target.eventType,
      fromLedger,
      this.options.pageSize ?? 100,
    );

    for await (const batch of generator) {
      for (const event of batch) {
        const indexedAt = this.now;

        // Deduplication: insertIndexedEventIfAbsent returns undefined on duplicate
        const newId = this.store.insertIndexedEventIfAbsent({
          contract_id: event.contractId,
          topic: event.topic,
          transaction_hash: event.transactionHash,
          ledger: event.ledger,
          payload: JSON.stringify(event.payload),
          indexed_at: indexedAt,
        });

        if (newId !== undefined) {
          // Enqueue deliveries for all active webhooks subscribed to this event type
          const webhooks = this.store.listWebhooksByEventType(target.eventType);
          for (const webhook of webhooks) {
            this.store.insertWebhookDelivery({
              webhook_id: webhook.id,
              event_id: newId,
              status: 'pending',
              attempt_count: 0,
              last_attempt_at: null,
              next_attempt_at: indexedAt,
              response_status: null,
              error_message: null,
              created_at: indexedAt,
            });
          }
        }

        if (event.ledger > highWaterLedger) {
          highWaterLedger = event.ledger;
        }
      }

      // Advance cursor after each page so a crash mid-batch only replays one page
      if (highWaterLedger > (cursor?.last_ledger ?? 0)) {
        this.store.upsertCursor(target.eventType, highWaterLedger, this.now);
      }
    }
  }
}

function isTransient(err: unknown): boolean {
  if (err instanceof Error) {
    const msg = err.message.toLowerCase();
    // Network errors, timeouts, 429/503 are retryable; 400/404 are not
    return (
      msg.includes('timeout') ||
      msg.includes('econnrefused') ||
      msg.includes('econnreset') ||
      msg.includes('network') ||
      msg.includes('503') ||
      msg.includes('429')
    );
  }
  return true;
}
