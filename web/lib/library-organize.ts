/**
 * 整理（organize）目标库选择的纯函数：可选目标 = 当前库 + 同类型、有根目录的其他库；
 * 跨库时把预览算出的目标路径从源库根换到目标库首个根，并带上 `to_library_id`
 * （后端只在条目显式指定目标库时才走跨库持久改挂）。
 *
 * 只允许 `import type`，以便 node --test 直接执行。
 */
import type { MediaLibrary } from "./api/libraries";

type OrganizeLibrary = Pick<MediaLibrary, "id" | "kind" | "root_paths">;

export interface OrganizeRenameRequest {
  from: string;
  to: string;
  to_library_id?: string;
}

export function organizeTargets<L extends OrganizeLibrary>(current: L, all: readonly L[]): L[] {
  return all.filter(
    (lib) => lib.id === current.id || (lib.kind === current.kind && lib.root_paths.length > 0),
  );
}

function trimSlash(path: string): string {
  return path.length > 1 ? path.replace(/\/+$/, "") : path;
}

function underRoot(path: string, root: string): string | null {
  const r = trimSlash(root);
  if (path === r) return "";
  return path.startsWith(`${r}/`) ? path.slice(r.length) : null;
}

export function buildOrganizeRenames(
  renames: readonly { from: string; to: string }[],
  current: OrganizeLibrary,
  target: OrganizeLibrary,
): OrganizeRenameRequest[] {
  if (target.id === current.id) {
    return renames.map(({ from, to }) => ({ from, to }));
  }
  const destRoot = target.root_paths[0];
  if (!destRoot) return [];
  const out: OrganizeRenameRequest[] = [];
  for (const { from, to } of renames) {
    const rest = current.root_paths
      .map((root) => underRoot(to, root))
      .filter((r): r is string => r != null)
      .sort((a, b) => a.length - b.length)[0];
    if (rest == null) continue;
    out.push({ from, to: `${trimSlash(destRoot)}${rest}`, to_library_id: target.id });
  }
  return out;
}
