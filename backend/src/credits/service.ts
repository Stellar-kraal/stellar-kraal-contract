import { ChainClient, ChainEvent } from '../chain/chainClient';
import { Store } from '../db/database';
import { ExecuteResult } from '../middleware/idempotency';
import { deriveId } from '../marketplace/service';

export interface RetireBody {
  ownerId: string;
  creditBatchId: string;
  quantity: number;
  reason?: string;
}

export class CreditsService {
  constructor(
    private readonly store: Store,
    private readonly chain: ChainClient,
    private readonly now: () => number,
  ) {}

  retire(body: Record<string, unknown>, key: string): ExecuteResult {
    const { ownerId, creditBatchId, quantity, reason } = body as unknown as RetireBody;
    const id = deriveId('ret', key);
    const createdAt = this.now();

    const payload = { id, ownerId, creditBatchId, quantity, reason: reason ?? null, createdAt };
    this.chain.submit(key, 'credits_retired', payload);

    this.store.transaction(() => this.applyRetirement(payload));
    return { status: 201, body: this.retirementBody(payload) };
  }

  reconcileRetirement(event: ChainEvent): ExecuteResult {
    const payload = event.payload as ReturnType<CreditsService['retirementPayload']>;
    this.store.transaction(() => this.applyRetirement(payload));
    return { status: 200, body: this.retirementBody(payload) };
  }

  private retirementPayload(p: {
    id: string;
    ownerId: string;
    creditBatchId: string;
    quantity: number;
    reason: string | null;
    createdAt: number;
  }) {
    return p;
  }

  private applyRetirement(p: ReturnType<CreditsService['retirementPayload']>): void {
    this.store.insertRetirementIfAbsent({
      id: p.id,
      owner_id: p.ownerId,
      credit_batch_id: p.creditBatchId,
      quantity: p.quantity,
      reason: p.reason,
      created_at: p.createdAt,
    });
  }

  private retirementBody(p: ReturnType<CreditsService['retirementPayload']>) {
    return {
      retirementId: p.id,
      ownerId: p.ownerId,
      creditBatchId: p.creditBatchId,
      quantity: p.quantity,
      reason: p.reason,
      createdAt: p.createdAt,
    };
  }
}
