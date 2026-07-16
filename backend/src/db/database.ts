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
}
