export function retryStartupTimeout<T>(
  operation: () => Promise<T>,
  isTimeout: (error: unknown) => boolean,
  delay?: (milliseconds: number) => Promise<void>,
): Promise<T>;
