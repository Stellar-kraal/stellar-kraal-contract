import { randomUUID } from "crypto";
import request from "supertest";
import { createApp } from "../src/app";
import { SimulatedChainClient } from "../src/chain/chainClient";
import { Store } from "../src/db/database";
import {
  CREATE_LISTING_BODY,
  EXPECTED_LISTING_DTO_KEYS,
  makeListingRow,
} from "./fixtures/marketplaceListings";

const TTL = 3600;
const NOT_FOUND_DTO = { error: "listing not found" };
const BAD_ID_DTO = { error: "id path parameter must be a non-empty string" };

/**
 * Deterministic harness: in-memory database, simulated chain and a fixed
 * clock — mirrors the pattern of idempotency.integration.test.ts.
 * A fresh app per test also resets the in-memory rate-limit store, so the
 * default ipBurst budget (20 req / 10 s per IP) is never exhausted by the
 * request volumes used here (max 10 per test).
 */
function makeTestApp() {
  const clock = 1_700_000_000;
  const config = { idempotencyTtlSeconds: TTL, now: () => clock };
  const store = new Store(":memory:");
  const chain = new SimulatedChainClient(store, config.now);
  const { app } = createApp({ store, chain, config });
  return { app, store, chain };
}

/**
 * Canary introspection: total on-chain events recorded so far. A read
 * endpoint must never emit chain events; any delta exposes an accidental
 * side effect introduced in the handler.
 */
function chainEventCount(store: Store): number {
  const row = store.db
    .prepare("SELECT COUNT(*) AS n FROM chain_events")
    .get() as { n: number };
  return row.n;
}

/** The error contract is exactly `{ error }` — never stack/SQL internals. */
function expectExactErrorKeys(body: unknown): void {
  expect(Object.keys(body as Record<string, unknown>)).toEqual(["error"]);
}

describe("GET /marketplace/listings/:id — Happy Path (200 OK)", () => {
  test("returns the seeded listing with field-by-field fidelity against ListingResponseDTO", async () => {
    // Arrange
    const { app, store } = makeTestApp();
    const row = makeListingRow();
    store.upsertListing(row);

    // Act
    const res = await request(app).get(`/marketplace/listings/${row.id}`);

    // Assert — status and transport contract
    expect(res.status).toBe(200);
    expect(res.headers["content-type"]).toMatch(/application\/json/);

    // Assert — field-by-field ListingRow → ListingResponseDTO mapping
    expect(res.body.listingId).toBe(row.id);
    expect(res.body.sellerId).toBe(row.seller_id);
    expect(res.body.creditBatchId).toBe(row.credit_batch_id);
    expect(res.body.quantity).toBe(row.quantity_total);
    expect(res.body.quantityRemaining).toBe(row.quantity_remaining);
    expect(res.body.priceStroops).toBe(row.price_stroops);
    expect(res.body.createdAt).toBe(row.created_at);

    // Assert — numeric types survive JSON serialization
    expect(typeof res.body.quantity).toBe("number");
    expect(typeof res.body.quantityRemaining).toBe("number");
    expect(typeof res.body.priceStroops).toBe("number");
    expect(typeof res.body.createdAt).toBe("number");

    // Assert — exact key set: no snake_case leakage, no extra/missing fields
    expect(Object.keys(res.body).sort()).toEqual(
      [...EXPECTED_LISTING_DTO_KEYS].sort(),
    );
  });

  test("emits a DTO structurally identical to the elements of GET /listings", async () => {
    // Arrange — create through the real write path so both endpoints map
    // the same persisted row (ADR-001 §7.3: shape compatibility contract).
    const { app } = makeTestApp();
    const created = await request(app)
      .post("/marketplace/listings")
      .set("Idempotency-Key", "parity-key")
      .send(CREATE_LISTING_BODY);
    expect(created.status).toBe(201);

    // Act
    const byId = await request(app).get(
      `/marketplace/listings/${created.body.listingId}`,
    );
    const collection = await request(app).get("/marketplace/listings");

    // Assert
    expect(byId.status).toBe(200);
    expect(collection.body.listings).toHaveLength(1);
    expect(byId.body).toEqual(collection.body.listings[0]);
  });

  test("returns a fully-sold listing (quantityRemaining = 0) instead of collapsing to 404", async () => {
    // Arrange — boundary value: a truthiness regression such as
    // `if (!row.quantity_remaining)` would make this listing invisible.
    const { app, store } = makeTestApp();
    const row = makeListingRow({ id: "lst_sold_out", quantity_remaining: 0 });
    store.upsertListing(row);

    // Act
    const res = await request(app).get(`/marketplace/listings/${row.id}`);

    // Assert
    expect(res.status).toBe(200);
    expect(res.body.listingId).toBe(row.id);
    expect(res.body.quantityRemaining).toBe(0);
  });
});

describe("GET /marketplace/listings/:id — 404 Not Found (valid but absent ids)", () => {
  test("unknown UUID returns the standard ErrorResponseDTO", async () => {
    // Arrange
    const { app, store } = makeTestApp();
    store.upsertListing(makeListingRow());

    // Act
    const res = await request(app).get(`/marketplace/listings/${randomUUID()}`);

    // Assert
    expect(res.status).toBe(404);
    expect(res.body).toEqual(NOT_FOUND_DTO);
    expectExactErrorKeys(res.body);
    expect(res.headers["content-type"]).toMatch(/application\/json/);
  });

  test("alphanumeric id absent from the database returns 404", async () => {
    // Arrange
    const { app, store } = makeTestApp();
    store.upsertListing(makeListingRow());

    // Act
    const res = await request(app).get(
      "/marketplace/listings/lst_0000000000000000000000000dead",
    );

    // Assert
    expect(res.status).toBe(404);
    expect(res.body).toEqual(NOT_FOUND_DTO);
    expectExactErrorKeys(res.body);
  });

  test("lookup is case-sensitive: a different-case id does not match", async () => {
    // Arrange — SQLite TEXT equality uses BINARY collation here; this pins
    // the contract so a future collation change is detected loudly.
    const { app, store } = makeTestApp();
    store.upsertListing(makeListingRow({ id: "lst_CaseSensitive001" }));

    // Act
    const res = await request(app).get(
      "/marketplace/listings/lst_casesensitive001",
    );

    // Assert
    expect(res.status).toBe(404);
    expect(res.body).toEqual(NOT_FOUND_DTO);
  });

  test("trailing-space id passes validation but misses the store (no implicit trim)", async () => {
    // Arrange — "lst_x " survives isValidListingId (trim only feeds the
    // emptiness check; the lookup key is never mutated), so this must be a
    // plain miss, not a 400 and never an accidental hit on "lst_x".
    const { app, store } = makeTestApp();
    store.upsertListing(makeListingRow({ id: "lst_x" }));

    // Act
    const res = await request(app).get("/marketplace/listings/lst_x%20");

    // Assert
    expect(res.status).toBe(404);
    expect(res.body).toEqual(NOT_FOUND_DTO);
    expect(store.getListing("lst_x")).toBeDefined();
  });

  test("astral-plane id (emoji) is treated as one code point and 404s cleanly", async () => {
    // Arrange — U+1F680 is a surrogate pair in UTF-16; Array.from +
    // codePointAt must classify it as a single valid character.
    const { app, store } = makeTestApp();
    store.upsertListing(makeListingRow());

    // Act
    const res = await request(app).get(
      `/marketplace/listings/${encodeURIComponent("🚀")}`,
    );

    // Assert
    expect(res.status).toBe(404);
    expect(res.body).toEqual(NOT_FOUND_DTO);
  });

  test("C1 control range (U+0080–U+009F) currently passes validation and 404s", async () => {
    // Arrange — pins the exact validator boundary for the security audit:
    // only code points < 0x20 and 0x7F are rejected today.
    const { app, store } = makeTestApp();
    store.upsertListing(makeListingRow());

    // Act — U+009F (APPLICATION PROGRAM COMMAND) percent-encoded in UTF-8
    const res = await request(app).get("/marketplace/listings/%C2%9F");

    // Assert
    expect(res.status).toBe(404);
    expect(res.body).toEqual(NOT_FOUND_DTO);
  });

  test("anomalously long id (2048 chars) returns 404 without crashing", async () => {
    // Arrange — the validator imposes no length cap; the contract is that a
    // long-but-valid id is a normal miss, never a 500 or a hang.
    const { app, store } = makeTestApp();
    store.upsertListing(makeListingRow());
    const longId = "a".repeat(2048);

    // Act
    const res = await request(app).get(`/marketplace/listings/${longId}`);

    // Assert
    expect(res.status).toBe(404);
    expect(res.body).toEqual(NOT_FOUND_DTO);
    expect(store.listListings()).toHaveLength(1);
  });
});

describe("GET /marketplace/listings/:id — 400 Bad Request (malformed ids)", () => {
  const MALFORMED_CASES: Array<{ label: string; rawSegment: string }> = [
    { label: "single space (%20)", rawSegment: "%20" },
    { label: "whitespace-only (%20%20%20)", rawSegment: "%20%20%20" },
    { label: "tab (%09)", rawSegment: "%09" },
    { label: "null byte (%00)", rawSegment: "%00" },
    { label: "newline (%0A)", rawSegment: "%0A" },
    { label: "unit separator (%1F)", rawSegment: "%1F" },
    { label: "delete (%7F)", rawSegment: "%7F" },
    { label: "control char embedded (abc%01def)", rawSegment: "abc%01def" },
    { label: "DEL embedded (abc%7Fdef)", rawSegment: "abc%7Fdef" },
  ];

  test.each(MALFORMED_CASES)(
    "rejects $label with the standard 400 ErrorResponseDTO",
    async ({ rawSegment }) => {
      // Arrange — one real listing so we can prove the store was untouched
      const { app, store } = makeTestApp();
      store.upsertListing(makeListingRow());
      const chainBefore = chainEventCount(store);

      // Act
      const res = await request(app).get(`/marketplace/listings/${rawSegment}`);

      // Assert — error contract
      expect(res.status).toBe(400);
      expect(res.body).toEqual(BAD_ID_DTO);
      expectExactErrorKeys(res.body);

      // Assert — rejection happens at the HTTP edge, before the Store/chain
      expect(store.listListings()).toHaveLength(1);
      expect(chainEventCount(store)).toBe(chainBefore);
    },
  );

  test("truly empty segment falls through to the collection route (Express non-strict routing)", async () => {
    // GET /marketplace/listings/ carries no :id segment at all, so it never
    // reaches this endpoint: Express resolves it as GET /listings. Pinned so
    // a future strict-routing change cannot silently alter the surface.
    const { app, store } = makeTestApp();
    store.upsertListing(makeListingRow());

    // Act
    const res = await request(app).get("/marketplace/listings/");

    // Assert
    expect(res.status).toBe(200);
    expect(Array.isArray(res.body.listings)).toBe(true);
  });
});

describe("GET /marketplace/listings/:id — hostile payloads", () => {
  test("SQL-injection-shaped id is looked up literally and the table survives", async () => {
    // Arrange
    const { app, store } = makeTestApp();
    const row = makeListingRow({ id: "lst_sqli_target" });
    store.upsertListing(row);

    // Act — "' OR '1'='1" percent-encoded
    const res = await request(app).get(
      `/marketplace/listings/${encodeURIComponent("' OR '1'='1")}`,
    );

    // Assert — plain miss, standard contract, table fully intact
    expect(res.status).toBe(404);
    expect(res.body).toEqual(NOT_FOUND_DTO);
    expect(store.listListings()).toHaveLength(1);
    expect(store.getListing(row.id)).toBeDefined();

    // Assert — a legitimate read still works afterwards (no corrupted state)
    const after = await request(app).get(`/marketplace/listings/${row.id}`);
    expect(after.status).toBe(200);
    expect(after.body.listingId).toBe(row.id);
  });

  test("path-traversal-shaped id cannot escape the route and leaks nothing", async () => {
    // Arrange
    const { app, store } = makeTestApp();
    store.upsertListing(makeListingRow());

    // Act — "../../etc/passwd" with percent-encoded slashes
    const res = await request(app).get(
      "/marketplace/listings/..%2F..%2Fetc%2Fpasswd",
    );

    // Assert
    expect(res.status).toBe(404);
    expect(res.body).toEqual(NOT_FOUND_DTO);
    expect(JSON.stringify(res.body)).not.toMatch(/root:|BEGIN|stack/i);
  });

  test("error responses never leak stack traces or SQL details", async () => {
    // Arrange
    const { app } = makeTestApp();

    // Act
    const notFound = await request(app).get("/marketplace/listings/nope");
    const badId = await request(app).get("/marketplace/listings/%20");

    // Assert
    for (const res of [notFound, badId]) {
      expectExactErrorKeys(res.body);
      expect(JSON.stringify(res.body)).not.toMatch(
        /stack|sqlite|SELECT|at\s+\w+\s+\(/i,
      );
    }
  });
});

describe("GET /marketplace/listings/:id — read idempotency and consistency", () => {
  test("five consecutive reads return identical bodies and mutate nothing", async () => {
    // Arrange
    const { app, store } = makeTestApp();
    const row = makeListingRow();
    store.upsertListing(row);
    const rowBefore = store.getListing(row.id);
    const chainBefore = chainEventCount(store);

    // Act
    const responses = [];
    for (let i = 0; i < 5; i++) {
      responses.push(await request(app).get(`/marketplace/listings/${row.id}`));
    }

    // Assert — uniform 200 with identical payloads
    for (const res of responses) {
      expect(res.status).toBe(200);
      expect(res.body).toEqual(responses[0].body);
    }

    // Assert — zero state drift: same row, same listing count, no chain events
    expect(store.getListing(row.id)).toEqual(rowBefore);
    expect(store.listListings()).toHaveLength(1);
    expect(chainEventCount(store)).toBe(chainBefore);
  });

  test("ten concurrent reads all succeed with identical payloads", async () => {
    // Arrange
    const { app, store } = makeTestApp();
    const row = makeListingRow();
    store.upsertListing(row);

    // Act
    const responses = await Promise.all(
      Array.from({ length: 10 }, () =>
        request(app).get(`/marketplace/listings/${row.id}`),
      ),
    );

    // Assert
    for (const res of responses) {
      expect(res.status).toBe(200);
      expect(res.body).toEqual(responses[0].body);
    }
    expect(store.listListings()).toHaveLength(1);
  });

  test("an Idempotency-Key header on a GET is ignored — no record is persisted", async () => {
    // Arrange — ADR-001 §7.1: reads must not enter the idempotency pipeline.
    // A persisted record here would mean the route was wrongly wrapped.
    const { app, store } = makeTestApp();
    const row = makeListingRow();
    store.upsertListing(row);
    const key = "get-must-not-persist";

    // Act
    const res = await request(app)
      .get(`/marketplace/listings/${row.id}`)
      .set("Idempotency-Key", key);

    // Assert
    expect(res.status).toBe(200);
    expect(store.getRecord(key)).toBeUndefined();
  });
});
