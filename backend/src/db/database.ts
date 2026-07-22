import Database from 'better-sqlite3';

export interface IdempotencyRecord {
  key: string;
  fingerprint: string;
  endpoint: string;
  status: 'in_progress' | 'completed';
  response_status: number | null;
  response_body: string | null;
  created_at: number;
  expires_at: number;
}

export interface ListingRow {
  id: string;
  seller_id: string;
  credit_batch_id: string;
  quantity_total: number;
  quantity_remaining: number;
  price_stroops: number;
  created_at: number;
}

export interface PurchaseRow {
  id: string;
  listing_id: string;
  buyer_id: string;
  quantity: number;
  total_price_stroops: number;
  created_at: number;
}

export interface RetirementRow {
  id: string;
  owner_id: string;
  credit_batch_id: string;
  quantity: number;
  reason: string | null;
  created_at: number;
}

// ── Indexer / Webhook types ────────────────────────────────────────────────

export interface LedgerCursorRow {
  event_type: string;
  last_ledger: number;
  updated_at: number;
}

export interface IndexedEventRow {
  id: number;
  contract_id: string;
  topic: string;
  transaction_hash: string;
  ledger: number;
  payload: string;
  indexed_at: number;
}

export interface WebhookRegistrationRow {
  id: string;
  url: string;
  secret: string;
  event_type: string;
  active: number;
  created_at: number;
  updated_at: number;
}

export interface WebhookDeliveryRow {
  id: number;
  webhook_id: string;
  event_id: number;
  status: 'pending' | 'delivered' | 'failed';
  attempt_count: number;
  last_attempt_at: number | null;
  next_attempt_at: number;
  response_status: number | null;
  error_message: string | null;
  created_at: number;
}

const SCHEMA = `
CREATE TABLE IF NOT EXISTS idempotency_records (
  key             TEXT PRIMARY KEY,
  fingerprint     TEXT NOT NULL,
  endpoint        TEXT NOT NULL,
  status          TEXT NOT NULL CHECK (status IN ('in_progress', 'completed')),
  response_status INTEGER,
  response_body   TEXT,
  created_at      INTEGER NOT NULL,
  expires_at      INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS listings (
  id                 TEXT PRIMARY KEY,
  seller_id          TEXT NOT NULL,
  credit_batch_id    TEXT NOT NULL,
  quantity_total     INTEGER NOT NULL,
  quantity_remaining INTEGER NOT NULL,
  price_stroops      INTEGER NOT NULL,
  created_at         INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS purchases (
  id                  TEXT PRIMARY KEY,
  listing_id          TEXT NOT NULL REFERENCES listings(id),
  buyer_id            TEXT NOT NULL,
  quantity            INTEGER NOT NULL,
  total_price_stroops INTEGER NOT NULL,
  created_at          INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS retirements (
  id              TEXT PRIMARY KEY,
  owner_id        TEXT NOT NULL,
  credit_batch_id TEXT NOT NULL,
  quantity        INTEGER NOT NULL,
  reason          TEXT,
  created_at      INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS chain_events (
  seq        INTEGER PRIMARY KEY AUTOINCREMENT,
  dedup_id   TEXT NOT NULL,
  event_type TEXT NOT NULL,
  payload    TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_chain_events_dedup ON chain_events(dedup_id);

CREATE TABLE IF NOT EXISTS ledger_cursors (
  event_type   TEXT PRIMARY KEY,
  last_ledger  INTEGER NOT NULL DEFAULT 0,
  updated_at   INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS indexed_events (
  id               INTEGER PRIMARY KEY AUTOINCREMENT,
  contract_id      TEXT NOT NULL,
  topic            TEXT NOT NULL,
  transaction_hash TEXT NOT NULL,
  ledger           INTEGER NOT NULL,
  payload          TEXT NOT NULL,
  indexed_at       INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_indexed_events_dedup
  ON indexed_events(contract_id, topic, transaction_hash);
CREATE INDEX IF NOT EXISTS idx_indexed_events_ledger
  ON indexed_events(ledger);

CREATE TABLE IF NOT EXISTS webhook_registrations (
  id         TEXT PRIMARY KEY,
  url        TEXT NOT NULL,
  secret     TEXT NOT NULL,
  event_type TEXT NOT NULL,
  active     INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_webhook_registrations_event
  ON webhook_registrations(event_type);

CREATE TABLE IF NOT EXISTS webhook_deliveries (
  id              INTEGER PRIMARY KEY AUTOINCREMENT,
  webhook_id      TEXT NOT NULL REFERENCES webhook_registrations(id),
  event_id        INTEGER NOT NULL REFERENCES indexed_events(id),
  status          TEXT NOT NULL CHECK (status IN ('pending','delivered','failed')),
  attempt_count   INTEGER NOT NULL DEFAULT 0,
  last_attempt_at INTEGER,
  next_attempt_at INTEGER NOT NULL DEFAULT 0,
  response_status INTEGER,
  error_message   TEXT,
  created_at      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_webhook_deliveries_pending
  ON webhook_deliveries(status, next_attempt_at)
  WHERE status IN ('pending','failed');
`;

/**
 * Thin data-access layer over SQLite.
 *
 * All domain writes performed by request handlers must go through
 * `transaction()`, which supports fault injection (`failNextTransaction`)
 * so tests can simulate the partial-failure case where the on-chain
 * transaction succeeded but the backend database write did not.
 */
export class Store {
  readonly db: Database.Database;
  private failNext = false;

  constructor(path = ':memory:') {
    this.db = new Database(path);
    this.db.pragma('journal_mode = WAL');
    this.db.exec(SCHEMA);
  }

  /** Test hook: make the next `transaction()` call throw before writing. */
  failNextTransaction(): void {
    this.failNext = true;
  }

  transaction<T>(fn: () => T): T {
    if (this.failNext) {
      this.failNext = false;
      throw new Error('injected database failure');
    }
    return this.db.transaction(fn)();
  }

  // ── Idempotency records ────────────────────────────────────────────────

  getRecord(key: string): IdempotencyRecord | undefined {
    return this.db
      .prepare('SELECT * FROM idempotency_records WHERE key = ?')
      .get(key) as IdempotencyRecord | undefined;
  }

  insertInProgress(
    key: string,
    fingerprint: string,
    endpoint: string,
    now: number,
    expiresAt: number,
  ): void {
    this.db
      .prepare(
        `INSERT INTO idempotency_records
           (key, fingerprint, endpoint, status, created_at, expires_at)
         VALUES (?, ?, ?, 'in_progress', ?, ?)`,
      )
      .run(key, fingerprint, endpoint, now, expiresAt);
  }

  completeRecord(
    key: string,
    fingerprint: string,
    endpoint: string,
    responseStatus: number,
    responseBody: string,
    now: number,
    expiresAt: number,
  ): void {
    this.db
      .prepare(
        `INSERT INTO idempotency_records
           (key, fingerprint, endpoint, status, response_status, response_body, created_at, expires_at)
         VALUES (?, ?, ?, 'completed', ?, ?, ?, ?)
         ON CONFLICT(key) DO UPDATE SET
           status = 'completed',
           response_status = excluded.response_status,
           response_body = excluded.response_body`,
      )
      .run(key, fingerprint, endpoint, responseStatus, responseBody, now, expiresAt);
  }

  deleteRecord(key: string): void {
    this.db.prepare('DELETE FROM idempotency_records WHERE key = ?').run(key);
  }

  // ── Marketplace domain ─────────────────────────────────────────────────

  upsertListing(row: ListingRow): void {
    this.db
      .prepare(
        `INSERT OR REPLACE INTO listings
           (id, seller_id, credit_batch_id, quantity_total, quantity_remaining, price_stroops, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        row.id,
        row.seller_id,
        row.credit_batch_id,
        row.quantity_total,
        row.quantity_remaining,
        row.price_stroops,
        row.created_at,
      );
  }

  getListing(id: string): ListingRow | undefined {
    return this.db.prepare('SELECT * FROM listings WHERE id = ?').get(id) as
      | ListingRow
      | undefined;
  }

  listListings(limit = 100): ListingRow[] {
    return this.db
      .prepare('SELECT * FROM listings ORDER BY created_at DESC, id LIMIT ?')
      .all(limit) as ListingRow[];
  }

  insertPurchase(row: PurchaseRow): void {
    this.db
      .prepare(
        `INSERT INTO purchases
           (id, listing_id, buyer_id, quantity, total_price_stroops, created_at)
         VALUES (?, ?, ?, ?, ?, ?)`,
      )
      .run(row.id, row.listing_id, row.buyer_id, row.quantity, row.total_price_stroops, row.created_at);
  }

  getPurchase(id: string): PurchaseRow | undefined {
    return this.db.prepare('SELECT * FROM purchases WHERE id = ?').get(id) as
      | PurchaseRow
      | undefined;
  }

  decrementListing(listingId: string, quantity: number): void {
    this.db
      .prepare('UPDATE listings SET quantity_remaining = quantity_remaining - ? WHERE id = ?')
      .run(quantity, listingId);
  }

  // ── Credits domain ─────────────────────────────────────────────────────

  insertRetirementIfAbsent(row: RetirementRow): void {
    this.db
      .prepare(
        `INSERT OR IGNORE INTO retirements
           (id, owner_id, credit_batch_id, quantity, reason, created_at)
         VALUES (?, ?, ?, ?, ?, ?)`,
      )
      .run(row.id, row.owner_id, row.credit_batch_id, row.quantity, row.reason, row.created_at);
  }

  getRetirement(id: string): RetirementRow | undefined {
    return this.db.prepare('SELECT * FROM retirements WHERE id = ?').get(id) as
      | RetirementRow
      | undefined;
  }

  // ── Ledger cursors ─────────────────────────────────────────────────────

  getCursor(eventType: string): LedgerCursorRow | undefined {
    return this.db
      .prepare('SELECT * FROM ledger_cursors WHERE event_type = ?')
      .get(eventType) as LedgerCursorRow | undefined;
  }

  upsertCursor(eventType: string, lastLedger: number, updatedAt: number): void {
    this.db
      .prepare(
        `INSERT INTO ledger_cursors (event_type, last_ledger, updated_at)
         VALUES (?, ?, ?)
         ON CONFLICT(event_type) DO UPDATE SET
           last_ledger = excluded.last_ledger,
           updated_at  = excluded.updated_at`,
      )
      .run(eventType, lastLedger, updatedAt);
  }

  // ── Indexed events ─────────────────────────────────────────────────────

  /**
   * Insert an indexed event, ignoring duplicates.
   * Returns the inserted row's id, or undefined if it was a duplicate.
   */
  insertIndexedEventIfAbsent(row: Omit<IndexedEventRow, 'id'>): number | undefined {
    const info = this.db
      .prepare(
        `INSERT OR IGNORE INTO indexed_events
           (contract_id, topic, transaction_hash, ledger, payload, indexed_at)
         VALUES (?, ?, ?, ?, ?, ?)`,
      )
      .run(
        row.contract_id,
        row.topic,
        row.transaction_hash,
        row.ledger,
        row.payload,
        row.indexed_at,
      );
    return info.changes > 0 ? Number(info.lastInsertRowid) : undefined;
  }

  getIndexedEvent(id: number): IndexedEventRow | undefined {
    return this.db
      .prepare('SELECT * FROM indexed_events WHERE id = ?')
      .get(id) as IndexedEventRow | undefined;
  }

  // ── Webhook registrations ──────────────────────────────────────────────

  insertWebhook(row: WebhookRegistrationRow): void {
    this.db
      .prepare(
        `INSERT INTO webhook_registrations
           (id, url, secret, event_type, active, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(row.id, row.url, row.secret, row.event_type, row.active, row.created_at, row.updated_at);
  }

  getWebhook(id: string): WebhookRegistrationRow | undefined {
    return this.db
      .prepare('SELECT * FROM webhook_registrations WHERE id = ?')
      .get(id) as WebhookRegistrationRow | undefined;
  }

  updateWebhook(
    id: string,
    fields: Partial<Pick<WebhookRegistrationRow, 'url' | 'secret' | 'active'>>,
    updatedAt: number,
  ): void {
    const sets: string[] = [];
    const values: unknown[] = [];
    if (fields.url !== undefined) { sets.push('url = ?'); values.push(fields.url); }
    if (fields.secret !== undefined) { sets.push('secret = ?'); values.push(fields.secret); }
    if (fields.active !== undefined) { sets.push('active = ?'); values.push(fields.active); }
    if (sets.length === 0) return;
    sets.push('updated_at = ?');
    values.push(updatedAt);
    values.push(id);
    this.db.prepare(`UPDATE webhook_registrations SET ${sets.join(', ')} WHERE id = ?`).run(...values);
  }

  listWebhooksByEventType(eventType: string): WebhookRegistrationRow[] {
    return this.db
      .prepare('SELECT * FROM webhook_registrations WHERE event_type = ? AND active = 1')
      .all(eventType) as WebhookRegistrationRow[];
  }

  listAllWebhooks(): WebhookRegistrationRow[] {
    return this.db
      .prepare('SELECT * FROM webhook_registrations ORDER BY created_at DESC')
      .all() as WebhookRegistrationRow[];
  }

  // ── Webhook deliveries ─────────────────────────────────────────────────

  insertWebhookDelivery(row: Omit<WebhookDeliveryRow, 'id'>): number {
    const info = this.db
      .prepare(
        `INSERT INTO webhook_deliveries
           (webhook_id, event_id, status, attempt_count, last_attempt_at,
            next_attempt_at, response_status, error_message, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        row.webhook_id,
        row.event_id,
        row.status,
        row.attempt_count,
        row.last_attempt_at,
        row.next_attempt_at,
        row.response_status,
        row.error_message,
        row.created_at,
      );
    return Number(info.lastInsertRowid);
  }

  getPendingDeliveries(now: number, limit = 50): WebhookDeliveryRow[] {
    return this.db
      .prepare(
        `SELECT * FROM webhook_deliveries
         WHERE status IN ('pending','failed') AND next_attempt_at <= ?
         ORDER BY next_attempt_at ASC LIMIT ?`,
      )
      .all(now, limit) as WebhookDeliveryRow[];
  }

  updateDelivery(
    id: number,
    fields: Partial<
      Pick<
        WebhookDeliveryRow,
        'status' | 'attempt_count' | 'last_attempt_at' | 'next_attempt_at' | 'response_status' | 'error_message'
      >
    >,
  ): void {
    const sets: string[] = [];
    const values: unknown[] = [];
    if (fields.status !== undefined) { sets.push('status = ?'); values.push(fields.status); }
    if (fields.attempt_count !== undefined) { sets.push('attempt_count = ?'); values.push(fields.attempt_count); }
    if (fields.last_attempt_at !== undefined) { sets.push('last_attempt_at = ?'); values.push(fields.last_attempt_at); }
    if (fields.next_attempt_at !== undefined) { sets.push('next_attempt_at = ?'); values.push(fields.next_attempt_at); }
    if (fields.response_status !== undefined) { sets.push('response_status = ?'); values.push(fields.response_status); }
    if (fields.error_message !== undefined) { sets.push('error_message = ?'); values.push(fields.error_message); }
    if (sets.length === 0) return;
    values.push(id);
    this.db.prepare(`UPDATE webhook_deliveries SET ${sets.join(', ')} WHERE id = ?`).run(...values);
  }

  countPendingDeliveries(): number {
    const row = this.db
      .prepare(`SELECT COUNT(*) AS n FROM webhook_deliveries WHERE status IN ('pending','failed')`)
      .get() as { n: number };
    return row.n;
  }
}
