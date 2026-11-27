import * as React from "react";
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type ReactNode,
} from "react";
import {
  isSdkError,
  isTerminalOperation,
  type CommandMap,
  type Environment,
  type Operation,
  type OperationRef,
  type PluginConfigController,
  type PluginHostSDK,
  type QueryMap,
  type SdkError,
  type WriteOptions,
} from "./contracts";

interface PluginSurfaceContextValue {
  host: PluginHostSDK;
  surfaceId: string;
  config?: PluginConfigController;
}

const PluginSurfaceContext = createContext<PluginSurfaceContextValue | null>(
  null,
);

export interface PluginHostProviderProps {
  host: PluginHostSDK;
  surfaceId?: string;
  config?: PluginConfigController;
  children?: ReactNode;
}

/** Provides host SDK, active surfaceId, and optional config controller to nested plugin components. */
export function PluginHostProvider({
  host,
  surfaceId,
  config,
  children,
}: PluginHostProviderProps) {
  const resolvedSurfaceId = surfaceId ?? host.context.surface_id;
  const value = useMemo<PluginSurfaceContextValue>(
    () => ({ host, surfaceId: resolvedSurfaceId, config }),
    [host, resolvedSurfaceId, config],
  );
  return (
    <PluginSurfaceContext.Provider value={value}>
      {children}
    </PluginSurfaceContext.Provider>
  );
}

/** Resolves PluginHostSDK either from explicit prop or enclosing PluginHostProvider context. */
export function useHostSDK(explicitHost?: PluginHostSDK): PluginHostSDK {
  const ctx = useContext(PluginSurfaceContext);
  const host = explicitHost ?? ctx?.host;
  if (!host) {
    throw new Error(
      "useHostSDK requires either a host argument or an enclosing <PluginHostProvider>",
    );
  }
  return host;
}

/** Returns host environment (theme/language/layout/user); host must return stable object until changed. */
export function usePluginEnvironment(explicitHost?: PluginHostSDK): Environment {
  const host = useHostSDK(explicitHost);
  const subscribe = useCallback(
    (listener: () => void) => host.environment.subscribe(listener),
    [host],
  );
  const snapshot = useCallback(() => host.environment.get(), [host]);
  return useSyncExternalStore(subscribe, snapshot, snapshot);
}

/** Returns the active host theme id (e.g. "silver", "paper", "netflix"). */
export function usePluginTheme(explicitHost?: PluginHostSDK): string {
  return usePluginEnvironment(explicitHost).theme;
}

/** Returns the config controller passed to PluginHostProvider (or explicit fallback). */
export function usePluginConfig(
  explicitConfig?: PluginConfigController,
): PluginConfigController | undefined {
  const ctx = useContext(PluginSurfaceContext);
  return explicitConfig ?? ctx?.config;
}

function toSdkError(err: unknown): SdkError {
  if (isSdkError(err)) {
    return err;
  }
  return {
    code: "request-failed",
    message: err instanceof Error ? err.message : String(err),
  };
}

export interface UsePluginQueryOptions {
  host?: PluginHostSDK;
  enabled?: boolean;
}

export interface UsePluginQueryResult<T> {
  data: T | undefined;
  loading: boolean;
  error: SdkError | null;
  reload: () => Promise<void>;
}

/** Executes a bounded host UI query (`sdk.query`) with cancellation-safe reloading. */
export function usePluginQuery<K extends keyof QueryMap>(
  name: K,
  input: QueryMap[K]["input"],
  options?: UsePluginQueryOptions,
): UsePluginQueryResult<QueryMap[K]["result"]> {
  const host = useHostSDK(options?.host);
  const enabled = options?.enabled ?? true;
  const serializedInput = JSON.stringify(input);
  const stableInputRef = useRef(input);
  stableInputRef.current = input;

  const [data, setData] = useState<QueryMap[K]["result"] | undefined>(
    undefined,
  );
  const [loading, setLoading] = useState<boolean>(enabled);
  const [error, setError] = useState<SdkError | null>(null);
  const seqRef = useRef(0);

  const run = useCallback(async () => {
    const seq = ++seqRef.current;
    setLoading(true);
    setError(null);
    try {
      const result = await host.query(name, stableInputRef.current);
      if (seq === seqRef.current) {
        setData(result);
        setLoading(false);
      }
    } catch (err) {
      if (seq === seqRef.current) {
        setError(toSdkError(err));
        setLoading(false);
      }
    }
  }, [host, name, serializedInput]);

  useEffect(() => {
    if (!enabled) {
      seqRef.current += 1;
      setLoading(false);
      return;
    }
    void run();
    return () => {
      seqRef.current += 1;
    };
  }, [enabled, run]);

  return { data, loading, error, reload: run };
}

export interface UsePluginCommandResult<K extends keyof CommandMap> {
  submit: (
    payload: CommandMap[K],
    options: WriteOptions,
  ) => Promise<OperationRef>;
  pending: boolean;
  error: SdkError | null;
  lastOperation: OperationRef | null;
  reset: () => void;
}

/** Submits a declared host command (`sdk.commands.submit`) while tracking inflight state and errors. */
export function usePluginCommand<K extends keyof CommandMap>(
  kind: K,
  explicitHost?: PluginHostSDK,
): UsePluginCommandResult<K> {
  const host = useHostSDK(explicitHost);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<SdkError | null>(null);
  const [lastOperation, setLastOperation] = useState<OperationRef | null>(null);
  const inflightRef = useRef(0);
  const seqRef = useRef(0);
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const reset = useCallback(() => {
    seqRef.current += 1;
    setError(null);
    setLastOperation(null);
  }, []);

  const submit = useCallback(
    async (payload: CommandMap[K], writeOptions: WriteOptions) => {
      const seq = ++seqRef.current;
      inflightRef.current += 1;
      setPending(true);
      setError(null);
      try {
        const op = await host.commands.submit(kind, payload, writeOptions);
        if (mountedRef.current && seq === seqRef.current) {
          setLastOperation(op);
        }
        return op;
      } catch (err) {
        const sdkErr = toSdkError(err);
        if (mountedRef.current && seq === seqRef.current) {
          setError(sdkErr);
        }
        throw sdkErr;
      } finally {
        inflightRef.current = Math.max(0, inflightRef.current - 1);
        if (mountedRef.current && inflightRef.current === 0) {
          setPending(false);
        }
      }
    },
    [host, kind],
  );

  return { submit, pending, error, lastOperation, reset };
}

/** Watches a host Operation ID via `sdk.operations.watch` until it reaches a terminal state. */
export function usePluginOperation(
  operationId: string | null | undefined,
  explicitHost?: PluginHostSDK,
): Operation | null {
  const host = useHostSDK(explicitHost);
  const [operation, setOperation] = useState<Operation | null>(null);

  useEffect(() => {
    if (!operationId) {
      setOperation(null);
      return;
    }
    setOperation((prev) => (prev?.operation_id === operationId ? prev : null));
    let disposed = false;
    let stopWatch: (() => void) | null = null;
    stopWatch = host.operations.watch(operationId, (next) => {
      if (disposed) {
        return;
      }
      setOperation(next);
      if (isTerminalOperation(next.state) && stopWatch) {
        stopWatch();
        stopWatch = null;
      }
    });
    return () => {
      disposed = true;
      if (stopWatch) {
        stopWatch();
      }
    };
  }, [host, operationId]);

  return operation && operation.operation_id === operationId ? operation : null;
}
