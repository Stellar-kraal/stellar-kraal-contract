import {
  LISTING_PAGINATION_DEFAULTS,
  LISTING_PAGINATION_MAX_LIMIT,
  LISTING_PAGINATION_MIN_LIMIT,
  LISTING_PAGINATION_MIN_OFFSET,
  ListingPagination,
} from "../src/marketplace/contracts/listingPagination";
import { ListingReadModel } from "../src/marketplace/contracts/listingReadModel";
import { ListingReader } from "../src/marketplace/ports/listingReader";
import { ListListingsUseCase } from "../src/marketplace/useCases/listListings";

/** In-memory fake satisfying the output port; no SQLite involved (ADR-0112 §6). */
class FakeListingReader implements ListingReader {
  public receivedPagination: ListingPagination[] = [];
  constructor(private readonly rows: ListingReadModel[]) {}
  listListings(pagination: ListingPagination): ListingReadModel[] {
    this.receivedPagination.push(pagination);
    return this.rows;
  }
}

class ThrowingListingReader implements ListingReader {
  listListings(): ListingReadModel[] {
    throw new Error("adapter exploded");
  }
}

function makeListing(
  overrides: Partial<ListingReadModel> = {},
): ListingReadModel {
  return {
    id: "listing-1",
    sellerId: "seller-1",
    creditBatchId: "batch-1",
    quantity: 10,
    quantityRemaining: 10,
    priceStroops: 1000,
    createdAt: 1_700_000_000,
    ...overrides,
  };
}

describe("ListListingsUseCase — pure domain orchestration (no SQLite)", () => {
  test("forwards the exact pagination object to the port untouched", () => {
    const reader = new FakeListingReader([]);
    const useCase = new ListListingsUseCase(reader);
    const pagination: ListingPagination = { limit: 42, offset: 7 };

    useCase.execute(pagination);

    expect(reader.receivedPagination).toEqual([pagination]);
  });

  test("does not mutate the pagination object it receives", () => {
    const reader = new FakeListingReader([]);
    const useCase = new ListListingsUseCase(reader);
    const pagination: ListingPagination = { limit: 42, offset: 7 };
    const frozen = Object.freeze({ ...pagination });

    expect(() => useCase.execute(frozen)).not.toThrow();
  });

  test("returns exactly what the port returns, without cloning or reshaping", () => {
    const rows = [makeListing({ id: "a" }), makeListing({ id: "b" })];
    const reader = new FakeListingReader(rows);
    const useCase = new ListListingsUseCase(reader);

    const result = useCase.execute(LISTING_PAGINATION_DEFAULTS);

    expect(result).toBe(rows);
  });

  test("handles an empty page from the port", () => {
    const useCase = new ListListingsUseCase(new FakeListingReader([]));

    expect(useCase.execute({ limit: 100, offset: 999_999 })).toEqual([]);
  });

  test("propagates port errors instead of swallowing them", () => {
    const useCase = new ListListingsUseCase(new ThrowingListingReader());

    expect(() => useCase.execute(LISTING_PAGINATION_DEFAULTS)).toThrow(
      "adapter exploded",
    );
  });

  test("invokes the port exactly once per execute() call (no retries, no duplicate reads)", () => {
    const reader = new FakeListingReader([]);
    const useCase = new ListListingsUseCase(reader);

    useCase.execute({ limit: 1, offset: 0 });
    useCase.execute({ limit: 2, offset: 5 });

    expect(reader.receivedPagination).toEqual([
      { limit: 1, offset: 0 },
      { limit: 2, offset: 5 },
    ]);
  });
});

describe("ListingPagination contract constants (ADR-0112 §3.1)", () => {
  test("defaults match the documented contract: limit=100, offset=0", () => {
    expect(LISTING_PAGINATION_DEFAULTS).toEqual({ limit: 100, offset: 0 });
  });

  test("bounds match the documented contract", () => {
    expect(LISTING_PAGINATION_MIN_LIMIT).toBe(1);
    expect(LISTING_PAGINATION_MAX_LIMIT).toBe(500);
    expect(LISTING_PAGINATION_MIN_OFFSET).toBe(0);
  });

  test("defaults are frozen so no adapter can silently redefine them at runtime", () => {
    expect(Object.isFrozen(LISTING_PAGINATION_DEFAULTS)).toBe(true);
  });
});
