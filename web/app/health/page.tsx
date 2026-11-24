"use client";

import { useEffect, useState } from "react";

import { getHealth, type HealthResponse } from "@/lib/api/health";
import { HttpError } from "@/lib/http";
import { usePageTitle } from "@/lib/use-page-title";

type LoadState =
  | { status: "loading"; data: null; error: null }
  | { status: "success"; data: HealthResponse; error: null }
  | { status: "error"; data: null; error: string };

function toErrorMessage(error: unknown): string {
  if (error instanceof HttpError) {
    return `${error.message} (HTTP ${error.status})`;
  }

  if (error instanceof Error) {
    return error.message;
  }

  return "Unknown error";
}

export default function HealthPage() {
  usePageTitle("服务状态");
  const [state, setState] = useState<LoadState>({
    status: "loading",
    data: null,
    error: null,
  });

  useEffect(() => {
    const controller = new AbortController();

    async function loadHealth() {
      setState({
        status: "loading",
        data: null,
        error: null,
      });

      try {
        const data = await getHealth({ signal: controller.signal });
        setState({
          status: "success",
          data,
          error: null,
        });
      } catch (error) {
        if (controller.signal.aborted) {
          return;
        }

        setState({
          status: "error",
          data: null,
          error: toErrorMessage(error),
        });
      }
    }

    void loadHealth();

    return () => {
      controller.abort();
    };
  }, []);

  return (
    <div className="grid gap-6 lg:grid-cols-[0.78fr_1.22fr]">
      <section className="css-glass rounded-[32px] p-8 text-[var(--text)]">
        <p className="text-sub font-semibold tracking-[0.24em] text-[var(--accent)] uppercase">
          Live contract check
        </p>
        <h1 className="mt-5 text-3xl font-semibold">Backend health status</h1>
        <p className="mt-4 max-w-xl text-body leading-7 text-[var(--text-muted)]">
          This page validates the base frontend contract: the web console talks to FastAPI through
          the shared client layer instead of calling ad hoc endpoints from the UI.
        </p>
        <div className="card-row-surface mt-8 rounded-[24px] p-5">
          <p className="text-body text-[var(--text-muted)]">State</p>
          <p className="mt-2 text-2xl font-semibold">
            {state.status === "loading" && "Loading"}
            {state.status === "success" && "Healthy"}
            {state.status === "error" && "Unavailable"}
          </p>
        </div>
      </section>

      <section className="css-glass rounded-[32px] p-8 text-[var(--text)]">
        {state.status === "loading" ? (
          <div className="space-y-4">
            <div className="skeleton-block h-4 w-24 rounded-full" />
            <div className="skeleton-block h-16 w-full rounded-[24px]" />
            <div className="grid gap-3 sm:grid-cols-3">
              <div className="skeleton-block h-24 rounded-[24px]" />
              <div className="skeleton-block h-24 rounded-[24px]" />
              <div className="skeleton-block h-24 rounded-[24px]" />
            </div>
          </div>
        ) : null}

        {state.status === "error" ? (
          <div className="rounded-[24px] border border-[var(--danger)]/25 bg-[var(--danger)]/10 p-6">
            <p className="text-body font-semibold uppercase tracking-[0.18em] text-[var(--danger)]">
              Request failed
            </p>
            <p className="mt-3 text-body leading-7 text-[var(--text)]">{state.error}</p>
            <p className="mt-4 text-body text-[var(--text-muted)]">
              Check whether FastAPI is running on port `8000`, or override the frontend env if the
              API lives elsewhere.
            </p>
          </div>
        ) : null}

        {state.status === "success" ? (
          <div className="space-y-5">
            <div className="flex items-center gap-3">
              <span className="inline-flex rounded-full bg-[var(--ok)]/15 px-3 py-1 text-body font-semibold text-[var(--ok)]">
                {state.data.status}
              </span>
              <span className="text-body text-[var(--text-muted)]">
                Backend contract from `/api/v1/health`
              </span>
            </div>

            <div className="grid gap-4 sm:grid-cols-3">
              <article className="card-row-surface rounded-[24px] p-5">
                <p className="text-body text-[var(--text-muted)]">Version</p>
                <p className="mt-2 text-xl font-semibold text-[var(--text)]">
                  {state.data.version ?? "—"}
                  {state.data.commit ? (
                    <span className="ml-2 font-mono text-sm text-[var(--text-muted)]">
                      {state.data.commit.slice(0, 8)}
                    </span>
                  ) : null}
                </p>
              </article>
              <article className="card-row-surface rounded-[24px] p-5">
                <p className="text-body text-[var(--text-muted)]">Environment</p>
                <p className="mt-2 text-xl font-semibold text-[var(--text)]">{state.data.environment}</p>
              </article>
              <article className="card-row-surface rounded-[24px] p-5">
                <p className="text-body text-[var(--text-muted)]">Protocol</p>
                <p className="mt-2 text-xl font-semibold text-[var(--text)]">HTTP JSON</p>
              </article>
            </div>
          </div>
        ) : null}
      </section>
    </div>
  );
}
