import { Router } from "express";
import {
  LISTING_PAGINATION_DEFAULTS,
  LISTING_PAGINATION_MAX_LIMIT,
  LISTING_PAGINATION_MIN_LIMIT,
  LISTING_PAGINATION_MIN_OFFSET,
  ListingPagination,
  PaginationParseResult,
} from "./contracts/listingPagination";
import { ListingReadModel } from "./contracts/listingReadModel";
import { ListingRow } from "../db/database";
import { IdempotencyDeps, idempotent } from "../middleware/idempotency";
import { MarketplaceService } from "./service";

function isPositiveInt(v: unknown): v is number {
  return typeof v === "number" && Number.isInteger(v) && v > 0;
}
function isNonEmptyString(v: unknown): v is string {
  return typeof v === "string" && v.length > 0;
}

function isValidListingId(v: unknown): v is string {
  return (
    typeof v === "string" &&
    v.trim().length > 0 &&
    Array.from(v).every((character) => {
      const codePoint = character.codePointAt(0) ?? 0;
      return codePoint >= 0x20 && codePoint !== 0x7f;
    })
  );
}

function listingResponse(row: ListingRow) {
  return {
    listingId: row.id,
    sellerId: row.seller_id,
    creditBatchId: row.credit_batch_id,
    quantity: row.quantity_total,
    quantityRemaining: row.quantity_remaining,
    priceStroops: row.price_stroops,
    createdAt: row.created_at,
  };
}

function listingReadModelResponse(listing: ListingReadModel) {
  return {
    listingId: listing.id,
    sellerId: listing.sellerId,
    creditBatchId: listing.creditBatchId,
    quantity: listing.quantity,
    quantityRemaining: listing.quantityRemaining,
    priceStroops: listing.priceStroops,
    createdAt: listing.createdAt,
  };
}

/**
 * ADR-0112 query parsing for GET /marketplace/listings. Only `limit` and
 * `offset` are accepted; each must be a canonical unsigned decimal integer
 * within bounds. Rejects anything else so malformed pagination never reaches
 * the store (unknown keys, repeated keys, signs, decimals, whitespace, etc.).
 */
function parseListingsPagination(query: unknown): PaginationParseResult {
  if (typeof query !== "object" || query === null || Array.isArray(query)) {
    return { error: "query must be an object" };
  }
  const record = query as Record<string, unknown>;
  for (const key of Object.keys(record)) {
    if (key !== "limit" && key !== "offset") {
      return { error: `unknown query parameter: ${key}` };
    }
  }

  const value: ListingPagination = { ...LISTING_PAGINATION_DEFAULTS };
  const parseParam = (
    raw: unknown,
    key: "limit" | "offset",
    min: number,
    max: number | null,
  ): string | null => {
    if (raw === undefined) return null;
    if (typeof raw !== "string" || !/^\d+$/.test(raw)) {
      return `${key} must be a non-negative integer`;
    }
    const n = Number(raw);
    if (!Number.isSafeInteger(n) || n < min || (max !== null && n > max)) {
      return `${key} is out of range`;
    }
    value[key] = n;
    return null;
  };

  const limitError = parseParam(
    record.limit,
    "limit",
    LISTING_PAGINATION_MIN_LIMIT,
    LISTING_PAGINATION_MAX_LIMIT,
  );
  if (limitError) return { error: limitError };
  const offsetError = parseParam(
    record.offset,
    "offset",
    LISTING_PAGINATION_MIN_OFFSET,
    null,
  );
  if (offsetError) return { error: offsetError };
  return { value };
}

export function validateCreateListing(body: unknown): string | null {
  const b = body as Record<string, unknown> | null;
  if (!b || typeof b !== "object") return "request body must be a JSON object";
  if (!isNonEmptyString(b.sellerId)) return "sellerId is required";
  if (!isNonEmptyString(b.creditBatchId)) return "creditBatchId is required";
  if (!isPositiveInt(b.quantity)) return "quantity must be a positive integer";
  if (!isPositiveInt(b.priceStroops))
    return "priceStroops must be a positive integer";
  return null;
}

export function validatePurchase(body: unknown): string | null {
  const b = body as Record<string, unknown> | null;
  if (!b || typeof b !== "object") return "request body must be a JSON object";
  if (!isNonEmptyString(b.buyerId)) return "buyerId is required";
  if (!isNonEmptyString(b.listingId)) return "listingId is required";
  if (!isPositiveInt(b.quantity)) return "quantity must be a positive integer";
  return null;
}

export function marketplaceRoutes(deps: IdempotencyDeps): Router {
  const service = new MarketplaceService(
    deps.store,
    deps.chain,
    deps.config.now,
  );
  const router = Router();

  // Public browse endpoint (scrape target — rate limited per config).
  // Paginated per ADR-0112: limit (1..500) and offset, defaulting to 100/0.
  router.get("/listings", (req, res) => {
    const parsed = parseListingsPagination(req.query);
    if (parsed.error) {
      res.status(400).json({ error: parsed.error });
      return;
    }
    const listings = deps.store
      .listListings(parsed.value!)
      .map(listingReadModelResponse);
    res.json({ listings });
  });

  router.get("/listings/:id", (req, res) => {
    const { id } = req.params;
    if (!isValidListingId(id)) {
      res
        .status(400)
        .json({ error: "id path parameter must be a non-empty string" });
      return;
    }

    const listing = deps.store.getListing(id);
    if (!listing) {
      res.status(404).json({ error: "listing not found" });
      return;
    }

    res.json(listingResponse(listing));
  });

  // Public price query. Backed by the oracle feed in production; served
  // from the simulated feed here. Classified as an expensive on-chain read
  // for rate-limiting purposes.
  router.get("/prices", (_req, res) => {
    res.json({
      pair: "CARBON/XLM",
      priceStroops: 5_000_000,
      source: "simulated-oracle",
      updatedAt: deps.config.now(),
    });
  });

  router.post(
    "/listings",
    idempotent(deps, {
      endpoint: "POST /marketplace/listings",
      validate: validateCreateListing,
      execute: (body, key) => service.createListing(body, key),
      reconcile: (event) => service.reconcileListing(event),
    }),
  );

  router.post(
    "/purchases",
    idempotent(deps, {
      endpoint: "POST /marketplace/purchases",
      validate: validatePurchase,
      execute: (body, key) => service.purchase(body, key),
      reconcile: (event) => service.reconcilePurchase(event),
    }),
  );

  return router;
}
