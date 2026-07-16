import { createHash } from 'crypto';
import { Request, RequestHandler, Response } from 'express';
import { ChainClient, ChainEvent } from '../chain/chainClient';
import { AppConfig } from '../config';
import { Store } from '../db/database';

/** Thrown by services for expected business rejections (never cached). */
export class BusinessError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

export interface ExecuteResult {
  status: number;
  body: Record<string, unknown>;
}

export interface IdempotentSpec {
  /** Stable endpoint identifier stored on the idempotency record. */
  endpoint: string;
  /** Request-shape validation; return an error message to reject with 400. */
  validate: (body: unknown) => string | null;
  /**
   * Perform the operation: submit on-chain (dedup-keyed by the
   * Idempotency-Key), then persist domain state via `Store.transaction()`.
   */
  execute: (body: Record<string, unknown>, key: string) => ExecuteResult;
  /**
   * Rebuild domain state and the response from an on-chain event when the
   * chain succeeded but the database write was lost. Must be idempotent.
   */
  reconcile: (event: ChainEvent) => ExecuteResult;
}

export interface IdempotencyDeps {
  store: Store;
  chain: ChainClient;
  config: AppConfig;
}

const KEY_PATTERN = /^[\x21-\x7e]{1,255}$/; // visible ASCII, 1–255 chars

/** JSON stringify with recursively sorted object keys, for stable fingerprints. */
export function stableStringify(value: unknown): string {
  if (Array.isArray(value)) {
    return `[${value.map(stableStringify).join(',')}]`;
  }
  if (value !== null && typeof value === 'object') {
    const entries = Object.entries(value as Record<string, unknown>)
      .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
      .map(([k, v]) => `${JSON.stringify(k)}:${stableStringify(v)}`);
    return `{${entries.join(',')}}`;
  }
  return JSON.stringify(value);
}

function fingerprintOf(req: Request): string {
  return createHash('sha256')
    .update(`${req.method} ${req.baseUrl}${req.path}\n${stableStringify(req.body ?? {})}`)
    .digest('hex');
}

function replay(res: Response, body: string): Response {
  // Per spec, replayed duplicates return 200 with the original response body.
  return res.set('Idempotent-Replayed', 'true').status(200).json(JSON.parse(body));
}

/**
 * Wrap a critical financial endpoint with the idempotency protocol:
 *
 * 1. `Idempotency-Key` header is required (400 if missing or malformed).
 * 2. A completed record within the TTL window replays the cached response
 *    with `Idempotent-Replayed: true` (409 if the same key is still
 *    in flight, 422 if the key is reused with a different payload).
 * 3. If no usable record exists but an on-chain event carrying this key
 *    does (partial failure: chain succeeded, DB write lost), the event is
 *    replayed to reconcile domain state instead of re-executing.
 * 4. Otherwise the operation executes; the response is cached atomically
 *    with the domain write so a lost write is always reconcilable via (3).
 */
export function idempotent(deps: IdempotencyDeps, spec: IdempotentSpec): RequestHandler {
  const { store, chain, config } = deps;

  return (req, res) => {
    const key = req.header('Idempotency-Key');
    if (!key || !KEY_PATTERN.test(key)) {
      res.status(400).json({
        error: 'missing or invalid Idempotency-Key header (1-255 visible ASCII characters)',
      });
      return;
    }

    const now = config.now();
    const ttl = config.idempotencyTtlSeconds;
    const fingerprint = fingerprintOf(req);

    let record = store.getRecord(key);
    if (record && record.expires_at <= now) {
      store.deleteRecord(key);
      record = undefined;
    }

    if (record) {
      if (record.fingerprint !== fingerprint) {
        res.status(422).json({
          error: 'Idempotency-Key was already used with a different request payload',
        });
        return;
      }
      if (record.status === 'completed' && record.response_body !== null) {
        replay(res, record.response_body);
        return;
      }
      // Same key, same payload, but the first attempt never completed.
      // If its on-chain submission made it through, fall through to
      // reconciliation; otherwise report the in-flight conflict.
      const pending = chain.findEvent(key);
      if (!pending) {
        res.status(409).json({
          error: 'a request with this Idempotency-Key is already in progress',
        });
        return;
      }
    }

    // Partial-failure reconciliation: the chain accepted the transaction
    // but no completed database record exists. Replay the on-chain event.
    const event = chain.findEvent(key);
    if (event && now - event.createdAt < ttl) {
      const result = spec.reconcile(event);
      store.completeRecord(
        key,
        fingerprint,
        spec.endpoint,
        result.status,
        JSON.stringify(result.body),
        now,
        now + ttl,
      );
      res
        .set('Idempotent-Replayed', 'true')
        .set('Idempotency-Reconciled', 'true')
        .status(200)
        .json(result.body);
      return;
    }

    const validationError = spec.validate(req.body);
    if (validationError) {
      res.status(400).json({ error: validationError });
      return;
    }

    if (!record) {
      store.insertInProgress(key, fingerprint, spec.endpoint, now, now + ttl);
    }

    let result: ExecuteResult;
    try {
      result = spec.execute(req.body as Record<string, unknown>, key);
    } catch (err) {
      store.deleteRecord(key);
      if (err instanceof BusinessError) {
        res.status(err.status).json({ error: err.message });
        return;
      }
      res.status(500).json({
        error: 'internal error while executing the request; retry with the same Idempotency-Key',
      });
      return;
    }

    store.completeRecord(
      key,
      fingerprint,
      spec.endpoint,
      result.status,
      JSON.stringify(result.body),
      now,
      now + ttl,
    );
    res.status(result.status).json(result.body);
  };
}
