import {
  LISTING_PAGINATION_DEFAULTS,
  LISTING_PAGINATION_MAX_LIMIT,
  LISTING_PAGINATION_MIN_LIMIT,
  LISTING_PAGINATION_MIN_OFFSET,
  ListingPaginationQueryParser,
  PaginationParseResult,
} from "../contracts/listingPagination";

/** Strict digits-only integer — rejects signs, decimals, exponents, and whitespace. */
const INTEGER_PATTERN = /^\d+$/;

const ALLOWED_QUERY_KEYS = new Set(["limit", "offset"]);

function parseBoundedInt(
  raw: string,
  field: string,
  min: number,
  max: number,
): number | { error: string } {
  if (!INTEGER_PATTERN.test(raw)) {
    return { error: `${field} must be a non-negative integer` };
  }
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value < min || value > max) {
    return { error: `${field} must be between ${min} and ${max}` };
  }
  return value;
}

/**
 * Parses `GET /marketplace/listings` query params into a `ListingPagination`
 * DTO per ADR-0112 §3. Express-specific (reads `req.query`'s string/array
 * shape) but produces a framework-agnostic result — no HTTP concerns leak
 * past `parse()`.
 */
export class ExpressListingPaginationQueryParser
  implements ListingPaginationQueryParser
{
  parse(query: unknown): PaginationParseResult {
    const q = (query ?? {}) as Record<string, unknown>;

    for (const key of Object.keys(q)) {
      if (!ALLOWED_QUERY_KEYS.has(key)) {
        return { error: `unknown query parameter: ${key}` };
      }
    }

    let limit = LISTING_PAGINATION_DEFAULTS.limit;
    if (q.limit !== undefined) {
      if (typeof q.limit !== "string") {
        return { error: "limit must be provided exactly once" };
      }
      const result = parseBoundedInt(
        q.limit,
        "limit",
        LISTING_PAGINATION_MIN_LIMIT,
        LISTING_PAGINATION_MAX_LIMIT,
      );
      if (typeof result !== "number") return { error: result.error };
      limit = result;
    }

    let offset = LISTING_PAGINATION_DEFAULTS.offset;
    if (q.offset !== undefined) {
      if (typeof q.offset !== "string") {
        return { error: "offset must be provided exactly once" };
      }
      const result = parseBoundedInt(
        q.offset,
        "offset",
        LISTING_PAGINATION_MIN_OFFSET,
        Number.MAX_SAFE_INTEGER,
      );
      if (typeof result !== "number") return { error: result.error };
      offset = result;
    }

    return { value: { limit, offset } };
  }
}
