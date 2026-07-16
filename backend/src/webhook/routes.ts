/**
 * Webhook registration API.
 *
 * POST   /webhooks          – register a new webhook
 * GET    /webhooks          – list all webhooks
 * GET    /webhooks/:id      – get one webhook (secret omitted)
 * PATCH  /webhooks/:id      – update url / active flag
 * POST   /webhooks/:id/rotate-secret – rotate the signing secret
 * DELETE /webhooks/:id      – deactivate (soft delete)
 */

import { randomBytes, randomUUID } from 'crypto';
import { Router } from 'express';
import { Store } from '../db/database';
import { AppConfig } from '../config';

export interface WebhookDeps {
  store: Store;
  config: AppConfig;
}

function generateSecret(): string {
  return randomBytes(32).toString('hex');
}

function stripSecret<T extends { secret: string }>(row: T): Omit<T, 'secret'> {
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  const { secret: _s, ...rest } = row;
  return rest;
}

function toResponse(row: import('../db/database').WebhookRegistrationRow) {
  return {
    id: row.id,
    url: row.url,
    eventType: row.event_type,
    active: row.active,
    createdAt: row.created_at,
    updatedAt: row.updated_at,
  };
}

export function webhookRoutes(deps: WebhookDeps): Router {
  const { store, config } = deps;
  const router = Router();

  // ── POST /webhooks ─────────────────────────────────────────────────────
  router.post('/', (req, res) => {
    const body = req.body as Record<string, unknown>;
    if (typeof body.url !== 'string' || !body.url) {
      res.status(400).json({ error: 'url is required' });
      return;
    }
    if (typeof body.eventType !== 'string' || !body.eventType) {
      res.status(400).json({ error: 'eventType is required' });
      return;
    }
    try {
      new URL(body.url);
    } catch {
      res.status(400).json({ error: 'url must be a valid URL' });
      return;
    }

    const now = config.now();
    const id = randomUUID();
    const secret = generateSecret();

    store.insertWebhook({
      id,
      url: body.url,
      secret,
      event_type: body.eventType,
      active: 1,
      created_at: now,
      updated_at: now,
    });

    const row = store.getWebhook(id)!;
    res.status(201).json({
      ...toResponse(row),
      // Return the secret once on creation so callers can store it
      secret,
    });
  });

  // ── GET /webhooks ──────────────────────────────────────────────────────
  router.get('/', (_req, res) => {
    const rows = store.listAllWebhooks().map(toResponse);
    res.json({ webhooks: rows });
  });

  // ── GET /webhooks/:id ──────────────────────────────────────────────────
  router.get('/:id', (req, res) => {
    const row = store.getWebhook(req.params.id);
    if (!row) { res.status(404).json({ error: 'webhook not found' }); return; }
    res.json(toResponse(row));
  });

  // ── PATCH /webhooks/:id ────────────────────────────────────────────────
  router.patch('/:id', (req, res) => {
    const row = store.getWebhook(req.params.id);
    if (!row) { res.status(404).json({ error: 'webhook not found' }); return; }

    const body = req.body as Record<string, unknown>;
    const fields: Parameters<typeof store.updateWebhook>[1] = {};
    if (typeof body.url === 'string') {
      try { new URL(body.url); } catch { res.status(400).json({ error: 'url must be a valid URL' }); return; }
      fields.url = body.url;
    }
    if (typeof body.active === 'boolean') {
      fields.active = body.active ? 1 : 0;
    }

    store.updateWebhook(req.params.id, fields, config.now());
    res.json(toResponse(store.getWebhook(req.params.id)!));
  });

  // ── POST /webhooks/:id/rotate-secret ──────────────────────────────────
  router.post('/:id/rotate-secret', (req, res) => {
    const row = store.getWebhook(req.params.id);
    if (!row) { res.status(404).json({ error: 'webhook not found' }); return; }

    const newSecret = generateSecret();
    store.updateWebhook(req.params.id, { secret: newSecret }, config.now());
    res.json({ id: req.params.id, secret: newSecret });
  });

  // ── DELETE /webhooks/:id ───────────────────────────────────────────────
  router.delete('/:id', (req, res) => {
    const row = store.getWebhook(req.params.id);
    if (!row) { res.status(404).json({ error: 'webhook not found' }); return; }

    store.updateWebhook(req.params.id, { active: 0 }, config.now());
    res.status(204).send();
  });

  return router;
}
