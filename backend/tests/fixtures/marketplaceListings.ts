import { ListingRow } from "../../src/db/database";

/**
 * Reusable test-data factories for the marketplace bounded context.
 *
 * Keeping fixtures here prevents integration suites from being polluted
 * with giant inline JSON literals and gives every test the same
 * deterministic baseline row.
 */

/**
 * A ListingRow with fully deterministic, distinctive values so that a
 * field-by-field assertion can prove the ListingRow → ListingResponseDTO
 * mapping instead of merely asserting "some object came back".
 */
export function makeListingRow(
  overrides: Partial<ListingRow> = {},
): ListingRow {
  return {
    id: "lst_fixture_3f9a1c2b7e4d5601",
    seller_id: "seller_42",
    credit_batch_id: "batch_2026_q1",
    quantity_total: 100,
    quantity_remaining: 60,
    price_stroops: 5_000_000,
    created_at: 1_755_600_000_000,
    ...overrides,
  };
}

/**
 * Exact key set of the public ListingResponseDTO (ADR-001, issue-115 §9.2).
 * Comparing Object.keys(body) against this catches snake_case leakage from
 * ListingRow and any silently added or removed DTO fields.
 */
export const EXPECTED_LISTING_DTO_KEYS: readonly string[] = [
  "listingId",
  "sellerId",
  "creditBatchId",
  "quantity",
  "quantityRemaining",
  "priceStroops",
  "createdAt",
];

/** Valid body for POST /marketplace/listings (shape-parity tests). */
export const CREATE_LISTING_BODY = {
  sellerId: "seller-1",
  creditBatchId: "batch-2026-KE-001",
  quantity: 100,
  priceStroops: 5_000_000,
} as const;
