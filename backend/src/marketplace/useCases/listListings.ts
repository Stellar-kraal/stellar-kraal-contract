import { ListingPagination } from "../contracts/listingPagination";
import { ListingReadModel } from "../contracts/listingReadModel";
import { ListingReader } from "../ports/listingReader";

/**
 * Orchestrates the paginated listings read. Receives the output port by
 * dependency injection and remains agnostic of Express and SQLite.
 */
export class ListListingsUseCase {
  constructor(private readonly listingReader: ListingReader) {}

  execute(pagination: ListingPagination): ListingReadModel[] {
    return this.listingReader.listListings(pagination);
  }
}
