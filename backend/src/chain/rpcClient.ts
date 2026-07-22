/**
 * Soroban RPC event source.
 *
 * Abstracts `getEvents` paging so the indexer only deals with a flat
 * stream of RpcEvent objects.  The real implementation calls the
 * Soroban JSON-RPC endpoint; tests inject a stub.
 */

export interface RpcEvent {
  contractId: string;
  /** The first topic string (e.g. event type tag). */
  topic: string;
  transactionHash: string;
  ledger: number;
  payload: Record<string, unknown>;
}

export interface GetEventsPage {
  events: RpcEvent[];
  /** Cursor to pass as `startLedger` for the next page, or undefined if done. */
  nextLedger: number | undefined;
}

/**
 * Minimal surface needed from a Soroban RPC endpoint.
 * Real: HTTP POST to RPC_URL.  Tests: in-memory stub.
 */
export interface SorobanRpcClient {
  /**
   * Fetch one page of events.
   *
   * @param contractId   Filter by contract address.
   * @param eventType    Topic filter string.
   * @param startLedger  Resume from this ledger (inclusive).
   * @param limit        Max events per page (default 100).
   */
  getEvents(
    contractId: string,
    eventType: string,
    startLedger: number,
    limit?: number,
  ): Promise<GetEventsPage>;

  /** Return the latest closed ledger sequence number. */
  getLatestLedger(): Promise<number>;
}

/**
 * Consume all pages for a given (contractId, eventType, fromLedger) query,
 * yielding batches of events until the RPC reports no more pages.
 *
 * Callers iterate with `for await`.
 */
export async function* paginateEvents(
  client: SorobanRpcClient,
  contractId: string,
  eventType: string,
  fromLedger: number,
  pageSize = 100,
): AsyncGenerator<RpcEvent[]> {
  let cursor: number | undefined = fromLedger;
  while (cursor !== undefined) {
    const page = await client.getEvents(contractId, eventType, cursor, pageSize);
    if (page.events.length > 0) {
      yield page.events;
    }
    cursor = page.nextLedger;
  }
}

// ── In-process stub used by tests ─────────────────────────────────────────

export interface StubPage {
  events: RpcEvent[];
  /** Set to a ledger number to simulate another page being available. */
  nextLedger?: number;
}

/**
 * Scriptable in-memory RPC client for integration tests.
 *
 * Feed pages with `addPage()` or `setLatestLedger()` before running.
 */
export class StubSorobanRpcClient implements SorobanRpcClient {
  private readonly pages: Map<string, StubPage[]> = new Map();
  private latestLedger = 0;

  setLatestLedger(ledger: number): this {
    this.latestLedger = ledger;
    return this;
  }

  /** Queue a page to be returned for (contractId, eventType, startLedger). */
  addPage(contractId: string, eventType: string, startLedger: number, page: StubPage): this {
    const key = `${contractId}:${eventType}:${startLedger}`;
    const list = this.pages.get(key) ?? [];
    list.push(page);
    this.pages.set(key, list);
    return this;
  }

  async getEvents(
    contractId: string,
    eventType: string,
    startLedger: number,
  ): Promise<GetEventsPage> {
    const key = `${contractId}:${eventType}:${startLedger}`;
    const list = this.pages.get(key);
    if (!list || list.length === 0) {
      return { events: [], nextLedger: undefined };
    }
    const page = list.shift()!;
    return { events: page.events, nextLedger: page.nextLedger };
  }

  async getLatestLedger(): Promise<number> {
    return this.latestLedger;
  }
}
