import { Store } from '../db/database';

export interface ChainEvent {
  seq: number;
  dedupId: string;
  eventType: string;
  payload: Record<string, unknown>;
  createdAt: number;
}

/**
 * Abstraction over the on-chain layer. Every state-changing submission
 * carries a `dedupId` (the request's Idempotency-Key) so that emitted
 * events can later be found and replayed for reconciliation.
 */
export interface ChainClient {
  submit(dedupId: string, eventType: string, payload: Record<string, unknown>): ChainEvent;
  /** Most recent event submitted with this dedup id, if any. */
  findEvent(dedupId: string): ChainEvent | undefined;
  eventCount(dedupId: string): number;
}

interface ChainEventRow {
  seq: number;
  dedup_id: string;
  event_type: string;
  payload: string;
  created_at: number;
}

/**
 * Deterministic in-process chain used for local development and tests.
 * It appends events to its own table and is deliberately NOT covered by
 * `Store.transaction()` fault injection: a simulated on-chain submission
 * can succeed even when the subsequent backend database write fails,
 * which is exactly the partial-failure mode the idempotency layer must
 * reconcile.
 */
export class SimulatedChainClient implements ChainClient {
  constructor(
    private readonly store: Store,
    private readonly now: () => number,
  ) {}

  submit(dedupId: string, eventType: string, payload: Record<string, unknown>): ChainEvent {
    const createdAt = this.now();
    const info = this.store.db
      .prepare(
        'INSERT INTO chain_events (dedup_id, event_type, payload, created_at) VALUES (?, ?, ?, ?)',
      )
      .run(dedupId, eventType, JSON.stringify(payload), createdAt);
    return { seq: Number(info.lastInsertRowid), dedupId, eventType, payload, createdAt };
  }

  findEvent(dedupId: string): ChainEvent | undefined {
    const row = this.store.db
      .prepare('SELECT * FROM chain_events WHERE dedup_id = ? ORDER BY seq DESC LIMIT 1')
      .get(dedupId) as ChainEventRow | undefined;
    if (!row) return undefined;
    return {
      seq: row.seq,
      dedupId: row.dedup_id,
      eventType: row.event_type,
      payload: JSON.parse(row.payload) as Record<string, unknown>,
      createdAt: row.created_at,
    };
  }

  eventCount(dedupId: string): number {
    const row = this.store.db
      .prepare('SELECT COUNT(*) AS n FROM chain_events WHERE dedup_id = ?')
      .get(dedupId) as { n: number };
    return row.n;
  }
}
