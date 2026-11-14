import type { RuleSetAtom } from "./api/subscriptions";

/** Preserve exact metadata for untouched allow/block groups and hidden atoms.
 * Edited groups use the editor's complete ranking so save/reload keeps its order.
 */
export function ruleSetEditAtoms(
  original: RuleSetAtom[],
  before: RuleSetAtom[],
  after: RuleSetAtom[],
): RuleSetAtom[] {
  const group = (atom: RuleSetAtom) => `${atom.kind}:${atom.exclude ?? false}`;
  const groups = new Set([...before, ...after].map(group));
  const signature = (atoms: RuleSetAtom[], key: string) => JSON.stringify(
    atoms.filter((atom) => group(atom) === key).map((atom) => [
      atom.value, atom.priority, atom.kind === "hr" ? atom.unknown : null,
    ]),
  );
  const changed = new Set([...groups].filter((key) => signature(before, key) !== signature(after, key)));
  const retained = original.filter((atom) => !changed.has(group(atom))
    || !before.some((candidate) => group(candidate) === group(atom) && candidate.value === atom.value));
  return [...retained, ...after.filter((atom) => changed.has(group(atom)))];
}

/** 按实际评分从高到低读取一个维度；黑名单单独读取。 */
export function ruleAtomValues(atoms: RuleSetAtom[], kind: RuleSetAtom["kind"], exclude = false): string[] {
  return atoms.filter((atom) => atom.kind === kind && !!atom.exclude === exclude && atom.value != null)
    .sort((a, b) => b.priority - a.priority).map((atom) => atom.value!);
}

export function ruleAtomValue(atoms: RuleSetAtom[], kind: RuleSetAtom["kind"]): string | undefined {
  return ruleAtomValues(atoms, kind)[0];
}
