import { request } from "@/lib/http";
export interface MetadataLanguageOperation {
  library_id: string;
  operation_id: string;
  state: string;
  error: string | null;
}
export interface MetadataLanguageState {
  language: string;
  revision: number;
  operations: MetadataLanguageOperation[];
}
type Envelope<T> = { ok: boolean; data: T };
export async function getMetadataLanguage(): Promise<MetadataLanguageState> {
  return (await request<Envelope<MetadataLanguageState>>("/settings/metadata-language")).data;
}
export async function saveMetadataLanguage(expected_revision: number, language: string): Promise<MetadataLanguageState> {
  return (await request<Envelope<MetadataLanguageState>>("/settings/metadata-language", {
    method: "PATCH", body: JSON.stringify({ expected_revision, language }),
  })).data;
}
export async function retryMetadataLanguageOperation(operation: MetadataLanguageOperation): Promise<void> {
  await request(`/libraries/${encodeURIComponent(operation.library_id)}/operations/${encodeURIComponent(operation.operation_id)}/retry`, { method: "POST" });
}
