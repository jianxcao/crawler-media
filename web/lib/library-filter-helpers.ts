import type { FacetValue, LibraryFacets } from "./api/libraries/item-types.ts";
import type { LibraryFilter } from "./library-filter.ts";

/**
 * 条件行里的维度顺序与叫法。
 *
 * **十个维度一个都不能少。** 条件行的职责是「收起面板之后，用户仍然看得见
 * 自己筛了什么」；只画一级四维的话，用 4K 筛完再收起面板，界面上只剩一个
 * 「筛选 1」的角标和「筛出 8 部」——筛的是什么、怎么取消，都无从得知。
 */
export const FILTER_DIMS = [
  { key: "genres", label: "类型" },
  { key: "decades", label: "年代" },
  { key: "countries", label: "地区" },
  { key: "watch", label: "观看" },
  { key: "ratingGte", label: "评分" },
  { key: "runtimes", label: "片长" },
  { key: "languages", label: "语言" },
  { key: "resolutions", label: "画质" },
  { key: "hdr", label: "动态范围" },
  { key: "stock", label: "库存" },
] as const;

/** 多值维度（单值的 watch / ratingGte / hdr 走清成 null 那条路）。 */
export type MultiDim =
  | "genres"
  | "countries"
  | "decades"
  | "runtimes"
  | "languages"
  | "resolutions"
  | "stock";

/**
 * 取值本身就是人话的维度：facet 里它们的 label 等于 value（"2160p"、"ja"）。
 * 这类维度在展示名还没到的时候直接印取值就行，不必等。
 */
const SELF_LABELLING = new Set(["resolutions", "languages"]);

/**
 * 展示名查不到时印什么。
 *
 * **不能印裸值。** 类型存的是 TMDB id、地区存的是国家码、片长存的是档位键，
 * 界面上冒出「878」「JP」「gt120」比空着更糟——用户不知道那是什么，也就无法
 * 判断自己筛的对不对。查不到只有一种情况：展示名还在路上（服务端保证选中的
 * 取值一定在 facet 里），所以给个省略号占位，到了自然就补上。
 */
export function labelFallback(dim: string, value: string): string {
  return SELF_LABELLING.has(dim) ? value : "…";
}

/** 条件里有没有二级维度（决定要不要取全份 facet：二级的展示名只在全份里）。 */
export function hasSecondaryFilter(filter: LibraryFilter): boolean {
  return Boolean(
    filter.ratingGte != null ||
      filter.runtimes?.length ||
      filter.languages?.length ||
      filter.resolutions?.length ||
      filter.hdr != null ||
      filter.stock?.length,
  );
}

/** 维度 → 服务端 facet 里对应的候选表（取展示名用）。 */
export function poolOfFacet(
  key: string,
  facets: LibraryFacets | null,
): FacetValue[] | undefined {
  switch (key) {
    case "genres":
      return facets?.genres;
    case "countries":
      return facets?.countries;
    case "decades":
      return facets?.decades;
    case "watch":
      return facets?.watch;
    case "ratingGte":
      return facets?.ratings;
    case "runtimes":
      return facets?.runtimes;
    case "languages":
      return facets?.languages;
    case "resolutions":
      return facets?.resolutions;
    case "hdr":
      return facets?.hdr;
    case "stock":
      return facets?.stock;
    default:
      return undefined;
  }
}

/** 取值 → 展示名。查不到就给省略号，绝不把裸 id/代码印出来。 */
export function facetLabelOf(
  dim: string,
  value: string,
  facets: LibraryFacets | null,
): string {
  return (
    poolOfFacet(dim, facets)?.find((row) => row.value === value)?.label ??
    labelFallback(dim, value)
  );
}

/** 摘掉一个筛选取值。单值维度清成 null，多值维度只去掉这一个。 */
export function dropFilterValue(
  filter: LibraryFilter,
  dim: string,
  value: string,
): LibraryFilter {
  if (dim === "watch" || dim === "ratingGte" || dim === "rating_gte" || dim === "hdr") {
    const field = dim === "rating_gte" ? "ratingGte" : dim;
    return { ...filter, [field]: null };
  }
  const key = dim as MultiDim;
  const current = (filter[key] ?? []) as (string | number)[];
  return {
    ...filter,
    [key]: current.filter((v) => String(v) !== String(value)),
  };
}

export interface FilterConditionGroup {
  key: string;
  label: string;
  values: string[];
}

/** 按稳定维度顺序生成条件行要渲染的非空分组。 */
export function buildFilterConditionGroups(filter: LibraryFilter): FilterConditionGroup[] {
  return FILTER_DIMS.map(({ key, label }) => {
    let values: string[] = [];
    if (key === "watch") {
      values = filter.watch ? [filter.watch] : [];
    } else if (key === "ratingGte") {
      values = filter.ratingGte == null ? [] : [String(filter.ratingGte)];
    } else if (key === "hdr") {
      // facet 里 HDR/SDR 的取值是 "1"/"0"，与筛选条件的布尔值对上
      values = filter.hdr == null ? [] : [filter.hdr ? "1" : "0"];
    } else {
      values = ((filter[key] ?? []) as (string | number)[]).map(String);
    }
    return { key, label, values };
  }).filter((group) => group.values.length > 0);
}
