import { Fragment, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { PointerEvent, SyntheticEvent } from "react";
import {
  ArrowDownToLine, AudioLines, BookmarkPlus, Check, ChevronDown, ChevronUp, ClipboardPaste, Clock3, Copy, ExternalLink, FileAudio2, FileText, FileVideo2, FolderOpen,
  LoaderCircle, Mic2, Pause, Play, Plus, Redo2, Scissors, Sparkles,
  KeyRound, Moon, Pencil, Sun, Trash2, Undo2, Upload, VolumeX, WandSparkles, X, ZoomIn,
} from "lucide-react";
import { api } from "./api";
import type { Enhancement, Media, MediaSequenceItem, Peak, Project, TopicMarker, Word } from "./types";

const wait = (ms: number) => new Promise((resolve) => window.setTimeout(resolve, ms));
const initialEnhancement: Enhancement = {
  input_gain_db: 0, highpass_hz: 70, lowpass_hz: 18000, noise_reduction: 30,
  speech_gate: 0, deesser_mix: 0, presence_eq_db: 0, compressor_amount: 20,
  loudnorm_target: -16, output_gain_db: 0,
};

async function waitForJob(id: string, onProgress?: (progress: number, message?: string) => void) {
  for (;;) {
    const job = await api.job(id);
    onProgress?.(job.progress, job.message);
    if (job.status === "completed") return job;
    if (job.status === "failed") throw new Error(job.error ?? "A operação falhou.");
    if (job.status === "cancelled") throw new Error("Operação cancelada.");
    await wait(900);
  }
}

type TranscriptionProvider = "local" | "deepgram" | "assemblyai";

function TranscriptionProviderCards({ value, onChange }: { value: TranscriptionProvider; onChange: (value: TranscriptionProvider) => void }) {
  const providers: Array<{ id: TranscriptionProvider; name: string; detail: string }> = [
    { id: "local", name: "Local", detail: "Privado" },
    { id: "deepgram", name: "Deepgram", detail: "Nova-3" },
    { id: "assemblyai", name: "AssemblyAI", detail: "Universal" },
  ];
  return <div className="provider-cards" role="group" aria-label="Serviço de transcrição">
    {providers.map((provider) => <button
      key={provider.id}
      type="button"
      className="provider-card"
      aria-pressed={value === provider.id}
      onClick={() => onChange(provider.id)}
    ><strong>{provider.name}</strong><small>{provider.detail}</small></button>)}
  </div>;
}

function formatTime(seconds: number) {
  const safe = Math.max(0, Math.floor(seconds));
  return `${Math.floor(safe / 60).toString().padStart(2, "0")}:${(safe % 60).toString().padStart(2, "0")}`;
}

function formatPreciseTime(seconds: number) {
  const totalMilliseconds = Math.round(Math.max(0, seconds) * 1000);
  const whole = Math.floor(totalMilliseconds / 1000);
  const milliseconds = (totalMilliseconds % 1000).toString().padStart(3, "0");
  return `${Math.floor(whole / 60).toString().padStart(2, "0")}:${(whole % 60).toString().padStart(2, "0")}.${milliseconds}`;
}

function isWordKept(word: Word, project: Project) {
  return project.edl.kept.some((part) => word.start >= part.start && word.end <= part.end);
}

function getOrderedKeptWords(project: Project) {
  return project.transcript.filter((word) => isWordKept(word, project)).sort((left, right) => {
    const leftPart = project.edl.kept.findIndex((part) => left.start >= part.start && left.end <= part.end);
    const rightPart = project.edl.kept.findIndex((part) => right.start >= part.start && right.end <= part.end);
    return leftPart - rightPart || left.start - right.start;
  });
}

function editedTimeAt(originalTime: number, kept: Project["edl"]["kept"]) {
  let editedTime = 0;
  for (const part of kept) {
    if (originalTime <= part.start) return editedTime;
    if (originalTime < part.end) return editedTime + originalTime - part.start;
    editedTime += part.end - part.start;
  }
  return editedTime;
}

function originalTimeAt(editedTime: number, kept: Project["edl"]["kept"]) {
  let offset = 0;
  for (const part of kept) {
    const duration = part.end - part.start;
    if (editedTime <= offset + duration) return part.start + Math.max(0, editedTime - offset);
    offset += duration;
  }
  return kept.at(-1)?.end ?? 0;
}

function getTopicPositions(markers: TopicMarker[], words: Word[]) {
  const positions = new Map<number, TopicMarker[]>();
  for (const marker of markers) {
    const index = words.findIndex((word) => word.start >= marker.time);
    const position = index < 0 ? words.length : index;
    positions.set(position, [...(positions.get(position) ?? []), marker]);
  }
  return positions;
}

function getSilencePositions(words: Word[], kept: Project["edl"]["kept"]) {
  const positions = new Map<number, { start: number; end: number }>();
  for (let index = 1; index < words.length; index += 1) {
    const previous = words[index - 1];
    const current = words[index];
    const gap = current.start - previous.end;
    const sameKeptInterval = kept.some((part) => previous.start >= part.start
      && previous.end <= part.end && current.start >= part.start && current.end <= part.end);
    if (sameKeptInterval && gap >= 0.8) {
      positions.set(index, { start: previous.end, end: current.start });
    }
  }
  return positions;
}

function getSelectionIntervals(transcript: Word[], selectedIds: string[]) {
  const selected = new Set(selectedIds);
  const selectedWords = transcript.map((word, index) => ({ word, index })).filter(({ word }) => selected.has(word.id));
  const groups: Array<Array<{ word: Word; index: number }>> = [];
  for (const item of selectedWords) {
    const previous = groups.at(-1)?.at(-1);
    if (!previous || item.index > previous.index + 1) groups.push([item]);
    else groups.at(-1)!.push(item);
  }
  return groups.map((group) => {
    const first = group[0];
    const last = group.at(-1)!;
    const start = first.index > 0 ? Math.max(first.word.start, transcript[first.index - 1].end) : first.word.start;
    const end = last.index + 1 < transcript.length ? Math.min(last.word.end, transcript[last.index + 1].start) : last.word.end;
    return { start, end };
  }).filter((interval) => interval.end > interval.start);
}

export default function App() {
  const [project, setProject] = useState<Project | null>(null);
  const [media, setMedia] = useState<Media | null>(null);
  const [sequenceRootId, setSequenceRootId] = useState<string | null>(null);
  const [sequenceItems, setSequenceItems] = useState<MediaSequenceItem[]>([]);
  const [sequenceProjects, setSequenceProjects] = useState<Project[]>([]);
  const [projects, setProjects] = useState<Array<Pick<Project, "id" | "name" | "media_id">>>([]);
  const [peaks, setPeaks] = useState<Peak[]>([]);
  const [presets, setPresets] = useState<Record<string, Enhancement>>({});
  const [enhancement, setEnhancement] = useState<Enhancement>(initialEnhancement);
  const [enhancementEnabled, setEnhancementEnabled] = useState(false);
  const [previewSignature, setPreviewSignature] = useState("");
  const [failedPreviewSignature, setFailedPreviewSignature] = useState("");
  const [selected, setSelected] = useState<string[]>([]);
  const [cutClipboard, setCutClipboard] = useState<Array<{ start: number; end: number }>>([]);
  const [selectionPopup, setSelectionPopup] = useState<{ left: number; top: number } | null>(null);
  const [timelineFocus, setTimelineFocus] = useState<{ start: number; end: number } | null>(null);
  const [timelineCutRange, setTimelineCutRange] = useState<{ start: number; end: number } | null>(null);
  const [currentTime, setCurrentTime] = useState(0);
  const [previewVisible, setPreviewVisible] = useState(true);
  const [theme, setTheme] = useState<"dark" | "light">(() => (
    localStorage.getItem("podcast-studio-theme") === "light" ? "light" : "dark"
  ));
  const [openMenu, setOpenMenu] = useState<"enhance" | "export" | null>(null);
  const [workspaceView, setWorkspaceView] = useState<"transcript" | "timeline" | "settings">("transcript");
  const [playing, setPlaying] = useState(false);
  const [playbackRate, setPlaybackRate] = useState(1);
  const selectionAnchor = useRef<string | null>(null);
  const isDraggingSelection = useRef(false);
  const timelineDragStart = useRef<number | null>(null);
  const previewEnd = useRef<number | null>(null);
  const [busy, setBusy] = useState("");
  const [transcriptionTargetId, setTranscriptionTargetId] = useState("");
  const [activeJobId, setActiveJobId] = useState("");
  const [progress, setProgress] = useState(0);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const [transcriptionAvailable, setTranscriptionAvailable] = useState(false);
  const [transcriptionProvider, setTranscriptionProvider] = useState<TranscriptionProvider>("local");
  const [transcriptionModel, setTranscriptionModel] = useState("small");
  const [providerKeys, setProviderKeys] = useState<Record<"deepgram" | "assemblyai", string>>(() => ({
    deepgram: localStorage.getItem("podcast-studio-key-deepgram") ?? "",
    assemblyai: localStorage.getItem("podcast-studio-key-assemblyai") ?? "",
  }));
  const [providerKeyDrafts, setProviderKeyDrafts] = useState<Record<"deepgram" | "assemblyai", string>>(() => ({
    deepgram: localStorage.getItem("podcast-studio-key-deepgram") ?? "",
    assemblyai: localStorage.getItem("podcast-studio-key-assemblyai") ?? "",
  }));
  const [exportUrl, setExportUrl] = useState("");
  const [previewUrl, setPreviewUrl] = useState("");
  const [format, setFormat] = useState("mp4");
  const [quality, setQuality] = useState(23);
  const [includeEnhancement, setIncludeEnhancement] = useState(false);
  const [zoom, setZoom] = useState(1);
  const mediaRef = useRef<HTMLMediaElement | null>(null);
  const playbackSegmentRef = useRef(0);
  const enhancedAudioRef = useRef<HTMLAudioElement | null>(null);
  const waveformRef = useRef<HTMLCanvasElement | null>(null);
  const uploadRef = useRef<HTMLInputElement | null>(null);
  const sequenceUploadRef = useRef<HTMLInputElement | null>(null);
  const sequenceLoadRef = useRef<string | null>(null);
  const pendingSequenceSeekRef = useRef<number | null>(null);
  const words = useMemo(() => project ? getOrderedKeptWords(project) : [], [project]);
  const topicMarkers = project?.topic_markers ?? [];
  const topicPositions = useMemo(() => getTopicPositions(topicMarkers, words), [topicMarkers, words]);
  const silencePositions = useMemo(() => getSilencePositions(words, project?.edl.kept ?? []), [words, project?.edl.kept]);
  const activeWord = words.find((word) => currentTime >= word.start && currentTime <= word.end)?.id;
  const selectedWords = useMemo(() => words.filter((word) => selected.includes(word.id)), [words, selected]);
  const hasSequenceTranscript = sequenceProjects.some((item) => item.transcript.length > 0);
  const sequenceWordCount = sequenceProjects.reduce((count, item) => count + item.transcript.filter((word) => isWordKept(word, item)).length, 0);
  const selectionRange = selectedWords.length
    ? { start: Math.min(...selectedWords.map((word) => word.start)), end: Math.max(...selectedWords.map((word) => word.end)) }
    : null;
  const editedDuration = project?.edl.edited_duration ?? media?.duration ?? 0;
  const editedCurrentTime = project ? editedTimeAt(currentTime, project.edl.kept) : currentTime;
  const enhancementSignature = JSON.stringify(enhancement);
  const usingEnhancedPlayback = enhancementEnabled && Boolean(previewUrl) && previewSignature === enhancementSignature;
  const appliedPresetName = useMemo(() => {
    const keys = Object.keys(initialEnhancement) as Array<keyof Enhancement>;
    return Object.entries(presets).find(([, preset]) => keys.every((key) => preset[key] === enhancement[key]))?.[0]
      ?? (Object.keys(presets).length ? "Personalizada" : "Natural");
  }, [enhancement, presets]);

  const loadProject = useCallback(async (id: string, preserveSequence = false) => {
    const [nextProject, nextProjects] = await Promise.all([api.getProject(id), api.listProjects()]);
    const nextMedia = await api.getMedia(nextProject.media_id);
    const nextSequenceItems = preserveSequence ? null : await api.getMediaSequence(id);
    const nextSequenceProjects = nextSequenceItems
      ? await Promise.all(nextSequenceItems.map((item) => api.getProject(item.project_id)))
      : null;
    if (!preserveSequence) {
      setWorkspaceView("transcript");
      setTimelineFocus(null);
      setTimelineCutRange(null);
      setOpenMenu(null);
      setSequenceRootId(id);
      setSequenceItems(nextSequenceItems ?? []);
      setSequenceProjects(nextSequenceProjects ?? []);
    } else {
      setSequenceProjects((current) => current.map((item) => item.id === nextProject.id ? nextProject : item));
    }
    setProject(nextProject);
    setMedia(nextMedia);
    setCutClipboard([]);
    playbackSegmentRef.current = 0;
    setEnhancement(nextProject.enhancement ?? initialEnhancement);
    setProjects(nextProjects);
    setSelected([]);
    selectionAnchor.current = null;
    setPreviewUrl("");
    setPreviewSignature("");
    setFailedPreviewSignature("");
    setEnhancementEnabled(false);
    setCurrentTime(0);
    setPreviewVisible(true);
    setPlaying(false);
    setPlaybackRate(1);
    localStorage.setItem("podcast-studio-project", id);
    api.peaks(nextMedia.id).then((result) => setPeaks(result.buckets)).catch((reason: unknown) => setError(String(reason)));
  }, []);

  async function activateSequenceProject(id: string, seekTime?: number) {
    const targetProject = sequenceProjects.find((item) => item.id === id);
    if (!targetProject) return;
    if (project?.id === id) {
      if (seekTime !== undefined) seekTo(seekTime, targetProject);
      return;
    }
    if (sequenceLoadRef.current === id) {
      if (seekTime !== undefined) pendingSequenceSeekRef.current = seekTime;
      return;
    }
    sequenceLoadRef.current = id;
    pendingSequenceSeekRef.current = seekTime ?? null;
    try {
      await loadProject(id, true);
      const time = pendingSequenceSeekRef.current;
      pendingSequenceSeekRef.current = null;
      sequenceLoadRef.current = null;
      if (time === null) return;
      window.setTimeout(() => {
        const element = mediaRef.current;
        if (!element) return;
        const seek = () => seekTo(time, targetProject);
        const targetMediaId = sequenceItems.find((item) => item.project_id === id)?.media_id;
        if (element.readyState >= 1 && element.currentSrc.includes(targetMediaId ?? "")) seek();
        else element.addEventListener("loadedmetadata", seek, { once: true });
      }, 0);
    } catch (reason) {
      sequenceLoadRef.current = null;
      pendingSequenceSeekRef.current = null;
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  useEffect(() => {
    if (!project) return;
    setSequenceProjects((current) => {
      const index = current.findIndex((item) => item.id === project.id);
      if (index < 0 || current[index] === project) return current;
      const updated = [...current];
      updated[index] = project;
      return updated;
    });
  }, [project]);

  useEffect(() => {
    api.health().then((result) => setTranscriptionAvailable(result.transcription_available)).catch((reason: unknown) => setError(String(reason)));
    api.presets().then(setPresets).catch((reason: unknown) => setError(String(reason)));
    api.listProjects().then(setProjects).catch((reason: unknown) => setError(String(reason)));
    const saved = localStorage.getItem("podcast-studio-project");
    if (saved) loadProject(saved).catch(() => localStorage.removeItem("podcast-studio-project"));
  }, [loadProject]);

  useEffect(() => {
    localStorage.setItem("podcast-studio-theme", theme);
  }, [theme]);

  useEffect(() => {
    if (!error && !message) return;
    const timeout = window.setTimeout(() => {
      setError("");
      setMessage("");
    }, 1000);
    return () => window.clearTimeout(timeout);
  }, [error, message]);

  useEffect(() => {
    if (!openMenu) return;
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpenMenu(null);
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [openMenu]);

  useEffect(() => {
    const handleEditorKeys = (event: KeyboardEvent) => {
      const target = event.target;
      if (target instanceof HTMLElement && (target.isContentEditable || target.closest("input, textarea, select, [contenteditable='true']"))) return;
      const modifier = event.ctrlKey || event.metaKey;
      const key = event.key.toLowerCase();
      if (key === " " && target instanceof HTMLElement && (target.closest(".word") || target.closest(".transcript-panel"))) {
        event.preventDefault();
        void togglePlayback();
      } else if (modifier && key === "c" && selected.length) {
        event.preventDefault();
        copySelection();
      } else if (modifier && key === "x" && selected.length) {
        event.preventDefault();
        void cutSelection();
      } else if (modifier && key === "v" && cutClipboard.length) {
        event.preventDefault();
        void pasteSelection();
      } else if (modifier && key === "z") {
        event.preventDefault();
        void changeHistory(event.shiftKey ? "redo" : "undo");
      } else if (modifier && key === "y") {
        event.preventDefault();
        void changeHistory("redo");
      } else if ((key === "delete" || key === "backspace") && selected.length) {
        event.preventDefault();
        void deleteSelection();
      }
    };
    const stopDragging = () => { isDraggingSelection.current = false; };
    window.addEventListener("keydown", handleEditorKeys);
    window.addEventListener("mouseup", stopDragging);
    return () => {
      window.removeEventListener("keydown", handleEditorKeys);
      window.removeEventListener("mouseup", stopDragging);
    };
  }, [selected, cutClipboard, project, currentTime, media]);

  useEffect(() => {
    if (!selectedWords.length) {
      setSelectionPopup(null);
      return;
    }
    const updatePosition = () => {
      const element = Array.from(document.querySelectorAll<HTMLElement>(".word[data-word-id]"))
        .find((item) => item.dataset.wordId === selectedWords[0].id);
      if (!element) {
        setSelectionPopup(null);
        return;
      }
      const rect = element.getBoundingClientRect();
      setSelectionPopup({
        left: Math.max(12, Math.min(window.innerWidth - 12, rect.left + rect.width / 2)),
        top: Math.max(52, rect.top - 8),
      });
    };
    updatePosition();
    window.addEventListener("resize", updatePosition);
    window.addEventListener("scroll", updatePosition, true);
    return () => {
      window.removeEventListener("resize", updatePosition);
      window.removeEventListener("scroll", updatePosition, true);
    };
  }, [selectedWords]);

  useEffect(() => {
    const canvas = waveformRef.current;
    if (!canvas || !media || peaks.length === 0) return;
    const width = Math.max(500, canvas.parentElement?.clientWidth ?? 700) * zoom;
    const height = workspaceView === "timeline" ? 136 : 88;
    canvas.width = width * devicePixelRatio;
    canvas.height = height * devicePixelRatio;
    canvas.style.width = `${width}px`;
    canvas.style.height = `${height}px`;
    const context = canvas.getContext("2d");
    if (!context) return;
    context.scale(devicePixelRatio, devicePixelRatio);
    context.clearRect(0, 0, width, height);
    const visibleStart = timelineFocus?.start ?? 0;
    const visibleEnd = timelineFocus?.end ?? media.duration;
    const visibleDuration = Math.max(0.001, visibleEnd - visibleStart);
    const xFor = (seconds: number) => ((seconds - visibleStart) / visibleDuration) * width;
    context.fillStyle = getComputedStyle(canvas).getPropertyValue("--waveform-color").trim();
    peaks.forEach((peak, index) => {
      const bucketStart = (index / peaks.length) * media.duration;
      const bucketEnd = ((index + 1) / peaks.length) * media.duration;
      if (bucketEnd < visibleStart || bucketStart > visibleEnd) return;
      const time = (bucketStart + bucketEnd) / 2;
      const clippedStart = Math.max(bucketStart, visibleStart);
      const clippedEnd = Math.min(bucketEnd, visibleEnd);
      const x = xFor(clippedStart);
      const removed = !project?.edl.kept.some((part) => time >= part.start && time < part.end);
      context.globalAlpha = removed ? 0.2 : 0.78;
      const center = height / 2;
      const top = center - Math.max(1, Math.abs(peak.max) * center * 0.88);
      const bottom = center + Math.max(1, Math.abs(peak.min) * center * 0.88);
      context.fillRect(x, top, Math.max(1, xFor(clippedEnd) - x), bottom - top);
    });
    context.globalAlpha = 1;
    const selectionIsFocused = timelineFocus && selectionRange
      && selectionRange.start <= timelineFocus.start && selectionRange.end >= timelineFocus.end;
    const visibleSelection = timelineCutRange ?? (selectionIsFocused ? null : selectionRange);
    if (visibleSelection && visibleSelection.end > visibleStart && visibleSelection.start < visibleEnd) {
      const left = xFor(Math.max(visibleStart, visibleSelection.start));
      const right = xFor(Math.min(visibleEnd, visibleSelection.end));
      context.fillStyle = timelineCutRange
        ? getComputedStyle(canvas).getPropertyValue("--danger").trim()
        : getComputedStyle(canvas).getPropertyValue("--selection-color").trim();
      context.globalAlpha = 0.24;
      context.fillRect(left, 0, Math.max(2, right - left), height);
      context.globalAlpha = 1;
      context.strokeStyle = getComputedStyle(canvas).getPropertyValue("--selection-color").trim();
      context.lineWidth = 2;
      context.strokeRect(left + 1, 1, Math.max(0, right - left - 2), height - 2);
    }
    const playheadX = xFor(currentTime);
    context.fillStyle = getComputedStyle(canvas).getPropertyValue("--playhead-color").trim();
    context.fillRect(playheadX, 0, 2, height);
  }, [currentTime, media, peaks, project, selectionRange?.start, selectionRange?.end, theme, workspaceView, zoom, timelineFocus?.start, timelineFocus?.end, timelineCutRange?.start, timelineCutRange?.end]);

  useEffect(() => {
    if (!project) return;
    const timer = window.setTimeout(() => {
      api.saveEnhancement(project.id, enhancement)
        .then((updated) => setProject((current) => current?.id === updated.id
          ? { ...current, enhancement: updated.enhancement }
          : current))
        .catch((reason: unknown) => setError(String(reason)));
    }, 500);
    return () => window.clearTimeout(timer);
  }, [enhancement, project?.id]);

  useEffect(() => {
    if (!enhancementEnabled || !project || !media || busy || previewSignature === enhancementSignature
      || failedPreviewSignature === enhancementSignature) return;
    const timer = window.setTimeout(() => void renderEnhancementPreview(), 450);
    return () => window.clearTimeout(timer);
  }, [enhancementEnabled, enhancementSignature, busy, failedPreviewSignature, media?.duration, previewSignature, project?.id]);

  useEffect(() => {
    const original = mediaRef.current;
    const enhanced = enhancedAudioRef.current;
    if (!original) return;
    if (usingEnhancedPlayback && enhanced) {
      enhanced.currentTime = currentTime;
      enhanced.playbackRate = playbackRate;
      if (media?.has_video) {
        original.currentTime = currentTime;
        if (playing && original.paused) {
          void original.play().catch((reason: unknown) => setError(String(reason)));
        }
      } else {
        original.pause();
      }
      if (playing && enhanced.paused) {
        void enhanced.play().catch((reason: unknown) => setError(String(reason)));
      }
    } else {
      enhanced?.pause();
      original.currentTime = currentTime;
      original.playbackRate = playbackRate;
      if (playing && original.paused) {
        void original.play().catch((reason: unknown) => setError(String(reason)));
      }
    }
  }, [usingEnhancedPlayback, previewUrl, playing, media?.has_video, playbackRate]);

  async function handleImport(file?: File) {
    if (!file) return;
    setError("");
    setBusy("import");
    setMessage("Importando e analisando arquivo…");
    try {
      const imported = await api.importMedia(file);
      const created = await api.createProject(imported.id);
      await loadProject(created.id);
      setMessage("Arquivo pronto para edição.");
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy("");
    }
  }

  async function handleAddSequenceMedia(file?: File) {
    const rootId = sequenceRootId ?? project?.id;
    if (!file || !rootId) return;
    setError("");
    setBusy("import");
    setMessage("Adicionando mídia à sequência…");
    try {
      const imported = await api.importMedia(file);
      const nextItems = await api.addMediaToSequence(rootId, imported.id);
      const nextProjects = await Promise.all(nextItems.map((item) => api.getProject(item.project_id)));
      const added = nextItems.find((item) => item.media_id === imported.id);
      setSequenceItems(nextItems);
      setSequenceProjects(nextProjects);
      if (added) await loadProject(added.project_id, true);
      setMessage("Mídia adicionada à sequência.");
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy("");
    }
  }

  async function moveSequenceItem(projectId: string, direction: -1 | 1) {
    if (!sequenceRootId) return;
    const index = sequenceItems.findIndex((item) => item.project_id === projectId);
    const target = index + direction;
    if (index < 0 || target < 0 || target >= sequenceItems.length) return;
    const reordered = [...sequenceItems];
    [reordered[index], reordered[target]] = [reordered[target], reordered[index]];
    try {
      const updated = await api.reorderMediaSequence(sequenceRootId, reordered.map((item) => item.project_id));
      const projectsById = new Map(sequenceProjects.map((item) => [item.id, item]));
      setSequenceItems(updated);
      setSequenceProjects(updated.map((item) => projectsById.get(item.project_id)).filter((item): item is Project => Boolean(item)));
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  async function returnToLibrary() {
    setWorkspaceView("transcript");
    setOpenMenu(null);
    setProject(null);
    setMedia(null);
    setSequenceRootId(null);
    setSequenceItems([]);
    setSequenceProjects([]);
    setPeaks([]);
    setSelected([]);
    setCutClipboard([]);
    setError("");
    setMessage("");
    localStorage.removeItem("podcast-studio-project");
    try {
      setProjects(await api.listProjects());
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  async function openProject(id: string) {
    setError("");
    try {
      await loadProject(id);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  async function renameProject(id: string, currentName: string) {
    const name = window.prompt("Nome do projeto:", currentName)?.trim();
    if (!name || name === currentName) return;
    setError("");
    try {
      const updated = await api.renameProject(id, name);
      setProjects(await api.listProjects());
      setProject((current) => current?.id === id ? updated : current);
      setMessage("Nome do projeto atualizado.");
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  async function deleteProject(id: string, name: string) {
    if (!window.confirm(`Excluir o projeto “${name}”? A mídia original será preservada.`)) return;
    setError("");
    try {
      await api.deleteProject(id);
      setProjects((current) => current.filter((item) => item.id !== id));
      if (project?.id === id) {
        setProject(null);
        setMedia(null);
        setPeaks([]);
        localStorage.removeItem("podcast-studio-project");
      }
      setMessage("Projeto excluído. A mídia original foi preservada.");
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  async function runTranscription(targetProjectId = project?.id) {
    if (!targetProjectId) return;
    const selectedProviderKey = transcriptionProvider === "local" ? "" : providerKeys[transcriptionProvider];
    if (transcriptionProvider !== "local" && !selectedProviderKey.trim()) {
      setError("Informe a chave de API do provedor selecionado.");
      setWorkspaceView("settings");
      return;
    }
    setError("");
    setBusy("transcribe");
    setTranscriptionTargetId(targetProjectId);
    setProgress(0);
    setMessage(transcriptionProvider === "local" ? "Preparando transcrição local…" : "Preparando envio seguro para o provedor…");
    try {
      const options = transcriptionProvider === "local"
        ? { provider: transcriptionProvider, model: transcriptionModel }
        : { provider: transcriptionProvider, api_key: selectedProviderKey.trim() };
      const job = await api.transcribe(targetProjectId, options);
      setActiveJobId(job.id);
      await waitForJob(job.id, (value, status) => {
        setProgress(value);
        if (status) setMessage(status);
      });
      const updated = await api.getProject(targetProjectId);
      setSequenceProjects((current) => current.map((item) => item.id === targetProjectId ? updated : item));
      if (project?.id === targetProjectId) setProject(updated);
      setMessage("Transcrição concluída para esta mídia.");
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
      setMessage("");
    } finally {
      setBusy("");
      setTranscriptionTargetId("");
      setActiveJobId("");
    }
  }

  function saveProviderKey(provider: "deepgram" | "assemblyai") {
    const key = providerKeyDrafts[provider].trim();
    const storageKey = `podcast-studio-key-${provider}`;
    if (key) localStorage.setItem(storageKey, key);
    else localStorage.removeItem(storageKey);
    setProviderKeys((current) => ({ ...current, [provider]: key }));
    setProviderKeyDrafts((current) => ({ ...current, [provider]: key }));
    setError("");
    setMessage(`Chave do ${provider === "deepgram" ? "Deepgram" : "AssemblyAI"} salva neste navegador.`);
  }

  async function deleteSelection() {
    if (!project || selected.length === 0) return;
    setError("");
    try {
      const updated = await api.deleteWords(project.id, selected);
      setProject(updated);
      seekTo(currentTime, updated);
      setSelected([]);
      selectionAnchor.current = null;
      setMessage("Trecho removido da edição. O arquivo original foi preservado.");
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  async function cutSelection() {
    if (!project || !selected.length) return;
    const intervals = getSelectionIntervals(project.transcript, selected);
    if (!intervals.length) return;
    setError("");
    try {
      const updated = await api.deleteWords(project.id, selected);
      setProject(updated);
      setSequenceProjects((current) => current.map((item) => item.id === updated.id ? updated : item));
      setCutClipboard(intervals);
      seekTo(currentTime, updated);
      setSelected([]);
      selectionAnchor.current = null;
      setMessage("Trecho recortado. Posicione o playhead e use Colar para inseri-lo.");
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  function copySelection() {
    if (!project || !selected.length) return;
    const intervals = getSelectionIntervals(project.transcript, selected);
    if (!intervals.length) return;
    setCutClipboard(intervals);
    setMessage("Trecho copiado. Posicione o playhead e use Ctrl+V para inseri-lo.");
  }

  async function pasteSelection() {
    if (!project || !cutClipboard.length) return;
    setError("");
    try {
      const updated = await api.paste(project.id, cutClipboard, currentTime);
      setProject(updated);
      setSequenceProjects((current) => current.map((item) => item.id === updated.id ? updated : item));
      const pastedStart = cutClipboard[0].start;
      seekTo(pastedStart, updated);
      setMessage("Trecho colado na posição do playhead.");
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  async function removeSilence(silence: { start: number; end: number }) {
    if (!project) return;
    setError("");
    try {
      const updated = await api.cut(project.id, silence.start, silence.end);
      setProject(updated);
      setSequenceProjects((current) => current.map((item) => item.id === updated.id ? updated : item));
      seekTo(silence.start, updated);
      setMessage(`Silêncio de ${formatTime(silence.start)}–${formatTime(silence.end)} removido.`);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  async function changeHistory(action: "undo" | "redo") {
    if (!project) return;
    try {
      const updated = await api.history(project.id, action);
      setProject(updated);
      seekTo(currentTime, updated);
      setCutClipboard([]);
      setSelected([]);
      selectionAnchor.current = null;
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  async function cutAtPlayhead() {
    if (!project || !media) return;
    const focusStart = timelineFocus?.start ?? 0;
    const focusEnd = timelineFocus?.end ?? media.duration;
    const end = Math.min(focusEnd, currentTime + Math.max(0.5, 1 / (media.fps || 30)));
    const start = Math.max(focusStart, currentTime - 0.5);
    if (end <= start) return;
    try {
      const updated = await api.cut(project.id, start, end);
      setProject(updated);
      setSequenceProjects((current) => current.map((item) => item.id === updated.id ? updated : item));
      seekTo(currentTime, updated);
      setMessage(`Trecho ${formatTime(start)}–${formatTime(end)} removido.`);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  async function correctWord(word: Word) {
    if (!project) return;
    const corrected = window.prompt("Corrigir texto (o áudio não será alterado):", word.text);
    if (!corrected?.trim() || corrected.trim() === word.text) return;
    try {
      setProject(await api.correctWord(project.id, word.id, corrected.trim()));
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  function seekTo(time: number, projectForSeek = project) {
    if (!mediaRef.current || !projectForSeek) return;
    const kept = projectForSeek.edl.kept;
    const segmentIndex = kept.findIndex((part) => time >= part.start && time < part.end);
    const followingIndex = kept.reduce((best, part, index) => part.start >= time && (best < 0 || part.start < kept[best].start) ? index : best, -1);
    const targetIndex = segmentIndex >= 0 ? segmentIndex : followingIndex >= 0 ? followingIndex : Math.max(0, kept.length - 1);
    const segment = kept[targetIndex];
    playbackSegmentRef.current = targetIndex;
    const target = segment ? segmentIndex >= 0 ? time : segment.start : 0;
    mediaRef.current.currentTime = target;
    if (enhancedAudioRef.current) enhancedAudioRef.current.currentTime = target;
    setCurrentTime(target);
  }

  function handlePlaybackTime(event: SyntheticEvent<HTMLMediaElement>) {
    if (usingEnhancedPlayback) return;
    const time = event.currentTarget.currentTime;
    if (previewEnd.current !== null && time >= previewEnd.current) {
      previewEnd.current = null;
      event.currentTarget.pause();
      enhancedAudioRef.current?.pause();
      setPlaying(false);
      return;
    }
    const kept = project?.edl.kept ?? [];
    const segment = kept[playbackSegmentRef.current];
    if (segment && time >= segment.end - 0.025) {
      const nextIndex = playbackSegmentRef.current + 1;
      if (nextIndex < kept.length) {
        playbackSegmentRef.current = nextIndex;
        event.currentTarget.currentTime = kept[nextIndex].start;
        if (enhancedAudioRef.current) enhancedAudioRef.current.currentTime = kept[nextIndex].start;
      } else {
        event.currentTarget.pause();
        enhancedAudioRef.current?.pause();
        setPlaying(false);
      }
    }
    setCurrentTime(time);
  }

  function handleEnhancedPlaybackTime(event: SyntheticEvent<HTMLAudioElement>) {
    const time = event.currentTarget.currentTime;
    if (previewEnd.current !== null && time >= previewEnd.current) {
      previewEnd.current = null;
      event.currentTarget.pause();
      mediaRef.current?.pause();
      setPlaying(false);
      return;
    }
    const kept = project?.edl.kept ?? [];
    const segment = kept[playbackSegmentRef.current];
    if (segment && time >= segment.end - 0.025) {
      const nextIndex = playbackSegmentRef.current + 1;
      if (nextIndex < kept.length) {
        playbackSegmentRef.current = nextIndex;
        event.currentTarget.currentTime = kept[nextIndex].start;
        if (mediaRef.current) mediaRef.current.currentTime = kept[nextIndex].start;
      } else {
        event.currentTarget.pause();
        mediaRef.current?.pause();
        setPlaying(false);
      }
    } else if (mediaRef.current && media?.has_video && Math.abs(mediaRef.current.currentTime - time) > 0.2) {
      mediaRef.current.currentTime = time;
    }
    setCurrentTime(time);
  }

  async function togglePlayback(startAt?: number) {
    const activeMedia = usingEnhancedPlayback ? enhancedAudioRef.current : mediaRef.current;
    if (!activeMedia) return;
    if (activeMedia.paused) {
      if (startAt !== undefined) seekTo(startAt);
      const playhead = startAt ?? currentTime;
      const segment = project?.edl.kept[playbackSegmentRef.current];
      if (segment && (playhead < segment.start || playhead >= segment.end)) seekTo(segment.start);
      try {
        if (media?.has_video && mediaRef.current && mediaRef.current !== activeMedia) {
          await mediaRef.current.play();
        }
        await activeMedia.play();
      } catch (reason) {
        setError(reason instanceof Error ? reason.message : String(reason));
      }
    } else {
      activeMedia.pause();
      if (media?.has_video && mediaRef.current && mediaRef.current !== activeMedia) mediaRef.current.pause();
    }
  }

  function selectWord(wordId: string, extend: boolean) {
    previewEnd.current = null;
    const targetIndex = words.findIndex((word) => word.id === wordId);
    const anchorIndex = words.findIndex((word) => word.id === selectionAnchor.current);
    if (extend && anchorIndex >= 0 && targetIndex >= 0) {
      const start = Math.min(anchorIndex, targetIndex);
      const end = Math.max(anchorIndex, targetIndex);
      setSelected(words.slice(start, end + 1).map((word) => word.id));
      return;
    }
    selectionAnchor.current = wordId;
    setSelected([wordId]);
  }

  function extendSelectionTo(wordId: string) {
    const wordsInView = words;
    const anchorIndex = wordsInView.findIndex((word) => word.id === selectionAnchor.current);
    const targetIndex = wordsInView.findIndex((word) => word.id === wordId);
    if (anchorIndex < 0 || targetIndex < 0) return;
    setSelected(wordsInView.slice(Math.min(anchorIndex, targetIndex), Math.max(anchorIndex, targetIndex) + 1).map((word) => word.id));
  }

  async function previewSelection() {
    if (!selectionRange || !mediaRef.current) return;
    previewEnd.current = selectionRange.end;
    seekTo(selectionRange.start);
    setPlaying(true);
    try {
      if (usingEnhancedPlayback && enhancedAudioRef.current) {
        if (media?.has_video) await mediaRef.current.play();
        await enhancedAudioRef.current.play();
      } else {
        await mediaRef.current.play();
      }
    } catch (reason) {
      previewEnd.current = null;
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  async function saveTopicMarkers(nextMarkers: TopicMarker[], successMessage: string) {
    if (!project) return;
    setError("");
    try {
      const updated = await api.saveTopicMarkers(project.id, [...nextMarkers].sort((a, b) => a.time - b.time));
      setProject(updated);
      setMessage(successMessage);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  async function addTopicMarker() {
    if (!project || words.length === 0) return;
    const selectedWords = words.filter((word) => selected.includes(word.id));
    const target = selectedWords.length
      ? selectedWords.reduce((earliest, word) => word.start < earliest.start ? word : earliest)
      : words.find((word) => word.start >= currentTime);
    const time = target?.start ?? Math.min(currentTime, media?.duration ?? currentTime);
    if (topicMarkers.some((marker) => Math.abs(marker.time - time) < 0.01)) {
      setError("Já existe um tópico nesse ponto da transcrição.");
      return;
    }
    const title = window.prompt("Nome do tópico:");
    if (!title?.trim()) return;
    const marker: TopicMarker = { id: crypto.randomUUID(), title: title.trim(), time };
    await saveTopicMarkers([...topicMarkers, marker], "Separador de tópico adicionado.");
  }

  async function editTopicMarker(marker: TopicMarker) {
    const title = window.prompt("Nome do tópico:", marker.title);
    if (!title?.trim() || title.trim() === marker.title) return;
    await saveTopicMarkers(topicMarkers.map((item) => item.id === marker.id ? { ...item, title: title.trim() } : item), "Tópico atualizado.");
  }

  async function removeTopicMarker(marker: TopicMarker) {
    await saveTopicMarkers(topicMarkers.filter((item) => item.id !== marker.id), "Separador de tópico removido.");
  }

  function exportTranscript() {
    if (!project || sequenceWordCount === 0) return;
    if (sequenceProjects.length > 1) {
      const content = sequenceProjects.map((item, index) => {
        const itemWords = getOrderedKeptWords(item);
        if (!itemWords.length) return "";
        const name = sequenceItems.find((sequenceItem) => sequenceItem.project_id === item.id)?.name ?? item.name;
        return `${index + 1}. ${name} [${formatTime(itemWords[0].start)}–${formatTime(itemWords[itemWords.length - 1].end)}]\n${itemWords.map((word) => word.text).join(" ")}`;
      }).filter(Boolean).join("\n\n");
      const file = new Blob([content], { type: "text/plain;charset=utf-8" });
      const url = URL.createObjectURL(file);
      const link = document.createElement("a");
      const safeName = project.name.trim().replace(/[\\/:*?"<>|]+/g, "-") || "transcricao";
      link.href = url;
      link.download = `${safeName}-transcricao.txt`;
      link.click();
      window.setTimeout(() => URL.revokeObjectURL(url), 1000);
      setMessage("Transcrição exportada na ordem da sequência.");
      return;
    }
    const sections = [...topicMarkers]
      .sort((a, b) => a.time - b.time)
      .map((marker, index, markers) => ({
        title: marker.title,
        start: marker.time,
        end: markers[index + 1]?.time ?? media?.duration ?? marker.time,
      }));
    const boundaries = [{ title: "Transcrição", start: 0, end: sections[0]?.start ?? media?.duration ?? 0 }, ...sections];
    const content = boundaries.map((section) => {
      const sectionWords = words.filter((word) => word.start >= section.start && word.start < section.end);
      if (sectionWords.length === 0) return "";
      const start = sectionWords[0].start;
      const end = sectionWords[sectionWords.length - 1].end;
      return `${section.title} [${formatTime(start)}–${formatTime(end)}]\n${sectionWords.map((word) => word.text).join(" ")}`;
    }).filter(Boolean).join("\n\n");
    const file = new Blob([content], { type: "text/plain;charset=utf-8" });
    const url = URL.createObjectURL(file);
    const link = document.createElement("a");
    const safeName = project.name.trim().replace(/[\\/:*?"<>|]+/g, "-") || "transcricao";
    link.href = url;
    link.download = `${safeName}-transcricao.txt`;
    link.click();
    window.setTimeout(() => URL.revokeObjectURL(url), 1000);
    setMessage("Transcrição exportada com os tópicos e timestamps.");
  }

  async function renderEnhancementPreview() {
    if (!project || !media) return;
    const params = { ...enhancement };
    const signature = JSON.stringify(params);
    const projectId = project.id;
    setError("");
    setBusy("enhance");
    setMessage("Aplicando melhorias à gravação…");
    try {
      const result = await api.enhancementPreview(projectId, params, media.duration);
      let url = result.url;
      if (result.id) {
        setActiveJobId(result.id);
        const job = await waitForJob(result.id, (value) => setProgress(value));
        url = job.result?.url;
      }
      if (!url) throw new Error("A prévia processada não retornou um arquivo de áudio.");
      setPreviewUrl(`${api.base}${url}`);
      setPreviewSignature(signature);
      setFailedPreviewSignature("");
      setProject(await api.saveEnhancement(projectId, params));
      setMessage("Melhoria de voz pronta para reprodução.");
    } catch (reason) {
      setFailedPreviewSignature(signature);
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy("");
      setActiveJobId("");
    }
  }

  async function savePreset() {
    const name = window.prompt("Nome da predefinição personalizada:");
    if (!name?.trim()) return;
    try {
      setPresets(await api.savePreset(name.trim(), enhancement));
      setMessage(`Predefinição “${name.trim()}” salva.`);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  async function runExport() {
    if (!project) return;
    setError("");
    setExportUrl("");
    setBusy("export");
    setProgress(0);
    setMessage("Exportando somente os trechos mantidos…");
    try {
      const job = await api.export(project.id, format, quality, includeEnhancement ? enhancement : undefined);
      setActiveJobId(job.id);
      const finished = await waitForJob(job.id, (value, status) => {
        setProgress(value);
        if (status) setMessage(status);
      });
      const url = `${api.base}${finished.result?.url}`;
      setExportUrl(url);
      setMessage("Exportação concluída.");
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
      setMessage("");
    } finally {
      setBusy("");
      setActiveJobId("");
    }
  }

  async function cancelActiveJob() {
    if (!activeJobId) return;
    try {
      await api.cancelJob(activeJobId);
      setMessage("Cancelando operação…");
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  function openFocusedTimeline() {
    if (!selectionRange || !media) return;
    setTimelineFocus({ start: selectionRange.start, end: selectionRange.end });
    setTimelineCutRange(null);
    setZoom(1);
    setWorkspaceView("timeline");
    setOpenMenu(null);
    seekTo(selectionRange.start);
  }

  function timelinePointerDown(event: PointerEvent<HTMLCanvasElement>) {
    if (event.button !== 0 || !media || !waveformRef.current) return;
    const bounds = waveformRef.current.getBoundingClientRect();
    const ratio = Math.max(0, Math.min(1, (event.clientX - bounds.left) / bounds.width));
    const start = timelineFocus?.start ?? 0;
    const end = timelineFocus?.end ?? media.duration;
    const time = start + ratio * (end - start);
    timelineDragStart.current = time;
    setTimelineCutRange({ start: time, end: time });
    event.currentTarget.setPointerCapture(event.pointerId);
  }

  function timelinePointerMove(event: PointerEvent<HTMLCanvasElement>) {
    if (timelineDragStart.current === null || !media || !waveformRef.current) return;
    const bounds = waveformRef.current.getBoundingClientRect();
    const ratio = Math.max(0, Math.min(1, (event.clientX - bounds.left) / bounds.width));
    const start = timelineFocus?.start ?? 0;
    const end = timelineFocus?.end ?? media.duration;
    const time = start + ratio * (end - start);
    setTimelineCutRange({ start: Math.min(timelineDragStart.current, time), end: Math.max(timelineDragStart.current, time) });
  }

  function timelinePointerUp(event: PointerEvent<HTMLCanvasElement>) {
    if (timelineDragStart.current === null) return;
    const start = timelineDragStart.current;
    timelineDragStart.current = null;
    const bounds = waveformRef.current?.getBoundingClientRect();
    if (!bounds || !media) return;
    const ratio = Math.max(0, Math.min(1, (event.clientX - bounds.left) / bounds.width));
    const focusStart = timelineFocus?.start ?? 0;
    const focusEnd = timelineFocus?.end ?? media.duration;
    const end = focusStart + ratio * (focusEnd - focusStart);
    if (Math.abs(end - start) < 0.04) {
      setTimelineCutRange(null);
      previewEnd.current = null;
      seekTo(end);
      return;
    }
    previewEnd.current = null;
    setTimelineCutRange({ start: Math.min(start, end), end: Math.max(start, end) });
  }

  async function toggleTimelinePlayback() {
    if (playing) {
      previewEnd.current = null;
      await togglePlayback();
      return;
    }
    const range = timelineCutRange ?? timelineFocus;
    if (range) previewEnd.current = range.end;
    await togglePlayback(range?.start);
  }

  async function cutTimelineSelection() {
    if (!project || !timelineCutRange || timelineCutRange.end - timelineCutRange.start < 0.04) return;
    setError("");
    try {
      const { start, end } = timelineCutRange;
      const updated = await api.cut(project.id, start, end);
      setProject(updated);
      setSequenceProjects((current) => current.map((item) => item.id === updated.id ? updated : item));
      setTimelineCutRange(null);
      seekTo(start, updated);
      setMessage(`Trecho ${formatTime(start)}–${formatTime(end)} removido da timeline.`);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  const activeEnhancement = busy === "enhance";
  return (
    <div className={`app-shell ${project && media ? "editor-active" : ""}`} data-theme={theme}>
      <header className="topbar">
        <a className="brand" href="#" aria-label="Voltar para os projetos" onClick={(event) => {
          event.preventDefault();
          void returnToLibrary();
        }}>
          <span className="brand-mark"><AudioLines size={19} /></span>
          <span>podcast<span className="brand-light">studio</span></span>
        </a>
        <button className="button subtle new-project-button" onClick={() => uploadRef.current?.click()} disabled={Boolean(busy)} aria-label="Novo projeto" title="Novo projeto">
          <Plus size={16} /> Novo projeto
        </button>
        {project && <div className="project-title" title={project.name}><span>{project.name}</span></div>}
        <div className="top-actions">
          {!project && workspaceView !== "settings" && <button className="button subtle menu-button" onClick={() => {
            setWorkspaceView("settings");
            setOpenMenu(null);
          }} aria-label="Configurações de transcrição" title="Configurações de transcrição">
            <KeyRound size={15} /> Chaves API
          </button>}
          {project && <button className={`button subtle menu-button view-button ${workspaceView === "transcript" ? "view-button-active" : ""}`} onClick={() => {
            setWorkspaceView("transcript");
            setOpenMenu(null);
          }} aria-label="Transcrição" aria-current={workspaceView === "transcript" ? "page" : undefined}>
            <FileText size={15} /> Transcrição
          </button>}
          {project && <button className="button subtle menu-button" onClick={() => {
            setWorkspaceView("transcript");
            setOpenMenu((menu) => menu === "enhance" ? null : "enhance");
          }} aria-label="Melhorar voz" aria-expanded={openMenu === "enhance"}>
            <WandSparkles size={15} /> Melhorar voz
          </button>}
          {project && <button className="button subtle menu-button" onClick={() => {
            setWorkspaceView("transcript");
            setOpenMenu((menu) => menu === "export" ? null : "export");
          }} aria-label="Exportação" aria-expanded={openMenu === "export"}>
            <ArrowDownToLine size={15} /> Exportação
          </button>}
          <button className="button subtle theme-toggle" onClick={() => setTheme((current) => current === "dark" ? "light" : "dark")} aria-label={theme === "dark" ? "Ativar tema claro" : "Ativar tema escuro"} title={theme === "dark" ? "Ativar tema claro" : "Ativar tema escuro"}>
            {theme === "dark" ? <Sun size={16} /> : <Moon size={16} />}
          </button>
        </div>
        <input ref={uploadRef} className="visually-hidden" type="file" accept="audio/*,video/*,.mkv" onChange={(event) => void handleImport(event.target.files?.[0])} />
      </header>

      <main className="workspace">
        {workspaceView === "settings" ? (
          <section className="settings-page">
            <div className="settings-page-heading">
              <p className="eyebrow">PREFERÊNCIAS DO NAVEGADOR</p>
              <h1>Chaves de API</h1>
              <p>Configure uma chave para cada serviço. As chaves ficam neste navegador e são usadas quando você seleciona o provedor na transcrição.</p>
            </div>
            <div className="provider-settings-grid">
              {(["deepgram", "assemblyai"] as const).map((provider) => {
                const name = provider === "deepgram" ? "Deepgram" : "AssemblyAI";
                const signupUrl = provider === "deepgram" ? "https://console.deepgram.com/signup" : "https://www.assemblyai.com/dashboard/signup";
                const keysUrl = provider === "deepgram" ? "https://developers.deepgram.com/docs/create-additional-api-keys" : "https://www.assemblyai.com/docs/faq/how-to-get-your-api-key";
                return <section className="provider-settings-card panel" key={provider}>
                  <div className="provider-settings-title"><span className="settings-key-icon"><KeyRound size={17} /></span><div><p className="eyebrow">PROVEDOR DE TRANSCRIÇÃO</p><h2>{name}</h2></div></div>
                  <label className="settings-key-field"><span>Chave de API</span><input type="password" autoComplete="off" spellCheck={false} value={providerKeyDrafts[provider]} onChange={(event) => setProviderKeyDrafts((current) => ({ ...current, [provider]: event.target.value }))} placeholder={`Cole sua chave ${name}`} /></label>
                  <div className="provider-settings-links"><a href={signupUrl} target="_blank" rel="noreferrer">Criar conta <ExternalLink size={13} /></a><a href={keysUrl} target="_blank" rel="noreferrer">Gerar chave <ExternalLink size={13} /></a></div>
                  <div className="provider-settings-footer"><span>{providerKeys[provider] ? "Chave salva neste navegador" : "Nenhuma chave salva"}</span><button className="button primary" onClick={() => saveProviderKey(provider)}>{providerKeyDrafts[provider].trim() ? "Salvar chave" : "Remover chave salva"}</button></div>
                </section>;
              })}
            </div>
            <p className="settings-storage-note">As chaves são armazenadas no armazenamento local deste navegador neste dispositivo. Evite salvar aqui em computadores compartilhados.</p>
          </section>
        ) : !project || !media ? (
          <section className="welcome-card">
            <div className="welcome-icon"><AudioLines size={28} /></div>


            <button className="button primary large" onClick={() => uploadRef.current?.click()} disabled={Boolean(busy)}>
              {busy === "import" ? <LoaderCircle className="spin" size={17} /> : <Upload size={17} />}
              {busy === "import" ? "Importando…" : "Importar mídia"}
            </button>
            <span className="file-hint">MP3, WAV, M4A, MP4, MOV, MKV ou WebM</span>
            {projects.length > 0 && <div className="recent-projects">
              <span>Projetos salvos</span>
              {projects.map((item) => <div className="project-entry" key={item.id}>
                <button className="project-entry-open" onClick={() => void openProject(item.id)}><FolderOpen size={14} />{item.name}</button>
                <button className="project-entry-action" onClick={() => void renameProject(item.id, item.name)} aria-label={`Renomear ${item.name}`} title="Renomear"><Pencil size={13} /></button>
                <button className="project-entry-action destructive" onClick={() => void deleteProject(item.id, item.name)} aria-label={`Excluir ${item.name}`} title="Excluir"><Trash2 size={13} /></button>
              </div>)}
            </div>}
          </section>
        ) : (
          <>
            {workspaceView === "timeline" ? (
              <section className="timeline-page">
                <div className="timeline-page-heading-row">
                  <div className="timeline-page-heading">
                    <p className="eyebrow">EDIÇÃO NÃO DESTRUTIVA</p>
                    <h1>{timelineFocus ? "Timeline do trecho" : "Linha do tempo"}</h1>
                    <p>{timelineFocus
                      ? `Trecho focado em ${formatPreciseTime(timelineFocus.start)}–${formatPreciseTime(timelineFocus.end)}. Arraste na onda para marcar um corte preciso.`
                      : "Revise a forma de onda, navegue pela gravação e corte no ponto atual."}</p>
                  </div>
                  <button className="button subtle timeline-return" onClick={() => {
                    previewEnd.current = null;
                    setTimelineCutRange(null);
                    setWorkspaceView("transcript");
                    setOpenMenu(null);
                  }}><FileText size={15} /> Voltar à edição</button>
                </div>
                <section className="timeline-panel panel">
                  <div className="timeline-heading">
                    <div className="heading-inline"><div><p className="eyebrow">ÁUDIO ORIGINAL</p><h2>Forma de onda</h2></div><span className="track-label"><AudioLines size={13} /> Cortes não destrutivos</span></div>
                    <div className="timeline-tools">
                      <button className="icon-button" title={playing ? "Pausar reprodução" : "Reproduzir trecho da onda"} aria-label={playing ? "Pausar reprodução" : "Reproduzir trecho da onda"} onClick={() => void toggleTimelinePlayback()}>{playing ? <Pause size={15} /> : <Play size={15} />}</button>
                      <button className="icon-button" title={timelineCutRange ? "Remover intervalo marcado" : "Cortar perto do playhead"} aria-label={timelineCutRange ? "Remover intervalo marcado" : "Cortar perto do playhead"} onClick={() => timelineCutRange ? void cutTimelineSelection() : void cutAtPlayhead()}><Scissors size={15} /></button>
                      <button className="icon-button" title={cutClipboard.length ? "Colar trecho recortado no playhead" : "Nenhum trecho recortado"} onClick={() => void pasteSelection()} disabled={!cutClipboard.length}><ClipboardPaste size={15} /></button>
                      <label className="zoom-control"><ZoomIn size={15} /><input type="range" min="1" max="4" step="0.5" value={zoom} onChange={(event) => setZoom(Number(event.target.value))} aria-label="Zoom da linha do tempo" /></label>
                      <span>{zoom.toFixed(1)}×</span>
                      {timelineFocus && <button className="button subtle timeline-reset" onClick={() => { setTimelineFocus(null); setTimelineCutRange(null); }} title="Mostrar a timeline completa">Timeline completa</button>}
                    </div>
                  </div>
                  <div className="waveform-scroll"><canvas ref={waveformRef} onPointerDown={timelinePointerDown} onPointerMove={timelinePointerMove} onPointerUp={timelinePointerUp} aria-label="Forma de onda interativa; arraste para marcar um corte" /></div>
                  <div className="timeline-ruler"><span>{formatPreciseTime(timelineFocus?.start ?? 0)}</span><span>{formatPreciseTime(((timelineFocus?.start ?? 0) + (timelineFocus?.end ?? media.duration)) / 2)}</span><span>{formatPreciseTime(timelineFocus?.end ?? media.duration)}</span></div>
                  <p className="timeline-help">Clique na onda para posicionar o playhead. Arraste para marcar um intervalo e use a tesoura para removê-lo com precisão{timelineFocus ? " dentro do trecho focado" : ""}.</p>
                </section>
              </section>
            ) : <>
            <div className="editor-grid">
              <section className={`preview-panel compact-preview panel ${previewVisible ? "" : "preview-hidden"}`}>
                {project.transcript.length > 0 && <div className="transcript-toolbar sidebar-transcript-tools">
                  <span><span className="speaker-dot" /> Speaker 1 {project.language ? `· ${project.language.toUpperCase()}` : ""}</span>
                  <div className="toolbar-actions">
                    <button title="Adicionar separador de tópico" aria-label="Adicionar separador de tópico" onClick={() => void addTopicMarker()} disabled={!words.length}><BookmarkPlus size={15} /></button>
                    <button title="Desfazer remoção" aria-label="Desfazer remoção" onClick={() => void changeHistory("undo")} disabled={!project.undo_stack.length}><Undo2 size={15} /></button>
                    <button title="Refazer remoção" aria-label="Refazer remoção" onClick={() => void changeHistory("redo")} disabled={!project.redo_stack.length}><Redo2 size={15} /></button>
                    {cutClipboard.length > 0 && <button title="Colar no playhead · Ctrl+V" aria-label="Colar trecho no playhead" onClick={() => void pasteSelection()}><ClipboardPaste size={15} /></button>}
                  </div>
                </div>}
                <div className="panel-heading">
                  <div><p className="eyebrow">MÍDIA DE REFERÊNCIA</p><h2>{media.has_video ? "Vídeo" : "Áudio"}</h2></div>
                  <div className="preview-actions">
                    <span className="media-tag">{media.has_video ? <FileVideo2 size={13} /> : <FileAudio2 size={13} />}{media.has_video ? "VÍDEO" : "ÁUDIO"}</span>
                    <button className="preview-toggle" onClick={() => setPreviewVisible((visible) => !visible)} aria-expanded={previewVisible} aria-label={previewVisible ? "Recolher vídeo" : "Mostrar vídeo"} title={previewVisible ? "Recolher vídeo" : "Mostrar vídeo"}>
                      {previewVisible ? <><ChevronUp size={14} /> Recolher</> : <><ChevronDown size={14} /> Mostrar vídeo</>}
                    </button>
                  </div>
                </div>
                <div className={`player-frame compact-player ${media.has_video ? "" : "audio-frame"}`}>
                  {media.has_video ? (
                    <video
                      ref={(node) => { mediaRef.current = node; }}
                      src={`${api.base}/api/media/${media.id}/stream`}
                      muted={usingEnhancedPlayback}
                      onTimeUpdate={handlePlaybackTime}
                      onSeeked={handlePlaybackTime}
                      onPlay={() => { if (!usingEnhancedPlayback) setPlaying(true); }}
                      onPause={() => { if (!usingEnhancedPlayback) setPlaying(false); }}
                      playsInline
                    />
                  ) : (
                    <div className="audio-art"><div className="audio-art-icon"><Mic2 size={32} /></div><div><strong>{media.original_filename}</strong><span>Áudio original preservado</span></div></div>
                  )}
                  {!media.has_video && <audio ref={(node) => { mediaRef.current = node; }} src={`${api.base}/api/media/${media.id}/stream`} muted={usingEnhancedPlayback} controls={!usingEnhancedPlayback} onTimeUpdate={handlePlaybackTime} onSeeked={handlePlaybackTime} onPlay={() => { if (!usingEnhancedPlayback) setPlaying(true); }} onPause={() => { if (!usingEnhancedPlayback) setPlaying(false); }} />}
                  {previewUrl && <audio ref={enhancedAudioRef} className="enhanced-playback-audio" preload="auto" src={previewUrl} onTimeUpdate={handleEnhancedPlaybackTime} onSeeked={handleEnhancedPlaybackTime} onPlay={() => { if (usingEnhancedPlayback) setPlaying(true); }} onPause={() => { if (usingEnhancedPlayback) setPlaying(false); }} onEnded={() => {
                    if (media.has_video) mediaRef.current?.pause();
                    setPlaying(false);
                  }} />}
                </div>
                <div className="media-sequence-list">
                  <div className="media-sequence-heading"><span>MÍDIAS DO PROJETO</span><small>{sequenceItems.length}</small></div>
                  <div className="media-sequence-items">
                    {sequenceItems.map((item, index) => <div className={`media-sequence-item ${project.id === item.project_id ? "media-sequence-item-active" : ""}`} key={item.project_id}>
                      <button className="media-sequence-select" onClick={() => void activateSequenceProject(item.project_id)} aria-current={project.id === item.project_id ? "true" : undefined} title={`Abrir ${item.name}`}>
                        <span className="media-sequence-number">{index + 1}</span>
                        {item.has_video ? <FileVideo2 size={14} /> : <FileAudio2 size={14} />}
                        <span className="media-sequence-name">{item.name}</span>
                      </button>
                      <div className="media-sequence-order">
                        <button onClick={() => void moveSequenceItem(item.project_id, -1)} disabled={index === 0} aria-label={`Mover ${item.name} para cima`} title="Mover para cima"><ChevronUp size={13} /></button>
                        <button onClick={() => void moveSequenceItem(item.project_id, 1)} disabled={index === sequenceItems.length - 1} aria-label={`Mover ${item.name} para baixo`} title="Mover para baixo"><ChevronDown size={13} /></button>
                      </div>
                    </div>)}
                  </div>
                  <button className="add-sequence-media" onClick={() => sequenceUploadRef.current?.click()} disabled={Boolean(busy)}><Plus size={14} /> Adicionar mídia</button>
                  <input ref={sequenceUploadRef} className="visually-hidden" type="file" accept="audio/*,video/*,.mkv" onChange={(event) => {
                    void handleAddSequenceMedia(event.target.files?.[0]);
                    event.target.value = "";
                  }} />
                </div>
              </section>

              <section className="transcript-panel primary-transcript panel">
                <div className="panel-heading transcript-heading">
                  <div><p className="eyebrow">EDIÇÃO POR TEXTO</p><h2>Transcrição</h2></div>
                  {hasSequenceTranscript && <span className="word-count">{sequenceProjects.length > 1 ? sequenceWordCount : words.length} palavras</span>}
                </div>
                {sequenceProjects.length <= 1 && !hasSequenceTranscript ? (
                  <div className="transcript-empty">
                    <div className="empty-icon"><Mic2 size={21} /></div>
                    <h3>Transforme a fala em texto</h3>
                    <p>{transcriptionProvider === "local"
                      ? "Processamento local, sem enviar a mídia. As palavras ficam sincronizadas com a gravação."
                      : "O áudio será enviado ao provedor escolhido. Confira os termos, limites e a política de privacidade do serviço."}</p>
                    <TranscriptionProviderCards value={transcriptionProvider} onChange={(value) => {
                      setTranscriptionProvider(value);
                    }} />
                    {transcriptionProvider === "local" ? (
                      <label className="model-select"><span>Modelo de voz</span><select value={transcriptionModel} onChange={(event) => setTranscriptionModel(event.target.value)}><option value="tiny">Tiny · mais rápido</option><option value="base">Base</option><option value="small">Small · recomendado</option><option value="medium">Medium · mais preciso</option></select></label>
                    ) : <div className="provider-key-status">{providerKeys[transcriptionProvider] ? "Chave configurada" : <button className="text-action" onClick={() => setWorkspaceView("settings")}>Configurar chave de API</button>}</div>}
                    <button className="button primary" onClick={() => void runTranscription()} disabled={Boolean(busy)}>
                      {busy === "transcribe" ? <LoaderCircle className="spin" size={15} /> : <Sparkles size={15} />}
                      {busy === "transcribe" ? "Transcrevendo…" : "Transcrever gravação"}
                    </button>
                    {transcriptionProvider === "local" && !transcriptionAvailable && <small>Requer faster-whisper instalado no ambiente local.</small>}
                    {transcriptionProvider !== "local" && <small>A chave configurada fica salva neste navegador e é enviada ao provedor apenas ao transcrever.</small>}
                  </div>
                ) : (
                  <>
                    <div className="transcript-scroll">
                      <div className="transcript-copy">
                        {sequenceProjects.length <= 1 && project.transcript.length > 0 && words.length === 0 && <p className="sequence-no-kept-text">As palavras desta mídia foram removidas da edição.</p>}
                        {sequenceProjects.length > 1 ? sequenceProjects.map((sequenceProject, sequenceIndex) => {
                          const sequenceWords = getOrderedKeptWords(sequenceProject);
                          const sequenceTopics = getTopicPositions(sequenceProject.topic_markers ?? [], sequenceWords);
                          const sequenceSilences = getSilencePositions(sequenceWords, sequenceProject.edl.kept);
                          const isActiveSequence = sequenceProject.id === project.id;
                          const sequenceName = sequenceItems.find((item) => item.project_id === sequenceProject.id)?.name ?? sequenceProject.name;
                          return <section className={`sequence-transcript-segment ${isActiveSequence ? "sequence-transcript-segment-active" : ""}`} key={sequenceProject.id}>
                            <button className="sequence-transcript-heading" onClick={() => void activateSequenceProject(sequenceProject.id)} aria-current={isActiveSequence ? "true" : undefined}>
                              <span className="sequence-transcript-heading-title"><small>MÍDIA {sequenceIndex + 1}</small><strong>{sequenceName}</strong></span>
                              <small>{isActiveSequence ? "Em edição" : "Abrir mídia"}</small>
                            </button>
                            <div className="sequence-transcript-copy">
                              {sequenceProject.transcript.length === 0 ? <div className="sequence-transcript-empty">
                                <div>
                                  <p>Esta mídia ainda não tem transcrição.</p>
                                  <TranscriptionProviderCards value={transcriptionProvider} onChange={(value) => {
                                    setTranscriptionProvider(value);
                                  }} />
                                  {transcriptionProvider === "local" ? <label className="model-select"><span>Modelo</span><select value={transcriptionModel} onChange={(event) => setTranscriptionModel(event.target.value)}><option value="tiny">Tiny · mais rápido</option><option value="base">Base</option><option value="small">Small · recomendado</option><option value="medium">Medium · mais preciso</option></select></label> : null}
                                  {transcriptionProvider === "local" && !transcriptionAvailable && <small>Requer faster-whisper instalado no ambiente local.</small>}
                                  {transcriptionProvider !== "local" && <div className="provider-key-status">{providerKeys[transcriptionProvider] ? "Chave configurada" : <button className="text-action" onClick={() => setWorkspaceView("settings")}>Configurar chave de API</button>}</div>}
                                </div>
                                <button className="button primary" onClick={() => void runTranscription(sequenceProject.id)} disabled={Boolean(busy)}>
                                  {busy === "transcribe" && transcriptionTargetId === sequenceProject.id ? <LoaderCircle className="spin" size={14} /> : <Sparkles size={14} />}
                                  {busy === "transcribe" && transcriptionTargetId === sequenceProject.id ? "Transcrevendo…" : "Transcrever esta mídia"}
                                </button>
                              </div> : sequenceWords.length === 0 ? <p className="sequence-no-kept-text">As palavras desta mídia foram removidas da edição.</p> : sequenceWords.map((word, index) => {
                                const silence = sequenceSilences.get(index);
                                const silenceDuration = silence ? silence.end - silence.start : 0;
                                return <Fragment key={word.id}>
                                  {isActiveSequence && silence && <span className="silence-marker">
                                    <button className="silence-marker-play" onClick={() => seekTo(silence.start)} title={`Ouvir pausa de ${silenceDuration.toLocaleString("pt-BR", { maximumFractionDigits: 1 })} segundos`}>
                                      <VolumeX size={14} /><span>Silêncio</span><strong>{silenceDuration.toLocaleString("pt-BR", { minimumFractionDigits: 1, maximumFractionDigits: 1 })} s</strong><small>{formatTime(silence.start)}–{formatTime(silence.end)}</small>
                                    </button>
                                    <button className="silence-marker-remove" onClick={() => void removeSilence(silence)} title="Remover silêncio da edição" aria-label={`Remover silêncio entre ${formatTime(silence.start)} e ${formatTime(silence.end)}`}><Trash2 size={13} /></button>
                                  </span>}
                                  {isActiveSequence && (sequenceTopics.get(index) ?? []).map((marker) => <div className="topic-marker" key={marker.id}>
                                    <span className="topic-marker-label"><BookmarkPlus size={13} />{marker.title}<small>{formatTime(marker.time)}</small></span>
                                    <button className="topic-marker-action" onClick={() => void editTopicMarker(marker)} title="Renomear tópico" aria-label={`Renomear tópico ${marker.title}`}><Pencil size={12} /></button>
                                    <button className="topic-marker-action" onClick={() => void removeTopicMarker(marker)} title="Remover separador" aria-label={`Remover tópico ${marker.title}`}><Trash2 size={12} /></button>
                                  </div>)}
                                  <button
                                    className={`word ${isActiveSequence && selected.includes(word.id) ? "selected" : ""} ${isActiveSequence && activeWord === word.id ? "playing" : ""}`}
                                    aria-pressed={isActiveSequence && selected.includes(word.id)}
                                    onClick={(event) => {
                                      if (isActiveSequence) selectWord(word.id, event.shiftKey);
                                      else void activateSequenceProject(sequenceProject.id);
                                    }}
                                    onDoubleClick={() => {
                                      void activateSequenceProject(sequenceProject.id, word.start);
                                    }}
                                    title={isActiveSequence ? `${formatTime(word.start)} · clique para selecionar · duplo clique posiciona o player` : `Abrir ${sequenceName} para navegar por este trecho`}
                                  >{word.text}</button>{index < sequenceWords.length - 1 ? " " : ""}
                                </Fragment>;
                              })}
                              {isActiveSequence && (sequenceTopics.get(sequenceWords.length) ?? []).map((marker) => <div className="topic-marker" key={marker.id}>
                                <span className="topic-marker-label"><BookmarkPlus size={13} />{marker.title}<small>{formatTime(marker.time)}</small></span>
                                <button className="topic-marker-action" onClick={() => void editTopicMarker(marker)} title="Renomear tópico" aria-label={`Renomear tópico ${marker.title}`}><Pencil size={12} /></button>
                                <button className="topic-marker-action" onClick={() => void removeTopicMarker(marker)} title="Remover separador" aria-label={`Remover tópico ${marker.title}`}><Trash2 size={12} /></button>
                              </div>)}
                            </div>
                          </section>;
                        }) : <>
                        {words.map((word, index) => {
                          const silence = silencePositions.get(index);
                          const silenceDuration = silence ? silence.end - silence.start : 0;
                          return <Fragment key={word.id}>
                            {silence && <span className="silence-marker">
                              <button
                                className="silence-marker-play"
                                onClick={() => seekTo(silence.start)}
                                title={`Ouvir pausa de silêncio de ${silenceDuration.toLocaleString("pt-BR", { maximumFractionDigits: 1 })} segundos`}
                                aria-label={`Pausa de silêncio de ${silenceDuration.toLocaleString("pt-BR", { maximumFractionDigits: 1 })} segundos, entre ${formatTime(silence.start)} e ${formatTime(silence.end)}. Clique para ouvir.`}
                              >
                                <VolumeX size={14} />
                                <span>Silêncio</span>
                                <strong>{silenceDuration.toLocaleString("pt-BR", { minimumFractionDigits: 1, maximumFractionDigits: 1 })} s</strong>
                                <small>{formatTime(silence.start)}–{formatTime(silence.end)}</small>
                              </button>
                              <button className="silence-marker-remove" onClick={() => void removeSilence(silence)} title="Remover silêncio da edição" aria-label={`Remover silêncio entre ${formatTime(silence.start)} e ${formatTime(silence.end)}`}><Trash2 size={13} /></button>
                            </span>}
                            {(topicPositions.get(index) ?? []).map((marker) => <div className="topic-marker" key={marker.id}>
                              <span className="topic-marker-label"><BookmarkPlus size={13} />{marker.title}<small>{formatTime(marker.time)}</small></span>
                              <button className="topic-marker-action" onClick={() => void editTopicMarker(marker)} title="Renomear tópico" aria-label={`Renomear tópico ${marker.title}`}><Pencil size={12} /></button>
                              <button className="topic-marker-action" onClick={() => void removeTopicMarker(marker)} title="Remover separador" aria-label={`Remover tópico ${marker.title}`}><Trash2 size={12} /></button>
                            </div>)}
                            <button
                              className={`word ${selected.includes(word.id) ? "selected" : ""} ${activeWord === word.id ? "playing" : ""}`}
                              data-word-id={word.id}
                              aria-pressed={selected.includes(word.id)}
                              onMouseDown={(event) => {
                                if (event.button !== 0) return;
                                isDraggingSelection.current = true;
                                selectWord(word.id, event.shiftKey);
                              }}
                              onMouseEnter={(event) => {
                                if (isDraggingSelection.current && event.buttons === 1) extendSelectionTo(word.id);
                              }}
                              onDoubleClick={() => seekTo(word.start)}
                              title={`${formatTime(word.start)} · arraste para selecionar um trecho · Shift+clique amplia · duplo clique posiciona o player`}
                            >{word.text}</button>{index < words.length - 1 ? " " : ""}
                          </Fragment>
                        })}
                        {(topicPositions.get(words.length) ?? []).map((marker) => <div className="topic-marker" key={marker.id}>
                          <span className="topic-marker-label"><BookmarkPlus size={13} />{marker.title}<small>{formatTime(marker.time)}</small></span>
                          <button className="topic-marker-action" onClick={() => void editTopicMarker(marker)} title="Renomear tópico" aria-label={`Renomear tópico ${marker.title}`}><Pencil size={12} /></button>
                          <button className="topic-marker-action" onClick={() => void removeTopicMarker(marker)} title="Remover separador" aria-label={`Remover tópico ${marker.title}`}><Trash2 size={12} /></button>
                        </div>)}
                        </>}
                      </div>
                    </div>
                    <div className="transcript-footer">
                      <span>{selected.length > 0 ? <><Clock3 size={13} /> {selected.length} palavra{selected.length === 1 ? "" : "s"} · {formatTime(selectionRange!.start)}–{formatTime(selectionRange!.end)} no original · Ctrl+C/X/V</> : <><Clock3 size={13} /> Arraste para selecionar · Shift+clique amplia · Ctrl+C/X/V copia, recorta e cola · duplo clique posiciona o player</>}</span>
                    </div>
                  </>
                )}
                {selectionPopup && selected.length > 0 && <div className="selection-popover" role="toolbar" aria-label="Ações do trecho selecionado" style={{ left: selectionPopup.left, top: selectionPopup.top }}>
                  <span>{selected.length} palavra{selected.length === 1 ? "" : "s"}</span>
                  <button onMouseDown={(event) => event.preventDefault()} onClick={() => void previewSelection()} title="Ouvir o trecho selecionado"><Play size={14} /> Ouvir</button>
                  <button onMouseDown={(event) => event.preventDefault()} onClick={openFocusedTimeline} title="Abrir a forma de onda focada no trecho"><AudioLines size={14} /> Abrir onda</button>
                  {selected.length === 1 && <button onMouseDown={(event) => event.preventDefault()} onClick={() => {
                    const word = words.find((item) => item.id === selected[0]);
                    if (word) void correctWord(word);
                  }} title="Corrigir o texto da palavra"><Pencil size={14} /> Corrigir</button>}
                  <button onMouseDown={(event) => event.preventDefault()} onClick={copySelection} title="Copiar trecho · Ctrl+C"><Copy size={14} /> Copiar</button>
                  <button onMouseDown={(event) => event.preventDefault()} onClick={() => void cutSelection()} title="Recortar trecho · Ctrl+X"><Scissors size={14} /> Recortar</button>
                  <button className="selection-popover-remove" onMouseDown={(event) => event.preventDefault()} onClick={() => void deleteSelection()} title="Remover trecho da edição"><Trash2 size={14} /> Remover</button>
                </div>}
              </section>
            </div>

            {openMenu && <div className={`editor-menu editor-menu-${openMenu}`} role="dialog" aria-label={openMenu === "enhance" ? "Melhorar voz" : "Exportação"}>
              <button className="editor-menu-close" onClick={() => setOpenMenu(null)} aria-label="Fechar menu"><X size={16} /></button>
              {openMenu === "enhance" ? <section className="enhance-panel panel">
                <div className="panel-heading">
                  <div className="heading-inline"><div className="feature-icon"><WandSparkles size={17} /></div><div><p className="eyebrow">PROCESSAMENTO DE ÁUDIO</p><h2>Melhorar voz</h2></div></div>
                  <span className="local-badge"><span /> LOCAL</span>
                </div>
                <div className="preset-row"><span>Predefinição aplicada: <strong className="preset-current">{appliedPresetName}</strong></span><div className="preset-actions"><select value={presets[appliedPresetName] ? appliedPresetName : "__custom__"} onChange={(event) => {
                  const preset = presets[event.target.value];
                  if (preset) setEnhancement(preset);
                }}><option value="__custom__" disabled>Personalizada</option>{Object.keys(presets).map((name) => <option key={name} value={name}>{name}</option>)}</select><button className="save-preset" onClick={() => void savePreset()}>Salvar atual</button></div></div>
                <div className="sliders-grid">
                  <Slider label="Ganho de entrada" value={enhancement.input_gain_db} min={-18} max={12} unit=" dB" onChange={(value) => setEnhancement({ ...enhancement, input_gain_db: value })} />
                  <Slider label="Filtro de graves" value={enhancement.highpass_hz} min={20} max={400} unit=" Hz" onChange={(value) => setEnhancement({ ...enhancement, highpass_hz: value })} />
                  <Slider label="Redução de ruído" value={enhancement.noise_reduction} min={0} max={100} unit="%" onChange={(value) => setEnhancement({ ...enhancement, noise_reduction: value })} />
                  <Slider label="Filtro de agudos" value={enhancement.lowpass_hz} min={8000} max={20000} unit=" Hz" onChange={(value) => setEnhancement({ ...enhancement, lowpass_hz: value })} />
                  <Slider label="Clareza de voz" value={enhancement.presence_eq_db} min={-8} max={8} unit=" dB" onChange={(value) => setEnhancement({ ...enhancement, presence_eq_db: value })} />
                  <Slider label="Porta de ruído" value={enhancement.speech_gate} min={0} max={100} unit="%" onChange={(value) => setEnhancement({ ...enhancement, speech_gate: value })} />
                  <Slider label="Compressão" value={enhancement.compressor_amount} min={0} max={100} unit="%" onChange={(value) => setEnhancement({ ...enhancement, compressor_amount: value })} />
                  <Slider label="De-esser" value={enhancement.deesser_mix} min={0} max={100} unit="%" onChange={(value) => setEnhancement({ ...enhancement, deesser_mix: value })} />
                  <Slider label="Nível de saída" value={enhancement.output_gain_db} min={-12} max={12} unit=" dB" onChange={(value) => setEnhancement({ ...enhancement, output_gain_db: value })} />
                  <Slider label="Alvo de loudness" value={enhancement.loudnorm_target} min={-34} max={-12} unit=" LUFS" onChange={(value) => setEnhancement({ ...enhancement, loudnorm_target: value })} />
                </div>
                <div className="enhance-actions">
                  <label className="enhancement-switch">
                    <input type="checkbox" checked={enhancementEnabled} onChange={(event) => setEnhancementEnabled(event.target.checked)} />
                    <span className="switch-track"><span /></span>
                    <span>{enhancementEnabled ? "Melhoria de voz ativada" : "Melhoria de voz desativada"}</span>
                  </label>
                  <span>{activeEnhancement ? <><LoaderCircle className="spin" size={13} /> Preparando prévia completa…</> : usingEnhancedPlayback ? "Reproduzindo com melhorias" : "Áudio original"}</span>
                </div>
              </section> : <section className="export-panel panel">
                <div className="panel-heading">
                  <div className="heading-inline"><div className="feature-icon export-icon"><ArrowDownToLine size={17} /></div><div><p className="eyebrow">ARQUIVO FINAL</p><h2>Exportação</h2></div></div>
                  <span className="media-tag"><Check size={12} /> SINCRONIZADO</span>
                </div>
                <div className="export-options">
                  <label><span>Formato</span><select value={format} onChange={(event) => {
                    setFormat(event.target.value);
                    if (event.target.value === "raw") setIncludeEnhancement(false);
                  }}>
                    {(media.has_video ? ["mp4", "webm", "wav", "mp3", "m4a", "aac", "raw"] : ["wav", "mp3", "m4a", "aac", "raw"]).map((item) => <option key={item} value={item}>{item === "raw" ? `RAW · sem perdas (${media.has_video ? "MKV" : "FLAC"})` : item.toUpperCase()}</option>)}
                  </select></label>
                  {(format === "mp4" || format === "webm") && <label><span>Qualidade</span><select value={quality} onChange={(event) => setQuality(Number(event.target.value))}><option value={18}>Alta</option><option value={23}>Padrão</option><option value={30}>Compacta</option></select></label>}
                </div>
                <label className="enhancement-toggle"><input type="checkbox" checked={includeEnhancement && format !== "raw"} disabled={format === "raw"} onChange={(event) => setIncludeEnhancement(event.target.checked)} /> Aplicar tratamento de voz na exportação</label>
                <p className="export-note">{format === "raw"
                  ? `RAW não aplica tratamento nem adiciona marca d’água. Sem cortes, baixa uma cópia idêntica da mídia; com cortes, usa codecs sem perdas${media.has_video ? " (FFV1 + FLAC em MKV)" : " (FLAC)"}. Marcas já presentes no original continuam nele.`
                  : "O arquivo inclui apenas os trechos mantidos na transcrição; MP4, WebM e formatos comprimidos podem reencodar a mídia."}</p>
                <button className="button primary export-button" onClick={() => void runExport()} disabled={Boolean(busy)}>
                  {busy === "export" ? <LoaderCircle className="spin" size={16} /> : <ArrowDownToLine size={16} />}
                  {busy === "export" ? "Renderizando…" : `Exportar ${format.toUpperCase()}`}
                </button>
                {busy && progress > 0 && <div className="progress-track"><span style={{ width: `${Math.round(progress * 100)}%` }} /></div>}
                {exportUrl && <a className="download-link" href={exportUrl}><ArrowDownToLine size={14} /> Baixar arquivo exportado</a>}
                <div className="transcript-export">
                  <div><strong>Transcrição com tópicos</strong><span>Arquivo de texto com timestamps por seção</span></div>
                  <button className="button subtle" onClick={exportTranscript} disabled={!sequenceWordCount}><FileText size={14} /> Exportar .TXT</button>
                </div>
              </section>}
            </div>}
            </>}

            <div className="playback-dock" role="region" aria-label="Controles de reprodução">
              <button className="dock-play" onClick={() => {
                void togglePlayback();
              }} aria-label={playing ? "Pausar reprodução" : "Reproduzir"}>
                {playing ? <Pause size={18} /> : <Play size={18} />}
              </button>
              <span className="dock-time">{formatTime(editedCurrentTime)}</span>
              <input className="playback-seek" type="range" min="0" max={editedDuration} step="0.05" value={Math.min(editedCurrentTime, editedDuration)} onChange={(event) => {
                if (project) seekTo(originalTimeAt(Number(event.target.value), project.edl.kept));
              }} aria-label="Posição da reprodução" style={{ background: `linear-gradient(to right, var(--accent) 0%, var(--accent) ${editedDuration ? (editedCurrentTime / editedDuration) * 100 : 0}%, var(--surface-3) ${editedDuration ? (editedCurrentTime / editedDuration) * 100 : 0}%, var(--surface-3) 100%)` }} />
              <span className="dock-time">{formatTime(editedDuration)}</span>
              <select className="dock-speed" aria-label="Velocidade de reprodução" value={playbackRate} onChange={(event) => {
                const rate = Number(event.target.value);
                setPlaybackRate(rate);
                if (mediaRef.current) mediaRef.current.playbackRate = rate;
                if (enhancedAudioRef.current) enhancedAudioRef.current.playbackRate = rate;
              }}><option value={0.75}>0.75×</option><option value={1}>1×</option><option value={1.25}>1.25×</option><option value={1.5}>1.5×</option><option value={2}>2×</option></select>
            </div>
          </>
        )}
      </main>
      {(error || message) && <div className={`toast ${error ? "error-toast" : ""}`}>
        <span>{error || message}</span>
        {error && <button onClick={() => setError("")}>Fechar</button>}
      </div>}
      <footer className="statusbar"><span><span className="status-dot" /> Processamento local</span><span className="status-actions">{busy ? message || "Trabalhando…" : "Salvo automaticamente"}{activeJobId && <button className="cancel-job" onClick={() => void cancelActiveJob()}>Cancelar</button>}</span></footer>
    </div>
  );
}

function Slider({ label, value, min, max, unit, onChange }: {
  label: string;
  value: number;
  min: number;
  max: number;
  unit: string;
  onChange: (value: number) => void;
}) {
  return <label className="slider-control">
    <span>{label}<output>{value}{unit}</output></span>
    <input type="range" min={min} max={max} step={1} value={value} onChange={(event) => onChange(Number(event.target.value))} />
  </label>;
}
