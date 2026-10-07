import { request } from "@/lib/http";

interface ApiEnvelope<T> {
  ok: boolean;
  data: T;
}

export interface FsEntry {
  name: string;
  path: string;
}

export interface FsBrowse {
  path: string;
  parent: string | null;
  entries: FsEntry[];
}

/** 浏览服务器目录（目录选择器数据源）；path 缺省为根目录。 */
export async function browseFs(path?: string): Promise<FsBrowse> {
  const qs = path ? `?path=${encodeURIComponent(path)}` : "";
  const resp = await request<ApiEnvelope<FsBrowse>>(`/fs/browse${qs}`);
  return resp.data;
}
