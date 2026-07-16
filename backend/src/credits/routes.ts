import { Router } from 'express';
import { IdempotencyDeps, idempotent } from '../middleware/idempotency';
import { CreditsService } from './service';

export function validateRetire(body: unknown): string | null {
  const b = body as Record<string, unknown> | null;
  if (!b || typeof b !== 'object') return 'request body must be a JSON object';
  if (typeof b.ownerId !== 'string' || b.ownerId.length === 0) return 'ownerId is required';
  if (typeof b.creditBatchId !== 'string' || b.creditBatchId.length === 0) {
    return 'creditBatchId is required';
  }
  if (typeof b.quantity !== 'number' || !Number.isInteger(b.quantity) || b.quantity <= 0) {
    return 'quantity must be a positive integer';
  }
  if (b.reason !== undefined && typeof b.reason !== 'string') return 'reason must be a string';
  return null;
}

export function creditsRoutes(deps: IdempotencyDeps): Router {
  const service = new CreditsService(deps.store, deps.chain, deps.config.now);
  const router = Router();

  router.post(
    '/retire',
    idempotent(deps, {
      endpoint: 'POST /credits/retire',
      validate: validateRetire,
      execute: (body, key) => service.retire(body, key),
      reconcile: (event) => service.reconcileRetirement(event),
    }),
  );

  return router;
}
