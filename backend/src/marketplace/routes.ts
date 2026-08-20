import { Router } from "express";
import { IdempotencyDeps, idempotent } from "../middleware/idempotency";
import { MarketplaceService } from "./service";
import {
  LISTING_PAGINATION_DEFAULTS,
  LISTING_PAGINATION_MAX_LIMIT,
  LISTING_PAGINATION_MIN_LIMIT,
  LISTING_PAGINATION_MIN_OFFSET,
  ListingPagination,
  ListingPaginationQueryParser,
  PaginationParseResult,
} from "./contracts/listingPagination";
import { ListListingsUseCase } from "./useCases/listListings";

function isPositiveInt(v: unknown): v is number {
  return typeof v === "number" && Number.isInteger(v) && v > 0;
}
function isNonEmptyString(v: unknown): v is string {
  return typeof v === "string" && v.length > 0;
}

const STRICT_NONNEGATIVE_INT = /^\d+$/;

/**
 * HTTP-boundary DTO parser for `GET /marketplace/listings`. Rejects anything
 * that is not a single, strictly-formatted non-negative integer string.
 */
class DefaultListingPaginationQueryParser implements ListingPaginationQueryParser {
  parse(query: unknown): PaginationParseResult {
    if (query === null || typeof query !== "object") {
      return { error: "query must be an object" };
    }
    const q = query as Record<string, unknown>;
    const allowedKeys = new Set(["limit", "offset"]);
    for (const key of Object.keys(q)) {
      if (!allowedKeys.has(key)) {
        return { error: `unknown query parameter: ${key}` };
      }
    }

    const limit = this.parseField(
      q.limit,
      "limit",
      LISTING_PAGINATION_MIN_LIMIT,
      LISTING_PAGINATION_MAX_LIMIT,
      LISTING_PAGINATION_DEFAULTS.limit,
    );
    if ("error" in limit) return limit;

    const offset = this.parseField(
      q.offset,
      "offset",
      LISTING_PAGINATION_MIN_OFFSET,
      Number.MAX_SAFE_INTEGER,
      LISTING_PAGINATION_DEFAULTS.offset,
    );
    if ("error" in offset) return offset;

    const value: ListingPagination = {
      limit: limit.value,
      offset: offset.value,
    };
    return { value };
  }

  private parseField(
    raw: unknown,
    field: string,
    min: number,
    max: number,
    fallback: number,
  ): { value: number } | { error: string } {
    if (raw === undefined) return { value: fallback };
    if (Array.isArray(raw)) return { error: `${field} must not be repeated` };
    if (typeof raw !== "string" || !STRICT_NONNEGATIVE_INT.test(raw)) {
      return { error: `${field} must be a non-negative integer` };
    }
    const num = Number(raw);
    if (!Number.isSafeInteger(num)) {
      return { error: `${field} is not a safe integer` };
    }
    if (num < min || num > max) {
      return { error: `${field} must be between ${min} and ${max}` };
    }
    return { value: num };
  }
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
  const paginationParser = new DefaultListingPaginationQueryParser();
  const listListingsUseCase = new ListListingsUseCase(deps.store);
  const router = Router();

  // Public browse endpoint (scrape target — rate limited per config).
  router.get("/listings", (req, res) => {
    const parsed = paginationParser.parse(req.query);
    if (parsed.error || !parsed.value) {
      res
        .status(400)
        .json({ error: parsed.error ?? "invalid pagination parameters" });
      return;
    }

    const listings = listListingsUseCase.execute(parsed.value).map((l) => ({
      listingId: l.id,
      sellerId: l.sellerId,
      creditBatchId: l.creditBatchId,
      quantity: l.quantity,
      quantityRemaining: l.quantityRemaining,
      priceStroops: l.priceStroops,
      createdAt: l.createdAt,
    }));
    res.json({ listings });
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
