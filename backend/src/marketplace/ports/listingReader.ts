import { ListingPagination } from "../contracts/listingPagination";
import { ListingReadModel } from "../contracts/listingReadModel";

/** Output port for reading listings. Implemented by the infrastructure adapter (`Store`). */
export interface ListingReader {
  listListings(pagination: ListingPagination): ListingReadModel[];
}
