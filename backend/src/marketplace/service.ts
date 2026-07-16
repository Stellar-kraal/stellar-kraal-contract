import { createHash } from 'crypto';
import { ChainClient, ChainEvent } from '../chain/chainClient';
import { Store } from '../db/database';
import { BusinessError, ExecuteResult } from '../middleware/idempotency';

/**
 * Entity ids are derived deterministically from the Idempotency-Key so a
 * reconciled replay produces byte-identical responses and storage rows.
 */
export function deriveId(prefix: string, key: string): string {
  return `${prefix}_${createHash('sha256').update(`${prefix}:${key}`).digest('hex').slice(0, 16)}`;
}

export interface CreateListingBody {
  sellerId: string;
  creditBatchId: string;
  quantity: number;
  priceStroops: number;
}

export interface PurchaseBody {
  buyerId: string;
  listingId: string;
  quantity: number;
}

export class MarketplaceService {
  constructor(
    private readonly store: Store,
    private readonly chain: ChainClient,
    private readonly now: () => number,
  ) {}

  // ── Listing creation ───────────────────────────────────────────────────

  createListing(body: Record<string, unknown>, key: string): ExecuteResult {
    const { sellerId, creditBatchId, quantity, priceStroops } = body as unknown as CreateListingBody;
    const id = deriveId('lst', key);
    const createdAt = this.now();

    const payload = { id, sellerId, creditBatchId, quantity, priceStroops, createdAt };
    this.chain.submit(key, 'listing_created', payload);

    this.store.transaction(() => this.applyListing(payload));
    return { status: 201, body: this.listingBody(payload) };
  }

  reconcileListing(event: ChainEvent): ExecuteResult {
    const payload = event.payload as ReturnType<MarketplaceService['listingPayload']>;
    this.store.transaction(() => this.applyListing(payload));
    return { status: 200, body: this.listingBody(payload) };
  }

  private listingPayload(p: {
    id: string;
    sellerId: string;
    creditBatchId: string;
    quantity: number;
    priceStroops: number;
    createdAt: number;
  }) {
    return p;
  }

  private applyListing(p: ReturnType<MarketplaceService['listingPayload']>): void {
    const existing = this.store.getListing(p.id);
    this.store.upsertListing({
      id: p.id,
      seller_id: p.sellerId,
      credit_batch_id: p.creditBatchId,
      quantity_total: p.quantity,
      // Preserve fills already applied against a previously-reconciled row.
      quantity_remaining: existing ? existing.quantity_remaining : p.quantity,
      price_stroops: p.priceStroops,
      created_at: p.createdAt,
    });
  }

  private listingBody(p: ReturnType<MarketplaceService['listingPayload']>) {
    return {
      listingId: p.id,
      sellerId: p.sellerId,
      creditBatchId: p.creditBatchId,
      quantity: p.quantity,
      quantityRemaining: this.store.getListing(p.id)?.quantity_remaining ?? p.quantity,
      priceStroops: p.priceStroops,
      createdAt: p.createdAt,
    };
  }

  // ── Purchase ───────────────────────────────────────────────────────────

  purchase(body: Record<string, unknown>, key: string): ExecuteResult {
    const { buyerId, listingId, quantity } = body as unknown as PurchaseBody;

    const listing = this.store.getListing(listingId);
    if (!listing) {
      throw new BusinessError(404, 'listing not found');
    }
    if (quantity > listing.quantity_remaining) {
      throw new BusinessError(409, 'insufficient quantity remaining on listing');
    }

    const id = deriveId('pur', key);
    const createdAt = this.now();
    const totalPriceStroops = quantity * listing.price_stroops;

    const payload = { id, listingId, buyerId, quantity, totalPriceStroops, createdAt };
    this.chain.submit(key, 'purchase_settled', payload);

    this.store.transaction(() => this.applyPurchase(payload));
    return { status: 201, body: this.purchaseBody(payload) };
  }

  reconcilePurchase(event: ChainEvent): ExecuteResult {
    const payload = event.payload as ReturnType<MarketplaceService['purchasePayload']>;
    this.store.transaction(() => this.applyPurchase(payload));
    return { status: 200, body: this.purchaseBody(payload) };
  }

  private purchasePayload(p: {
    id: string;
    listingId: string;
    buyerId: string;
    quantity: number;
    totalPriceStroops: number;
    createdAt: number;
  }) {
    return p;
  }

  /** Idempotent: the listing is decremented only when the purchase row is first inserted. */
  private applyPurchase(p: ReturnType<MarketplaceService['purchasePayload']>): void {
    if (this.store.getPurchase(p.id)) {
      return;
    }
    this.store.insertPurchase({
      id: p.id,
      listing_id: p.listingId,
      buyer_id: p.buyerId,
      quantity: p.quantity,
      total_price_stroops: p.totalPriceStroops,
      created_at: p.createdAt,
    });
    this.store.decrementListing(p.listingId, p.quantity);
  }

  private purchaseBody(p: ReturnType<MarketplaceService['purchasePayload']>) {
    return {
      purchaseId: p.id,
      listingId: p.listingId,
      buyerId: p.buyerId,
      quantity: p.quantity,
      totalPriceStroops: p.totalPriceStroops,
      createdAt: p.createdAt,
    };
  }
}
