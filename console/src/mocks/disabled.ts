/** Stands in for the mock worker in every build without API mocks (see next.config.ts). */
export function startMockWorker(): Promise<void> {
  return Promise.resolve();
}
