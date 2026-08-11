/**
 * Small guard for UI refreshes whose responses may complete out of order.
 * A newer request invalidates every older response.
 */
export function createLatestRequestGuard() {
  let generation = 0;

  return {
    begin(): number {
      generation += 1;
      return generation;
    },
    isCurrent(request: number): boolean {
      return request === generation;
    },
  };
}
