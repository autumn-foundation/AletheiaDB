/**
 * A temporal view over an {@link AletheiaClient} that injects bi-temporal
 * `as_of_*` coordinates into supporting reads (Issue #3225 / #3236).
 *
 * Obtained via {@link AletheiaClient.asOf}. Each dimension is independent: a
 * view scoped with only `validTime` sends only `as_of_valid_time`; only
 * `transactionTime` sends only `as_of_transaction_time`; both sends both.
 *
 * @packageDocumentation
 */

import type { AletheiaClient, AsOfCoordinates } from './client.js';
import type {
  FindNodesAtTimeRequest,
  NodeListResponse,
  TraverseOptions,
  TraverseResponse,
} from './types.js';

/** A read view pinned to bi-temporal coordinates. */
export class TemporalView {
  private readonly client: AletheiaClient;
  private readonly coords: AsOfCoordinates;

  constructor(client: AletheiaClient, coords: AsOfCoordinates) {
    this.client = client;
    this.coords = coords;
  }

  /** The coordinates this view injects. */
  coordinates(): AsOfCoordinates {
    return { ...this.coords };
  }

  /**
   * `GET /traverse` scoped to this view's coordinates — injects
   * `as_of_valid_time` / `as_of_transaction_time` (each independently). Explicit
   * `asOfValidTime` / `asOfTransactionTime` on `opts` take precedence.
   */
  traverse(opts: TraverseOptions): Promise<TraverseResponse> {
    const merged: TraverseOptions = { ...opts };
    if (this.coords.validTime !== undefined && merged.asOfValidTime === undefined) {
      merged.asOfValidTime = this.coords.validTime;
    }
    if (this.coords.transactionTime !== undefined && merged.asOfTransactionTime === undefined) {
      merged.asOfTransactionTime = this.coords.transactionTime;
    }
    return this.client.traverse(merged);
  }

  /**
   * `POST /nodes/find_at_time` scoped to this view — maps `validTime` /
   * `transactionTime` from the view onto the request's `validTime` /
   * `transactionTime` (Issue #3236). Explicit values on `req` take precedence.
   */
  findNodesAtTime(req: FindNodesAtTimeRequest): Promise<NodeListResponse> {
    const merged: FindNodesAtTimeRequest = { ...req };
    if (this.coords.validTime !== undefined && merged.validTime === undefined) {
      merged.validTime = this.coords.validTime;
    }
    if (this.coords.transactionTime !== undefined && merged.transactionTime === undefined) {
      merged.transactionTime = this.coords.transactionTime;
    }
    return this.client.findNodesAtTime(merged);
  }
}
