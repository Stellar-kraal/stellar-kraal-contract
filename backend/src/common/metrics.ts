/**
 * In-process counters for rate-limiter observability, exposed at
 * `GET /metrics` as JSON. The shape maps 1:1 onto Prometheus counters
 * (`ratelimit_allowed_total`, `ratelimit_rejected_total{tier,endpoint}`)
 * if an exporter is added later.
 */
export class RateLimitMetrics {
  private allowedTotal = 0;
  private rejectedTotal = 0;
  private storeFailovers = 0;
  private readonly rejectedByTier: Record<string, number> = {};
  private readonly rejectedByEndpoint: Record<string, number> = {};

  recordAllowed(): void {
    this.allowedTotal += 1;
  }

  recordRejected(tier: string, endpoint: string): void {
    this.rejectedTotal += 1;
    this.rejectedByTier[tier] = (this.rejectedByTier[tier] ?? 0) + 1;
    this.rejectedByEndpoint[endpoint] = (this.rejectedByEndpoint[endpoint] ?? 0) + 1;
  }

  recordStoreFailover(): void {
    this.storeFailovers += 1;
  }

  snapshot() {
    return {
      allowedTotal: this.allowedTotal,
      rejectedTotal: this.rejectedTotal,
      storeFailovers: this.storeFailovers,
      rejectedByTier: { ...this.rejectedByTier },
      rejectedByEndpoint: { ...this.rejectedByEndpoint },
    };
  }
}
