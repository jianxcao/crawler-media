import type { RuleSetAtom } from "./api/subscriptions";

/** Keep exact stored atoms for unchanged dimensions, replace edited dimensions.
 * Unknown dimensions absent from both editor projections remain untouched.
 */
export function ruleSetEditAtoms(
  original: RuleSetAtom[],
  before: RuleSetAtom[],
  after: RuleSetAtom[],
): RuleSetAtom[] {
  const kinds = new Set([...before, ...after].map((atom) => atom.kind));
  const changed = new Set<string>();
  const signature = (atoms: RuleSetAtom[], kind: string) => JSON.stringify(
    atoms.filter((atom) => atom.kind === kind)
      .map((atom) => [atom.value ?? null, atom.exclude ?? false]),
  );
  for (const kind of kinds) {
    if (signature(before, kind) !== signature(after, kind)) changed.add(kind);
  }
  return [
    ...original.filter((atom) => !changed.has(atom.kind)),
    ...after.filter((atom) => changed.has(atom.kind)).map((atom) => {
      const unchanged = before.some((candidate) => candidate.kind === atom.kind
        && candidate.value === atom.value
        && (candidate.exclude ?? false) === (atom.exclude ?? false));
      if (!unchanged) return atom;
      const candidates = original.filter((candidate) => candidate.kind === atom.kind
        && candidate.value === atom.value);
      return candidates.find((candidate) => (candidate.exclude ?? false) === (atom.exclude ?? false))
        ?? candidates[0] ?? atom;
    }),
  ];
}
