# AGENTS.md

Primary instruction set for agents in this repository. Local docs beat training data.

## How we develop

During the project's initial phase, the user's request is the unit of work. Do not create, require, assign, comment on, or close GitHub issues unless the user explicitly asks to track work there. Do not hunt for skills unless this file tells you to.

1. Implement the user's current request only. Do not expand it into unrelated work.
2. Read **only** the relevant parts of `CONTEXT.md`, ADRs that apply to the request, this file, and `docs/agents/roadmap.md` if the request cites it. If the user explicitly provides an issue, also read that issue body and its comments. Vocabulary in `CONTEXT.md` is mandatory (`_Avoid_` means do not use that word).
3. Tests first at the behavior's public seam (red → green). See **Testing**.
4. Run `cargo test -p <crate>` for every crate touched, then `cargo test --workspace` once at the end of the request.
5. Commit on the current branch after the change is green. Do not include unrelated user changes in the commit.
6. Wrap up with what changed, the user-visible effect, and verification results.
7. **Subagents**: use them for parallel research, file splits, or independent slices when appropriate. The parent still owns the request, the commit, and the wrap-up.

### Skills (only when stuck)

You do not need to search the skill catalog. This file is enough for task work.

- `/tdd` — what a good test is (behavior at a public seam, not internals).
- `/code-review` — after the requested change is green, before commit, if the diff is large.
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

Already over the hard limit (split when you next touch them — do not ignore):

- `crates/subscribe/tests/run.rs`

### Shape

- One module, one reason to change. Prefer `media/src/tmdb/{search.rs,details.rs,cache.rs,client.rs}` over a single `tmdb.rs`.
- Public API stays small: structs and traits a sibling crate actually calls. Keep parse/SQL/HTTP mapping private.
- `thiserror` for crate errors. No `unwrap` / `expect` on IO or parse paths except tests.
- No `unsafe` without a nearby code comment explaining why.
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
- Spec / current state: the user's request and `CONTEXT.md`; read an issue only when the user explicitly asks to use GitHub issue tracking
- ADRs: `docs/adr/`
- Tracker: `docs/agents/issue-tracker.md`
