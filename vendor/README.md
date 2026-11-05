# Bundled SQLite initialization repair

`libsqlite3-sys-0.35.0/` is the complete published crate, extracted from
`libsqlite3-sys-0.35.0.crate` after verifying SHA256
`133c182a6a2c87864fe97778797e46c7e999672690dc9fa3ee8e241aa4a9c13f`
against the original Cargo.lock registry checksum. Its package VCS commit is
`44e0ef965580b94d59c5dfe8874b57ab5993a8f7`. The upstream MIT license is retained.

The only change to the published package is six added lines in
`sqlite3/sqlite3.c`, backporting the mutex initialization read barrier from
[SQLite commit a6f89b9d](https://github.com/sqlite/sqlite/commit/a6f89b9d0b5be75f5cdd2d6c8e6bdc6c37d092ad)
(2026-10-20; Fossil
`37b83ad2f5245217a4a4626916c3974acb3491fc5d45ed082ac0386e9d84b4ce`).
It prevents a thread from observing the published `xMutexAlloc` pointer before
the other mutex methods on processors with weak memory ordering. An actual
Apple Silicon test-process SIGSEGV called a null `xMutexInit` during concurrent
first connection opens. The write-side barrier was already present; this
upstream change adds its missing read-side partner.

The workspace patch keeps rusqlite 0.37.0, SQLite 3.50.2, bindings, build script,
features and query planner unchanged. Its generated and third-party Rust files
remain outside the first-party workspace and source-size rules. The published
SQLCipher and platform inputs are preserved, but this fix applies only to the
normal bundled SQLite engine selected by this workspace. System SQLite and
SQLCipher are outside this verification scope.

As checked on 2026-11-05, the latest released SQLite 3.54.1 predates this fix;
the released rusqlite 0.41.1 bundle also lacks it. Remove this path patch when
a released dependency is verified to include the same read barrier, retaining
the fresh-process concurrency regression. Do not patch Cargo registry caches
or depend on an unpinned upstream branch.

The new bounded subprocess regression may pass before the patch because the
race is rare. The original crash is retained as failure evidence; passing
stress iterations alone do not prove its absence. Verification also checks the
new ARM binary for a barrier before the `xMutexInit` load/call.
