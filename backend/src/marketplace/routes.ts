import { Router } from 'express';
import { IdempotencyDeps, idempotent } from '../middleware/idempotency';
import { MarketplaceService } from './service';

function isPositiveInt(v: unknown): v is number {
  return typeof v === 'number' && Number.isInteger(v) && v > 0;
}
function isNonEmptyString(v: unknown): v is string {
  return typeof v === 'string' && v.length > 0;
}

export function validateCreateListing(body: unknown): string | null {
  const b = body as Record<string, unknown> | null;
  if (!b || typeof b !== 'object') return 'request body must be a JSON object';
  if (!isNonEmptyString(b.sellerId)) return 'sellerId is required';
  if (!isNonEmptyString(b.creditBatchId)) return 'creditBatchId is required';
  if (!isPositiveInt(b.quantity)) return 'quantity must be a positive integer';
  if (!isPositiveInt(b.priceStroops)) return 'priceStroops must be a positive integer';
  return null;
}

export function validatePurchase(body: unknown): string | null {
  const b = body as Record<string, unknown> | null;
  if (!b || typeof b !== 'object') return 'request body must be a JSON object';
  if (!isNonEmptyString(b.buyerId)) return 'buyerId is required';
  if (!isNonEmptyString(b.listingId)) return 'listingId is required';
  if (!isPositiveInt(b.quantity)) return 'quantity must be a positive integer';
  return null;
}

export function marketplaceRoutes(deps: IdempotencyDeps): Router {
  const service = new MarketplaceService(deps.store, deps.chain, deps.config.now);
  const router = Router();

  // Public browse endpoint (scrape target — rate limited per config).
  router.get('/listings', (_req, res) => {
    const listings = deps.store.listListings().map((l) => ({
      listingId: l.id,
      sellerId: l.seller_id,
      creditBatchId: l.credit_batch_id,
      quantity: l.quantity_total,
      quantityRemaining: l.quantity_remaining,
      priceStroops: l.price_stroops,
      createdAt: l.created_at,
    }));
    res.json({ listings });
  });

  // Public price query. Backed by the oracle feed in production; served
  // from the simulated feed here. Classified as an expensive on-chain read
  // for rate-limiting purposes.
  router.get('/prices', (_req, res) => {
    res.json({
      pair: 'CARBON/XLM',
      priceStroops: 5_000_000,
      source: 'simulated-oracle',
      updatedAt: deps.config.now(),
    });
  });

  router.post(
    '/listings',
    idempotent(deps, {
      endpoint: 'POST /marketplace/listings',
      validate: validateCreateListing,
      execute: (body, key) => service.createListing(body, key),
      reconcile: (event) => service.reconcileListing(event),
    }),
  );

  router.post(
    '/purchases',
    idempotent(deps, {
      endpoint: 'POST /marketplace/purchases',
      validate: validatePurchase,
      execute: (body, key) => service.purchase(body, key),
      reconcile: (event) => service.reconcilePurchase(event),
    }),
  );

  return router;
}
