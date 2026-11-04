import type { Enhancement, Media, MediaSequenceItem, Project, TopicMarker } from "./types";

const BASE = "http://127.0.0.1:8787";

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`${BASE}${path}`, init);
  if (!response.ok) {
    const body = await response.json().catch(() => null) as { detail?: string } | null;
    throw new Error(body?.detail ?? `Falha na API (${response.status})`);
  }
  return response.json() as Promise<T>;
}

export const api = {
  base: BASE,
  health: () => request<{ ok: boolean; transcription_available: boolean }>("/api/health"),
  importMedia: async (file: File) => {
    const data = new FormData();
    data.append("file", file);
    return request<Media>("/api/media/import", { method: "POST", body: data });
  },
  createProject: (mediaId: string) => request<Project>("/api/projects", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ media_id: mediaId }),
  }),
  getMedia: (id: string) => request<Media>(`/api/media/${id}`),
  getProject: (id: string) => request<Project>(`/api/projects/${id}`),
  getMediaSequence: (id: string) => request<MediaSequenceItem[]>(`/api/projects/${id}/media-sequence`),
  addMediaToSequence: (id: string, mediaId: string) => request<MediaSequenceItem[]>(`/api/projects/${id}/media-sequence`, {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ media_id: mediaId }),
  }),
  reorderMediaSequence: (id: string, projectIds: string[]) => request<MediaSequenceItem[]>(`/api/projects/${id}/media-sequence`, {
    method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ project_ids: projectIds }),
  }),
  listProjects: () => request<Array<Pick<Project, "id" | "name" | "media_id">>>("/api/projects"),
  renameProject: (id: string, name: string) => request<Project>(`/api/projects/${id}`, {
    method: "PATCH", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ name }),
  }),
  deleteProject: (id: string) => request<{ deleted: boolean }>(`/api/projects/${id}`, { method: "DELETE" }),
  cut: (id: string, start: number, end: number) => request<Project>(`/api/projects/${id}/cuts`, {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ start, end }),
  }),
  paste: (id: string, intervals: Array<{ start: number; end: number }>, target: number) => request<Project>(`/api/projects/${id}/paste`, {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ intervals, target }),
  }),
  deleteWords: (id: string, wordIds: string[]) => request<Project>(`/api/projects/${id}/delete-words`, {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ word_ids: wordIds }),
  }),
  correctWord: (id: string, wordId: string, text: string) => request<Project>(`/api/projects/${id}/words/${wordId}`, {
    method: "PATCH", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ text }),
  }),
  saveTopicMarkers: (id: string, topicMarkers: TopicMarker[]) => request<Project>(`/api/projects/${id}/topic-markers`, {
    method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ topic_markers: topicMarkers }),
  }),
  history: (id: string, action: "undo" | "redo") => request<Project>(`/api/projects/${id}/${action}`, { method: "POST" }),
  transcribe: (id: string, options: { provider: "local" | "deepgram" | "assemblyai"; model?: string; api_key?: string }) => request<{ id: string; status: string }>(`/api/projects/${id}/transcribe`, {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(options),
  }),
  job: (id: string) => request<{ id: string; status: string; progress: number; message?: string; error?: string; result?: Record<string, string> }>(`/api/jobs/${id}`),
  cancelJob: (id: string) => request<{ id: string; status: string }>(`/api/jobs/${id}/cancel`, { method: "POST" }),
  peaks: (mediaId: string) => request<{ buckets: Array<{ min: number; max: number }> }>(`/api/media/${mediaId}/peaks`),
  presets: () => request<Record<string, Enhancement>>("/api/enhance/presets"),
  savePreset: (name: string, params: Enhancement) => request<Record<string, Enhancement>>("/api/enhance/presets", {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ name, params }),
  }),
  saveEnhancement: (id: string, params: Enhancement) => request<Project>(`/api/projects/${id}/enhancement`, {
    method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(params),
  }),
  enhancementPreview: (id: string, params: Enhancement, seconds: number) => request<{ id?: string; status?: string; url?: string; cached?: boolean }>(`/api/projects/${id}/enhance-preview`, {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ params, seconds }),
  }),
  export: (id: string, format: string, quality: number, enhancement?: Enhancement) => request<{ id: string; status: string }>(`/api/projects/${id}/export`, {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ format, quality, enhancement }),
  }),
};
