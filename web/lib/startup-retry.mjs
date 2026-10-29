/** Retry one transient startup timeout after a short backoff. */
export async function retryStartupTimeout(operation, isTimeout, delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms))) {
  try {
    return await operation();
  } catch (error) {
    if (!isTimeout(error)) throw error;
    await delay(1000);
    return operation();
  }
}
