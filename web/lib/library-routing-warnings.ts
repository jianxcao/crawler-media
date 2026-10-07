import type { MatchRule, MediaLibrary } from "./api/libraries";

function valuesOverlap(a: MatchRule[], b: MatchRule[]): boolean {
  for (const ra of a) {
    for (const rb of b) {
      if (ra.field !== rb.field) continue;
      const set = new Set(ra.values.map(String));
      if (rb.values.some((v) => set.has(String(v)))) return true;
    }
  }
  return false;
}

/**
 * 同类型两库的收藏范围重叠提示。比较 `match_rules`；没有规则则不提示。
 */
export function routingOverlapWarnings(libraries: MediaLibrary[]): string[] {
  const warnings: string[] = [];
  for (let i = 0; i < libraries.length; i++) {
    for (let j = i + 1; j < libraries.length; j++) {
      const a = libraries[i];
      const b = libraries[j];
      if (a.kind !== b.kind) continue;
      const aRules = a.match_rules ?? [];
      const bRules = b.match_rules ?? [];
      if (aRules.length === 0 || bRules.length === 0) continue;
      const aKey = JSON.stringify(aRules);
      const bKey = JSON.stringify(bRules);
      if (aKey === bKey) {
        warnings.push(`「${a.name}」与「${b.name}」的收藏范围相同，自动入库可能落到任一库。`);
      } else if (valuesOverlap(aRules, bRules)) {
        warnings.push(`「${a.name}」与「${b.name}」的收藏范围有重叠，自动入库可能落到任一库。`);
      }
    }
  }
  return warnings;
}
