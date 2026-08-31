import request from "supertest";
import { createApp } from "../src/app";
import { RateLimitConfig } from "../src/config/rateLimits";
import { ListingRow, Store } from "../src/db/database";

/**
 * Rate limits generous enough that pagination assertions never collide with
 * the 429 tier under test elsewhere (rateLimit.integration.test.ts already
 * covers throttling behavior in isolation).
 */
const GENEROUS_LIMITS: RateLimitConfig = {
  tiers: {
    ipBurst: { limit: 100_000, windowSeconds: 10 },
    userSustained: { limit: 100_000, windowSeconds: 60 },
  },
  endpoints: {
    "GET /marketplace/listings": { limit: 100_000, windowSeconds: 60 },
  },
  exemptPaths: ["/health", "/metrics"],
};

function makeTestApp() {
  const store = new Store(":memory:");
  const config = { idempotencyTtlSeconds: 3600, now: () => 1_700_000_000 };
  const { app } = createApp({
    store,
    config,
    rateLimitConfig: GENEROUS_LIMITS,
    trustProxy: true,
  });
  return { app, store };
}

/** Seeds `count` listings with strictly increasing `created_at`. */
function seedListings(
  store: Store,
  count: number,
  baseCreatedAt = 1_700_000_000,
): ListingRow[] {
  const rows: ListingRow[] = [];
  for (let i = 0; i < count; i++) {
    const row: ListingRow = {
      id: `listing-${String(i).padStart(4, "0")}`,
      seller_id: `seller-${i}`,
      credit_batch_id: `batch-${i}`,
      quantity_total: 100 + i,
      quantity_remaining: 100 + i,
      price_stroops: 1_000_000 + i,
      created_at: baseCreatedAt + i,
    };
    store.upsertListing(row);
    rows.push(row);
  }
  return rows;
}

/** ADR-0112 §3.3 ordering: created_at DESC, id ASC as the stable tiebreaker. */
function expectedOrder(rows: ListingRow[]): ListingRow[] {
  return [...rows].sort(
    (a, b) => b.created_at - a.created_at || a.id.localeCompare(b.id),
  );
}

describe("GET /marketplace/listings — pagination happy path (pre-seeded dataset)", () => {
  test("limit=10&offset=20 returns the correct slice of pre-seeded data", async () => {
    const { app, store } = makeTestApp();
    const rows = seedListings(store, 50);
    const expected = expectedOrder(rows);

    const res = await request(app).get(
      "/marketplace/listings?limit=10&offset=20",
    );

    expect(res.status).toBe(200);
    expect(res.body.listings).toHaveLength(10);
    expect(
      res.body.listings.map((l: { listingId: string }) => l.listingId),
    ).toEqual(expected.slice(20, 30).map((r) => r.id));
  });

  test("no query params: defaults to 100 items, offset 0, stable order", async () => {
    const { app, store } = makeTestApp();
    const rows = seedListings(store, 150);
    const expected = expectedOrder(rows);

    const res = await request(app).get("/marketplace/listings");

    expect(res.status).toBe(200);
    expect(res.body.listings).toHaveLength(100);
    expect(
      res.body.listings.map((l: { listingId: string }) => l.listingId),
    ).toEqual(expected.slice(0, 100).map((r) => r.id));
  });

  test("secondary sort by id ASC applies only when created_at ties", async () => {
    const { app, store } = makeTestApp();
    const tie = 1_800_000_000;
    // Insert out of lexicographic order to prove the DB — not insertion order — drives the tiebreak.
    for (const id of ["listing-c", "listing-a", "listing-b"]) {
      store.upsertListing({
        id,
        seller_id: "s",
        credit_batch_id: "b",
        quantity_total: 1,
        quantity_remaining: 1,
        price_stroops: 1,
        created_at: tie,
      });
    }

    const res = await request(app).get(
      "/marketplace/listings?limit=10&offset=0",
    );

    expect(res.status).toBe(200);
    expect(
      res.body.listings.map((l: { listingId: string }) => l.listingId),
    ).toEqual(["listing-a", "listing-b", "listing-c"]);
  });

  test.each([
    ["limit=1 (lower boundary) is accepted", "limit=1", 1],
    ["limit=500 (upper boundary) is accepted", "limit=500", 10],
  ])("%s", async (_label, qs, expectedCount) => {
    const { app, store } = makeTestApp();
    seedListings(store, 10);

    const res = await request(app).get(`/marketplace/listings?${qs}`);

    expect(res.status).toBe(200);
    expect(res.body.listings).toHaveLength(expectedCount);
  });

  test("offset beyond dataset size returns an empty page, not an error", async () => {
    const { app, store } = makeTestApp();
    seedListings(store, 5);

    const res = await request(app).get(
      "/marketplace/listings?limit=10&offset=1000",
    );

    expect(res.status).toBe(200);
    expect(res.body.listings).toEqual([]);
  });

  test("response shape matches the ADR-0112 public DTO exactly ({ listings })", async () => {
    const { app, store } = makeTestApp();
    seedListings(store, 1);

    const res = await request(app).get("/marketplace/listings?limit=1");

    expect(res.status).toBe(200);
    expect(Object.keys(res.body)).toEqual(["listings"]);
    expect(res.body.listings[0]).toEqual({
      listingId: "listing-0000",
      sellerId: "seller-0",
      creditBatchId: "batch-0",
      quantity: 100,
      quantityRemaining: 100,
      priceStroops: 1_000_000,
      createdAt: 1_700_000_000,
    });
  });
});

describe("GET /marketplace/listings — malformed/out-of-range parameters reject with 400", () => {
  test.each([
    ["limit above the maximum", "limit=501"],
    ["limit at zero (below the minimum)", "limit=0"],
    ["negative limit", "limit=-10"],
    ["offset below the minimum", "offset=-1"],
    ["non-numeric limit", "limit=abc"],
    ["non-numeric offset", "offset=abc"],
    ["decimal limit", "limit=1.5"],
    ["decimal offset", "offset=1.5"],
    ["exponent notation", "limit=1e2"],
    ["explicit plus sign (url-encoded)", "limit=%2B10"],
    ["empty string value", "limit="],
    ["leading whitespace (url-encoded)", "limit=%2010"],
    ["trailing whitespace (url-encoded)", "limit=10%20"],
    ["literal Infinity", "offset=Infinity"],
    ["literal NaN", "offset=NaN"],
    ["integer beyond Number.MAX_SAFE_INTEGER", "limit=9999999999999999999"],
    ["unknown query key alone", "foo=bar"],
    ["unknown query key alongside a valid one", "limit=10&foo=bar"],
    ["repeated limit key (array)", "limit=10&limit=20"],
    ["repeated offset key (array)", "offset=0&offset=5"],
  ])("%s -> HTTP 400", async (_label, qs) => {
    const { app } = makeTestApp();

    const res = await request(app).get(`/marketplace/listings?${qs}`);

    expect(res.status).toBe(400);
    expect(typeof res.body.error).toBe("string");
    expect(res.body.error.length).toBeGreaterThan(0);
  });

  test("error responses never leak SQL, stack traces, or internal identifiers", async () => {
    const { app } = makeTestApp();

    const res = await request(app).get("/marketplace/listings?limit=501");

    expect(res.status).toBe(400);
    expect(JSON.stringify(res.body)).not.toMatch(
      /select|sqlite|stack|at Object|node_modules/i,
    );
  });

  test("a rejected request never reaches the store (no rows read on 400)", async () => {
    const { app, store } = makeTestApp();
    seedListings(store, 5);
    const spy = jest.spyOn(store, "listListings");

    const res = await request(app).get("/marketplace/listings?limit=501");

    expect(res.status).toBe(400);
    expect(spy).not.toHaveBeenCalled();
    spy.mockRestore();
  });
});

describe("Store adapter (SQLite) — invariants hold even when called outside the HTTP boundary", () => {
  test("clamps limit to the ADR-0112 maximum even if invoked directly with an out-of-range value", () => {
    const store = new Store(":memory:");
    seedListings(store, 5);

    const result = store.listListings({ limit: 10_000, offset: 0 });

    expect(result.length).toBeLessThanOrEqual(5);
  });

  test("preserves created_at DESC, id ASC ordering when invoked directly", () => {
    const store = new Store(":memory:");
    const rows = seedListings(store, 20);
    const expected = expectedOrder(rows);

    const result = store.listListings({ limit: 20, offset: 0 });

    expect(result.map((r) => r.id)).toEqual(expected.map((r) => r.id));
  });

  test("maps snake_case columns to the camelCase ListingReadModel", () => {
    const store = new Store(":memory:");
    seedListings(store, 1);

    const [result] = store.listListings({ limit: 1, offset: 0 });

    expect(result).toEqual({
      id: "listing-0000",
      sellerId: "seller-0",
      creditBatchId: "batch-0",
      quantity: 100,
      quantityRemaining: 100,
      priceStroops: 1_000_000,
      createdAt: 1_700_000_000,
    });
  });
});
