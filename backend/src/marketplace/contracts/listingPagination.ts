/** Application-level pagination DTO for `GET /marketplace/listings`. Never import Express or SQLite here. */
export interface ListingPagination {
  /** Page size, integer in [1, 500]. Default 100. */
  limit: number;
  /** Rows skipped after the stable order, integer >= 0. Default 0. */
  offset: number;
}

export const LISTING_PAGINATION_DEFAULTS: Readonly<ListingPagination> =
  Object.freeze({
    limit: 100,
    offset: 0,
  });

export const LISTING_PAGINATION_MIN_LIMIT = 1;
export const LISTING_PAGINATION_MAX_LIMIT = 500;
export const LISTING_PAGINATION_MIN_OFFSET = 0;

export interface PaginationParseResult {
  value?: ListingPagination;
  error?: string;
}

export interface ListingPaginationQueryParser {
  parse(query: unknown): PaginationParseResult;
}
