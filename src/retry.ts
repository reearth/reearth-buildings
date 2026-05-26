// Single-retry wrapper for subrequests. The Workers runtime exposes
// upstream flakiness as either a thrown "Network connection lost." (or
// similar) or as a 5xx response. Both are typically transient — one
// retry with a short backoff converts them into a recovered success
// instead of bubbling out as a 500.

const TRANSIENT_STATUS = new Set([408, 425, 429, 500, 502, 503, 504]);

function isTransientError(err: unknown): boolean {
  const msg = err instanceof Error ? err.message : String(err);
  return /network|connection|reset|timeout|fetch failed/i.test(msg);
}

export async function fetchWithRetry(
  input: RequestInfo,
  init?: RequestInit,
  backoffMs = 120,
): Promise<Response> {
  try {
    const resp = await fetch(input, init);
    if (TRANSIENT_STATUS.has(resp.status)) {
      await sleep(backoffMs);
      return fetch(input, init);
    }
    return resp;
  } catch (err) {
    if (!isTransientError(err)) throw err;
    await sleep(backoffMs);
    return fetch(input, init);
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
