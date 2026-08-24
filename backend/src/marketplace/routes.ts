import { Router } from "express";
import { ListingRow } from "../db/database";
import { IdempotencyDeps, idempotent } from "../middleware/idempotency";
import { ExpressListingPaginationQueryParser } from "./adapters/listingPaginationQueryParser";
import { ListingReadModel } from "./contracts/listingReadModel";
import { MarketplaceService } from "./service";
import { ListListingsUseCase } from "./useCases/listListings";

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

function listingReadModelResponse(model: ListingReadModel) {
  return {
    listingId: model.id,
    sellerId: model.sellerId,
    creditBatchId: model.creditBatchId,
    quantity: model.quantity,
    quantityRemaining: model.quantityRemaining,
    priceStroops: model.priceStroops,
    createdAt: model.createdAt,
  };
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
  const paginationParser = new ExpressListingPaginationQueryParser();
  const listListingsUseCase = new ListListingsUseCase(deps.store);
  const router = Router();

  // Public browse endpoint (scrape target — rate limited per config).
  // ADR-0112 §3: query params are validated before the store is ever
  // touched — a malformed/out-of-range/unknown param rejects with 400 and
  // no read happens.
  router.get("/listings", (req, res) => {
    const parsed = paginationParser.parse(req.query);
    if (parsed.error || !parsed.value) {
      res.status(400).json({ error: parsed.error ?? "invalid query parameters" });
      return;
    }

    const listings = listListingsUseCase
      .execute(parsed.value)
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
