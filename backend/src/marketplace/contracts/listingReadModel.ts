/** Application read model for a marketplace listing. Camel-cased, independent of `ListingRow`. */
export interface ListingReadModel {
  id: string;
  sellerId: string;
  creditBatchId: string;
  quantity: number;
  quantityRemaining: number;
  priceStroops: number;
  createdAt: number;
}
