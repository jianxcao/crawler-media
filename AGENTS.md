# AGENTS.md

Primary instruction set for agents in this repository. Local docs beat training data.

## How we develop

One GitHub issue at a time as the unit of work. A session MAY continue to the next unblocked ticket after the current one is committed, commented, and closed. Do not hunt for skills unless this file tells you to.

1. Pick the frontier ticket: open, `ready-for-agent`, no open GitHub **blocked-by**. Start at the lowest number if several are unblocked.
2. Read **only**: the issue body, `CONTEXT.md`, ADRs named by the issue, this file, `docs/agents/roadmap.md` if the issue cites it. Vocabulary in `CONTEXT.md` is mandatory (`_Avoid_` means do not use that word).
3. Implement **that issue only** until it is green. Do not start a blocked child. Do not mix two issues in one commit.
4. Tests first at the issue’s seams (red → green). See **Testing**.
5. `cargo test -p <crate>` for every crate you touched, then `cargo test --workspace` once at the end of that issue.
6. Comment on the issue with what landed, close it, commit on the current branch. Do not close parent `#1` unless the user accepts the slice.
7. **Wrap-up before the next ticket**: say what landed (commit, tests, user-visible change) and what the new frontier is. Then pick the next unblocked lowest number.
8. **Subagents**: use them for parallel research, file splits, or independent slices inside one ticket. The parent still owns the issue, the commit, and the wrap-up.

`#1` is the spec, not a build ticket. Wave 1 (`#2`–`#7`) is the download loop. Wave 2 (`#8`–`#19`) is the rest. Native blocked-by on GitHub is the gate.

Do not start a second issue until the current one is pushed and closed.

### Skills (only when stuck)

You do not need to search the skill catalog. This file is enough for ticket work.

- `/tdd` — what a good test is (behavior at a public seam, not internals).
- `/code-review` — after the ticket is green, before you push, if the diff is large.
- `/domain-modeling` — only when you must add or change a glossary term or ADR.

## Testing

Each crate must be testable **alone**: `cargo test -p media` must not need qBittorrent, a real TMDB network, or Chromium.

- Put integration tests in `crates/<name>/tests/`. Unit tests next to the module they cover (`#[cfg(test)]` in the same file or `src/.../tests`).
- HTTP, TMDB, qBittorrent, CDP: inject a trait (`Fetcher`, catalog client, `Downloader`, `MediaProbe`). Tests supply fixtures / fakes. No live network in `cargo test`.
- Example for `#10` (`media` crate): one test per TMDB method you expose (search movie, search TV, details), plus cache hit / miss / stale against a fake HTTP backend and a temp cache dir. Do not defer cache tests to the API crate.
- Goldens (HTML/JSON/XML bytes in `crates/<name>/tests/fixtures/`) for parsers.
- Assert observable behavior: returned **Torrent**, **Subscribe** facts, ledger row, cache file contents. Do not assert private helpers, SQL text, or mock call counts unless the bug is “this outbound request is wrong.”
- Do not start Chromium in tests. `render` uses injected HTML.

## Rust conventions

Workspace: `crates/<bounded-context>`. `domain` is types only — no IO.
`store` is SQLite (ADR-0008). It may depend on `domain` and `library`.
Other crates may depend on `domain` and `store`; they must not depend on `api`.

### Size

These are hard limits, not vibes:

| Thing | Soft | Hard | What to do |
|---|---|---|---|
| `.rs` file | 200 lines | 800 | Split by type or use-case into sibling modules. `lib.rs` re-exports. |
| Production function / method | 60 lines | 120 | Split by responsibility into named helpers when needed. |
| Test function | 80 lines | 120 | Extract coherent fixture setup or behavior steps when needed. |
| `lib.rs` | 150 lines | 800 | `lib.rs` is module tree + public re-exports only. |

Count `wc -l`. Generated code and fixture bytes in tests are exempt; test **logic** is not.

- Soft limits are review prompts, not mandatory split points. The 120-line hard limit is a ceiling, not a target.
- Split by responsibility, not merely to reduce line counts. A function with multiple independent responsibilities should be split even below the hard limit; avoid helpers that only forward large parameter lists.
- Size compliance and functional correctness are separate acceptance criteria. Meeting these limits does not establish deletion safety, state compatibility, or retry correctness.

Already over the hard limit (split when you next touch them, or file a follow-up on that crate’s issue — do not ignore):

- `crates/subscribe/tests/run.rs`

### Shape

- One module, one reason to change. Prefer `media/src/tmdb/{search.rs,details.rs,cache.rs,client.rs}` over a single `tmdb.rs`.
- Public API stays small: structs and traits a sibling crate actually calls. Keep parse/SQL/HTTP mapping private.
- `thiserror` for crate errors. No `unwrap` / `expect` on IO or parse paths except tests.
- No `unsafe` without an issue comment explaining why.
- Edition and versions: inherit `workspace.package` / `workspace.dependencies`.

### Naming

Use `CONTEXT.md` terms in type names (`Subscribe`, `Torrent`, `Release`, `Library` ledger, `Wash-cut` as `wash_cut` in code). Do not invent `SearchResult`, `MetaInfo`, `MediaMeta`.

### Observability & Logging Rules

Do not write silent code. All critical paths must emit structured tracing logs:
- **`error!`**: Any unexpected failure, network error, third-party timeout, or rejected operation with full error context.
- **`warn!`**: Recoverable degradation (e.g. falling back to secondary metadata source, token renewal required, download client retry).
- **`info!`**: Key business lifecycle events: job execution start/finish, subscribe search round result (keywords, candidate count, admitted torrents), download additions, transfer/hardlink/copy outcomes, catalog scrape & NFO generation, user logins.
- **`debug!`**: Parsing details, HTTP request/response latency, matched filter atoms, individual file probing.

### Static Artwork & Route Authentication Rules

**DO NOT require authentication on read-only static image endpoints.**
- Endpoints serving images consumed by native HTML tags (such as `<img>`, `<video poster="...">`, CSS backgrounds) — including `/posters/*`, `/fanart/*`, `/stills/*`, `/chapters/*`, and `/libraries/{id}/cover` — **MUST remain on the public router** (`public`).
- **Why**: Web frontends and third-party clients store auth in `Authorization: Bearer <token>` in localStorage, NOT in session cookies. Native `<img>` browser requests do not send custom `Authorization` headers. Placing image endpoints under `member` router causes instant 401 image breakage across all views.
- **Handler Authentication**: If library visibility or user scoping is optional for an image handler, inject `user_id: Option<axum::Extension<Option<domain::UserId>>>` to permit unauthenticated/anonymous `<img>` reads while still honoring user context when present.
- Jellyfin video streaming endpoints follow the Jellyfin protocol's authentication contract. Do not apply the static-image public-route rule to those endpoints.

## Docs map

- Glossary: `CONTEXT.md`
- API conventions (envelope + core DTOs, **not** the live route table): `docs/api-contracts/self.md`
- Live HTTP surface: `crates/api/src/http/mod.rs` (`/api/v1`) and `web/lib/api/*`
- Stage archive (not an API spec): `docs/agents/roadmap.md`
- Spec / current state for ticket work: issue `#1` plus the issue body
- ADRs: `docs/adr/`
- Tracker: `docs/agents/issue-tracker.md`
