# Review follow-up remediation

Scope: follow-up to `962564c`, based on `fbc6c13`. No artwork/authentication, Web credential, Browser capability, schema, or unrelated watcher changes.

## Corrected behavior

- Wash-cut with no ladder rejects score-only replacement when owned Release quality is absent. Configured ladder dimensions must be known on both sides, including the both-unknown case. Cutoff and ladder use one resolver, with stored/probed dimensions taking precedence and filename fallback filling absent dimensions. Existing known-quality score upgrades remain supported.
- Subscribe facts are scoped to a unique strict selected Library owner, even for missing files. Foreign, external, ambiguous and symlink-escaped paths do not count as owned. Same-target deletion facts remain, so deletion does not authorize automatic restoration. Store lookup failures propagate with structured context; returned quality only belongs to retained facts. No persisted-fact schema changes.
- Check-in normalizes English markers and response text consistently. Inline markup stays attached to its containing statement. Split negative/conditional and explicitly instructional text cannot confirm; detectable hidden/script content is excluded; failure takes precedence. Confirmation is a complete marker, optional punctuation, or a bounded numeric Chinese reward suffix. Unrelated footer/navigation and standalone help links do not invalidate an explicit result.

## Regression evidence

- Subscribe red: absent-quality score-only and both-unknown source ladder tests failed on the prior implementation.
- Library red: initial public Store suite had 7 failing cases. Final 11-case suite covers missing-path retarget, external/relative/ambiguous ownership, nested roots, same-target deletion, root/DB errors, symlink escape and quality isolation.
- Hooks red: English confirmation, split negatives/rewards, unrelated footer/inline-help and block-split English conditional cases reproduced defects before their respective fixes.
- Existing legal-upgrade fixtures now supply actual known Release quality; existing ledger failure/parity fixtures use the selected Library root. Their original cleanup, submission, score and rollback assertions remain intact.

## Boundaries

The default NexusPHP policy is intentionally conservative. It does not evaluate computed stylesheet visibility and cannot distinguish arbitrary unlabeled prose from a genuine result in every possible Site page. Existing injectable profile-specific response policies remain available. Score-only mode retains configured scoring semantics for known owned quality; it is not silently replaced with an implicit UpgradeLadder.

The previously noted oversized Playback view handlers and native-time ABI portability are outside this follow-up functional closure and remain unchanged.
