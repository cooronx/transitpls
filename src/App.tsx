import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { confirm as confirmDialog, open } from "@tauri-apps/plugin-dialog";
import { openPath } from "@tauri-apps/plugin-opener";
import {
  ArrowRight,
  Braces,
  Check,
  CheckCircle2,
  ChevronDown,
  ChevronUp,
  CircleDashed,
  Clock3,
  Copy,
  Download,
  Eye,
  EyeOff,
  FolderKanban,
  Languages,
  LibraryBig,
  LoaderCircle,
  Minus,
  PanelTop,
  Play,
  Plus,
  RotateCcw,
  Search,
  Settings,
  SlidersHorizontal,
  Square,
  SquareStop,
  Trash2,
  X,
  type LucideIcon,
} from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import "./App.css";
import providerPresets from "./provider-presets.json";

type Status = "initialized" | "translating" | "translated" | "failed";
type ItemStatus = "pending" | "translated" | "failed";
type ExportOptions = {
  bilingual: boolean;
  order?: "target-first" | "source-first";
};
type PolishStatus = "pending" | "succeeded" | "failed";
interface Project {
  id: string;
  title: string;
  source_file: string;
  source_path: string;
  source_language: string;
  target_language: string;
  status: Status;
  chapters_total: number;
  chapters_completed: number;
  updated_at: string;
  cover_data_url?: string | null;
  task_initialized?: boolean;
}
interface Segment {
  id: string;
  ordinal: number;
  source: string;
  target: string | null;
  target_before_polish?: string | null;
  polish_status?: PolishStatus | null;
  kind: string;
  status: ItemStatus;
  meta?: Record<string, unknown>;
}
interface Chapter {
  id: string;
  title: string;
  target_title?: string;
  status: ItemStatus;
  segments: Segment[];
  meta?: Record<string, unknown>;
}
type TermPolicy = "automatic" | "fixed" | "non_fixed" | "ignored";
interface Term {
  source: string;
  target: string;
  type: string;
  aliases: string[];
  first_chapter: number;
  note?: string;
  status: "ok" | "conflict" | "resolved";
  policy: TermPolicy;
  manual_target?: string | null;
}
interface ConflictCandidate {
  target: string;
  occurrences: number;
  chapters: number[];
  evidence: Array<{ chapter: number; source_excerpt: string; target_excerpt: string }>;
}
interface TermConflict {
  source: string;
  current_target: string;
  policy: TermPolicy;
  manual_target?: string | null;
  unresolved_events: number;
  resolved_events: number;
  candidates: ConflictCandidate[];
}
interface AffectedContent {
  id: string;
  chapterId: string;
  chapter: number;
  kind: string;
  source: string;
  currentTarget: string;
  previousTarget?: string | null;
  retranslationError?: string | null;
}
interface RetranslationProgress {
  projectId: string;
  completed: number;
  total: number;
  succeeded: number;
  failed: number;
  itemId: string;
  detail: Detail;
}
interface TranslationTiming {
  projectId: string;
  startedAt: number;
  finishedAt?: number;
  completedRequests: number;
  totalRequests: number;
  maxCharsPerBatch: number;
  chapterId?: string;
}
interface LogEntry {
  timestamp: string;
  event: string;
  details: unknown;
}
interface Config {
  language: { source: string; target: string };
  llm: {
    provider: string;
    model: string;
    api_key_env: string;
    base_url?: string;
    timeout_secs: number;
    max_retries: number;
  };
  segment: { max_chars_per_segment: number; max_chars_per_batch: number };
  pipeline: { polish: boolean; recent_context_chars: number };
  analysis: { full_book: boolean };
  general: {
    visible_segments: number;
    retranslation_concurrency: number;
    polish_concurrency: number;
  };
}
interface TaskConfigDraft {
  sourceLanguage: string;
  maxCharsPerSegment: number;
  maxCharsPerBatch: number;
  recentContextChars: number;
  timeoutSecs: number;
  maxRetries: number;
  fullBook: boolean;
}
interface CredentialStatus {
  configured: boolean;
  source?: "environment" | "desktop" | "none";
}
interface Bootstrap {
  config: Config;
  credential: CredentialStatus;
  configPath?: string;
  stateDir: string;
  projects: Project[];
}
interface PolishSummary {
  roundId: string;
  finished: boolean;
  total: number;
  succeeded: number;
  failed: number;
  pending: number;
  pendingSegments: number;
  lastError?: string | null;
  updatedAt: string;
}
interface Detail {
  project: Project;
  taskInitialized: boolean;
  chapters: Chapter[];
  logs: LogEntry[];
  terms: Term[];
  conflicts: Array<{ source: string; target: string; chapter: number }>;
  termConflicts: TermConflict[];
  pendingConflicts: number;
  report?: unknown;
  polish: PolishSummary | null;
}
type View =
  | "workspace"
  | "projects"
  | "terms"
  | "settings"
  | "history";
type TrayName = "tasks" | "issues" | "logs";

const navItems: Array<{ id: View; icon: LucideIcon; label: string }> = [
  { id: "workspace", icon: Languages, label: "工作台" },
  { id: "projects", icon: FolderKanban, label: "项目" },
  { id: "terms", icon: LibraryBig, label: "术语库" },
];
const statusText: Record<Status | ItemStatus, string> = {
  initialized: "待翻译",
  translating: "翻译中",
  translated: "已完成",
  failed: "失败",
  pending: "待处理",
};
const polishStatusText: Record<PolishStatus, string> = {
  pending: "待润色",
  succeeded: "已润色",
  failed: "润色失败",
};
const termTypeText: Record<string, string> = {
  person: "人名",
  place: "地名",
  organization: "组织机构",
  term: "术语",
  appellation: "称谓",
  speech: "语言习惯",
  fixed_expr: "固定表达",
};
const isMac =
  typeof navigator !== "undefined" && /Mac/i.test(navigator.userAgent);

export default function App() {
  const [exportFormat, setExportFormat] = useState<"txt" | "epub" | null>(null);
  const [bootstrap, setBootstrap] = useState<Bootstrap | null>(null);
  const [detail, setDetail] = useState<Detail | null>(null);
  const [chapterIndex, setChapterIndex] = useState(0);
  const [view, setView] = useState<View>("workspace");
  const [tray, setTray] = useState<TrayName>("tasks");
  const [busy, setBusy] = useState<string | null>(null);
  const [retranslationProgress, setRetranslationProgress] =
    useState<RetranslationProgress | null>(null);
  const [translationTiming, setTranslationTiming] =
    useState<TranslationTiming | null>(null);
  const [now, setNow] = useState(Date.now());
  const [notice, setNotice] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const [mockClient] = useState(false);
  const [visibleSegmentCount, setVisibleSegmentCount] = useState(100);
  const [displaySegmentCount, setDisplaySegmentCount] = useState(100);
  const [taskDraft, setTaskDraft] = useState<TaskConfigDraft | null>(null);
  const [inspectorOpen, setInspectorOpen] = useState(true);

  const reload = async (projectId?: string) => {
    const data = await invoke<Bootstrap>("ui_bootstrap");
    setBootstrap(data);
    const id = projectId ?? detail?.project.id ?? data.projects[0]?.id;
    if (!id) {
      setDetail(null);
      return;
    }
    const next = await invoke<Detail>("ui_project", { projectId: id });
    setDetail(next);
    setChapterIndex((current) =>
      Math.min(current, Math.max(0, next.chapters.length - 1)),
    );
  };
  const boot = () =>
    reload().catch((error) => {
      if (String(error).includes("invoke")) setBootstrap(browserPreview());
      else setNotice(String(error));
    });
  useEffect(() => {
    void boot();
  }, []);
  useEffect(() => {
    if (!isTauri()) return;
    let disposed = false;
    let stop: undefined | (() => void);
    listen<Detail>("translation-progress", ({ payload }) => {
      setTranslationTiming((current) => {
        if (
          !current ||
          current.finishedAt ||
          current.projectId !== payload.project.id
        )
          return current;
        const remaining = countTranslationRequests(
          payload.chapters,
          current.maxCharsPerBatch,
          current.chapterId,
        );
        return {
          ...current,
          completedRequests: Math.max(
            current.completedRequests,
            current.totalRequests - remaining,
          ),
        };
      });
      setDetail((current) =>
        current?.project.id === payload.project.id ? payload : current,
      );
      setBootstrap((current) =>
        current
          ? {
              ...current,
              projects: current.projects.map((project) =>
                project.id === payload.project.id
                  ? {
                      ...payload.project,
                      cover_data_url: project.cover_data_url,
                      task_initialized: project.task_initialized,
                    }
                  : project,
              ),
            }
          : current,
      );
    })
      .then((unlisten) => {
        if (disposed) unlisten();
        else stop = unlisten;
      })
      .catch((error) => {
        if (!String(error).includes("invoke")) setNotice(String(error));
      });
    return () => {
      disposed = true;
      stop?.();
    };
  }, []);
  useEffect(() => {
    if (!isTauri()) return;
    let disposed = false;
    let stop: undefined | (() => void);
    listen<RetranslationProgress>("retranslation-progress", ({ payload }) => {
      setRetranslationProgress(payload);
      setBusy(`重译 ${payload.completed}/${payload.total}`);
      setDetail((current) =>
        current?.project.id === payload.projectId ? payload.detail : current,
      );
      setBootstrap((current) =>
        current
          ? {
              ...current,
              projects: current.projects.map((project) =>
                project.id === payload.projectId
                  ? {
                      ...payload.detail.project,
                      cover_data_url: project.cover_data_url,
                      task_initialized: project.task_initialized,
                    }
                  : project,
              ),
            }
          : current,
      );
    })
      .then((unlisten) => {
        if (disposed) unlisten();
        else stop = unlisten;
      })
      .catch((error) => {
        if (!String(error).includes("invoke")) setNotice(String(error));
      });
    return () => {
      disposed = true;
      stop?.();
    };
  }, []);
  useEffect(() => {
    if (!isTauri()) return;
    let disposed = false;
    let stop: undefined | (() => void);
    listen<Detail>("polish-progress", ({ payload }) => {
      setDetail((current) =>
        current?.project.id === payload.project.id ? payload : current,
      );
      setBootstrap((current) =>
        current
          ? {
              ...current,
              projects: current.projects.map((project) =>
                project.id === payload.project.id
                  ? {
                      ...payload.project,
                      cover_data_url: project.cover_data_url,
                      task_initialized: project.task_initialized,
                    }
                  : project,
              ),
            }
          : current,
      );
    })
      .then((unlisten) => {
        if (disposed) unlisten();
        else stop = unlisten;
      })
      .catch((error) => {
        if (!String(error).includes("invoke")) setNotice(String(error));
      });
    return () => {
      disposed = true;
      stop?.();
    };
  }, []);
  useEffect(() => {
    const next = bootstrap?.config.general.visible_segments ?? 100;
    setVisibleSegmentCount(next);
    setDisplaySegmentCount(next);
  }, [bootstrap?.config.general.visible_segments]);
  useEffect(() => {
    if (!bootstrap?.config || !detail) {
      setTaskDraft(null);
      return;
    }
    setTaskDraft({
      sourceLanguage: detail.taskInitialized
        ? detail.project.source_language
        : bootstrap.config.language.source,
      maxCharsPerSegment: bootstrap.config.segment.max_chars_per_segment,
      maxCharsPerBatch: bootstrap.config.segment.max_chars_per_batch,
      recentContextChars: bootstrap.config.pipeline.recent_context_chars,
      timeoutSecs: bootstrap.config.llm.timeout_secs,
      maxRetries: bootstrap.config.llm.max_retries,
      fullBook: bootstrap.config.analysis.full_book,
    });
  }, [
    bootstrap?.config,
    detail?.project.id,
    detail?.project.source_language,
    detail?.taskInitialized,
  ]);
  useEffect(() => {
    if (busy !== "翻译") return;
    setNow(Date.now());
    const interval = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(interval);
  }, [busy]);
  useEffect(() => {
    if (busy === "翻译") return;
    setTranslationTiming((current) =>
      current && !current.finishedAt
        ? { ...current, finishedAt: Date.now() }
        : current,
    );
  }, [busy]);
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "f") {
        const input = document.querySelector<HTMLInputElement>(
          ".search-field input",
        );
        if (!input) return;
        event.preventDefault();
        input.focus();
        input.select();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  const run = async (label: string, action: () => Promise<Detail | void>) => {
    setBusy(label);
    setNotice(null);
    try {
      const result = await action();
      if (result) {
        setDetail(result);
        await reload(result.project.id);
      }
      setNotice(`${label}已完成`);
    } catch (error) {
      setNotice(String(error));
    } finally {
      setBusy(null);
    }
  };
  const importFile = async () => {
    const path = await open({
      multiple: false,
      filters: [{ name: "电子书", extensions: ["epub", "txt"] }],
    });
    if (path)
      await run("导入书籍", async () => {
        try {
          return await invoke<Detail>("ui_import", {
            input: path,
          });
        } catch (error) {
          try {
            await reload();
          } catch (reloadError) {
            throw new Error(
              `${String(error)}；刷新项目失败：${String(reloadError)}`,
            );
          }
          throw error;
        }
      });
  };
  const taskConfigArgs = (value: TaskConfigDraft) => ({ ...value });
  const persistTaskDraft = async (value: TaskConfigDraft | null) => {
    if (!value || !validTaskConfig(value)) return null;
    const next = await invoke<Bootstrap>(
      "ui_save_task_config",
      taskConfigArgs(value),
    );
    setBootstrap(next);
    return next;
  };
  const requireValidDraft = (value: TaskConfigDraft | null) => {
    if (value && !validTaskConfig(value)) {
      setNotice("任务配置中有未填写的数值，请修正后再开始");
      return false;
    }
    return true;
  };
  const initializeTask = () => {
    if (busy || !detail || detail.taskInitialized) return;
    if (!bootstrap?.credential.configured && !mockClient) {
      setView("settings");
      setNotice("请先在设置中配置并验证 API Key");
      return;
    }
    if (!requireValidDraft(taskDraft)) return;
    void run("项目初始化", async () => {
      await persistTaskDraft(taskDraft);
      return invoke<Detail>("ui_initialize", {
        projectId: detail.project.id,
        mockClient,
      });
    });
  };
  const reanalyze = async () => {
    if (busy || !detail || !detail.taskInitialized) return;
    if (!requireValidDraft(taskDraft)) return;
    const confirmed = await confirmDialog(
      "重新分析会再次调用模型，并刷新风格分析、章节摘要与全书梗概。继续吗？",
      { title: "重新分析", kind: "warning", okLabel: "重新分析", cancelLabel: "取消" },
    );
    if (!confirmed) return;
    void run("重新分析", async () => {
      await persistTaskDraft(taskDraft);
      return invoke<Detail>("ui_reanalyze", {
        projectId: detail.project.id,
        mockClient,
      });
    });
  };
  const selectProject = async (id: string) => {
    setBusy("加载项目");
    try {
      setDetail(await invoke<Detail>("ui_project", { projectId: id }));
      setChapterIndex(0);
      setView("workspace");
    } catch (error) {
      setNotice(String(error));
    } finally {
      setBusy(null);
    }
  };
  const deleteProject = async (project: Project) => {
    if (busy) return;
    const confirmed = await confirmDialog(
      `确定删除项目“${project.title}”吗？\n\n翻译进度、术语和日志将被永久删除，原始书籍文件不会受到影响。`,
      {
        title: "删除项目",
        kind: "warning",
        okLabel: "删除",
        cancelLabel: "取消",
      },
    );
    if (!confirmed) return;
    setBusy("删除项目");
    setNotice(null);
    try {
      const wasCurrent = detail?.project.id === project.id;
      const next = await invoke<Bootstrap>("ui_delete_project", {
        projectId: project.id,
      });
      setBootstrap(next);
      if (wasCurrent) {
        setDetail(null);
        const replacement = next.projects[0];
        if (replacement)
          setDetail(
            await invoke<Detail>("ui_project", { projectId: replacement.id }),
          );
        else setDetail(null);
        setChapterIndex(0);
      }
      setNotice(`项目“${project.title}”已删除，原始文件未改动`);
    } catch (error) {
      setNotice(String(error));
    } finally {
      setBusy(null);
    }
  };
  const startTranslation = (
    source: Detail,
    chapter: number | undefined,
    config?: Config,
  ) => {
    const maxCharsPerBatch =
      taskDraft?.maxCharsPerBatch ?? config?.segment.max_chars_per_batch ?? 1;
    const chapterId =
      chapter === undefined ? undefined : source.chapters[chapter]?.id;
    setTranslationTiming({
      projectId: source.project.id,
      startedAt: Date.now(),
      completedRequests: 0,
      totalRequests: countTranslationRequests(
        source.chapters,
        maxCharsPerBatch,
        chapterId,
      ),
      maxCharsPerBatch,
      chapterId,
    });
    return run("翻译", () =>
      invoke<Detail>("ui_transit", {
        projectId: source.project.id,
        chapter: chapter ?? null,
        mockClient,
      }),
    );
  };
  const beginTranslation = async (
    source: Detail,
    chapter: number | undefined,
    config?: Config,
  ) => {
    if (!requireValidDraft(taskDraft)) return;
    setBusy("保存任务配置");
    setNotice(null);
    try {
      const next = await persistTaskDraft(taskDraft);
      await startTranslation(source, chapter, next?.config ?? config);
    } catch (error) {
      setBusy(null);
      setNotice(String(error));
    }
  };
  const initializeThenTranslate = async (chapter?: number) => {
    if (!detail || !bootstrap) return;
    const draft = taskDraft ?? taskConfigFromConfig(bootstrap.config);
    if (!requireValidDraft(draft)) return;
    const confirmed = await confirmDialog(
      "项目尚未初始化。初始化会调用模型完成译前分析，可能消耗 API Token。\n\n是否现在开始初始化？初始化完成后将自动开始翻译。",
      {
        title: "开始翻译",
        kind: "info",
        okLabel: "初始化并翻译",
        cancelLabel: "取消",
      },
    );
    if (!confirmed) return;
    setBusy("项目初始化");
    setNotice(null);
    try {
      const next = await persistTaskDraft(draft);
      const initialized = await invoke<Detail>("ui_initialize", {
        projectId: detail.project.id,
        mockClient,
      });
      setDetail(initialized);
      await startTranslation(initialized, chapter, next?.config);
    } catch (error) {
      setBusy(null);
      setNotice(String(error));
    }
  };
  const translate = (chapter?: number) => {
    if (busy || !detail) return;
    if (!bootstrap?.credential.configured && !mockClient) {
      setView("settings");
      setNotice("请先在设置中配置并验证 API Key");
      return;
    }
    if (!detail.taskInitialized) {
      void initializeThenTranslate(chapter);
      return;
    }
    void beginTranslation(detail, chapter, bootstrap?.config);
  };
  const retranslateItems = async (itemIds: string[]) => {
    if (busy || !detail || !itemIds.length) return;
    const projectId = detail.project.id;
    setBusy(`重译 0/${itemIds.length}`);
    setRetranslationProgress({
      projectId,
      completed: 0,
      total: itemIds.length,
      succeeded: 0,
      failed: 0,
      itemId: "",
      detail,
    });
    setNotice(null);
    try {
      await persistTaskDraft(taskDraft);
      const next = await invoke<Detail>("ui_retranslate", {
        projectId,
        itemIds,
        mockClient,
      });
      setDetail(next);
      await reload(projectId);
      setNotice(`重译任务已完成，共处理 ${itemIds.length} 项`);
    } catch (error) {
      setNotice(String(error));
      throw error;
    } finally {
      setRetranslationProgress(null);
      setBusy(null);
    }
  };
  const startPolish = async (retryFailed: boolean) => {
    if (busy || !detail) return;
    if (!bootstrap?.credential.configured && !mockClient) {
      setView("settings");
      setNotice("请先在设置中配置并验证 API Key");
      return;
    }
    if (!requireValidDraft(taskDraft)) return;
    const projectId = detail.project.id;
    const label = retryFailed ? "重试润色" : "润色";
    setBusy(label);
    setNotice(null);
    try {
      await persistTaskDraft(taskDraft);
      const next = await invoke<Detail>("ui_polish", {
        projectId,
        retryFailed,
        mockClient,
      });
      setDetail(next);
      await reload(projectId);
      const failed = next.polish?.failed ?? 0;
      setNotice(
        failed > 0
          ? `润色结束，仍有 ${failed} 个批次失败，可重试失败批次`
          : "润色已完成",
      );
    } catch (error) {
      setNotice(String(error));
    } finally {
      setBusy(null);
    }
  };
  const saveModel = async (value: Config, apiKey: string) => {
    setBusy("验证模型");
    setNotice(null);
    try {
      const next = await invoke<Bootstrap>("ui_verify_and_save_model", {
        value,
        apiKey: apiKey || null,
      });
      setBootstrap(next);
      setNotice("连接验证成功，模型设置已保存");
    } catch (error) {
      setNotice(String(error));
    } finally {
      setBusy(null);
    }
  };
  const exportBook = async (format: "txt" | "epub", options: ExportOptions) => {
    if (!detail) return [];
    const polish = detail.polish;
    if (polish && !(polish.finished && polish.failed === 0)) {
      const confirmed = await confirmDialog(
        "润色尚未全部完成，导出的内容可能同时包含初稿和已润色译文。仍要导出吗？",
        { title: "导出提醒", kind: "warning", okLabel: "继续导出", cancelLabel: "取消" },
      );
      if (!confirmed) return [];
    }
    setBusy("导出");
    try {
      const output = await invoke<string>("ui_export", {
        projectId: detail.project.id,
        format,
        options,
      });
      setNotice(`已导出至 ${output}`);
      await openPath(output);
    } catch (error) {
      setNotice(String(error));
    } finally {
      setBusy(null);
    }
  };
  const saveGeneral = async (
    visibleSegments: number,
    retranslationConcurrency: number,
    polishConcurrency: number,
  ) => {
    setBusy("保存通用设置");
    setNotice(null);
    try {
      const next = await invoke<Bootstrap>("ui_save_general", {
        visibleSegments,
        retranslationConcurrency,
        polishConcurrency,
      });
      setBootstrap(next);
      setNotice("通用设置已保存");
    } catch (error) {
      setNotice(String(error));
    } finally {
      setBusy(null);
    }
  };
  const savePipeline = async (polish: boolean) => {
    if (busy) return;
    setBusy("保存润色设置");
    setNotice(null);
    try {
      const next = await invoke<Bootstrap>("ui_save_pipeline", { polish });
      setBootstrap(next);
      setNotice(`译后润色已${polish ? "开启" : "关闭"}`);
    } catch (error) {
      setNotice(String(error));
    } finally {
      setBusy(null);
    }
  };
  const cancelTask = async () => {
    const taskId =
      busy === "项目初始化" || busy === "重新分析"
        ? "initialize"
        : busy === "导入书籍"
          ? "import"
          : detail?.project.id;
    if (!taskId) return;
    const cancelled = await invoke<boolean>("ui_cancel_task", { taskId });
    if (cancelled) setNotice("正在取消任务，已完成的进度会保留");
  };
  const chapter = detail?.chapters[chapterIndex];
  const matchingSegments = useMemo(() => {
    if (!chapter) return [];
    const q = search.trim().toLowerCase();
    return q
      ? chapter.segments.filter(
          (s) =>
            s.source.toLowerCase().includes(q) ||
            s.target?.toLowerCase().includes(q),
        )
      : chapter.segments;
  }, [chapter, search]);
  const segments = useMemo(
    () => matchingSegments.slice(0, displaySegmentCount),
    [matchingSegments, displaySegmentCount],
  );
  useEffect(() => {
    setDisplaySegmentCount(visibleSegmentCount);
  }, [chapterIndex, search, visibleSegmentCount]);

  const activeRetranslation =
    retranslationProgress?.projectId === detail?.project.id
      ? retranslationProgress
      : null;
  const activeTiming =
    translationTiming?.projectId === detail?.project.id
      ? translationTiming
      : null;
  let busyPercent: number | null = null;
  let busyDetail: string | null = null;
  if (busy?.startsWith("重译") && activeRetranslation) {
    busyPercent = Math.round(
      (activeRetranslation.completed /
        Math.max(1, activeRetranslation.total)) *
        100,
    );
    busyDetail = `${activeRetranslation.completed} / ${activeRetranslation.total} 项 · 成功 ${activeRetranslation.succeeded} · 失败 ${activeRetranslation.failed}`;
  } else if (busy === "翻译" && activeTiming && activeTiming.totalRequests > 0) {
    busyPercent = Math.round(
      (activeTiming.completedRequests / activeTiming.totalRequests) * 100,
    );
    busyDetail = `${activeTiming.completedRequests} / ${activeTiming.totalRequests} 个批次`;
  } else if (
    (busy === "润色" || busy === "重试润色") &&
    detail?.polish &&
    detail.polish.total > 0
  ) {
    const done = detail.polish.succeeded + detail.polish.failed;
    busyPercent = Math.round((done / detail.polish.total) * 100);
    busyDetail = `${done} / ${detail.polish.total} 批 · 成功 ${detail.polish.succeeded} · 失败 ${detail.polish.failed}`;
  }
  const cancellable =
    busy === "翻译" ||
    busy === "润色" ||
    busy === "重试润色" ||
    busy === "项目初始化" ||
    busy === "重新分析" ||
    busy === "导入书籍" ||
    Boolean(busy?.startsWith("重译"));

  if (!bootstrap) {
    return (
      <div className="app-loading">
        <Logo />
        {notice ? (
          <>
            <b>无法加载工作台</b>
            <small>{notice}</small>
            <button
              type="button"
              className="btn btn-primary"
              onClick={() => {
                setNotice(null);
                void boot();
              }}
            >
              重试
            </button>
          </>
        ) : (
          <>
            <LoaderCircle className="spin" />
            <b>正在加载工作台…</b>
            <small>读取本地项目与配置</small>
          </>
        )}
      </div>
    );
  }

  return (
    <div className={`app-shell ${isMac ? "is-macos" : ""}`}>
      <TitleBar
        project={detail?.project}
        projects={bootstrap.projects}
        model={bootstrap.config.llm.model ?? "—"}
        view={view}
        onView={setView}
        onProject={(id) => void selectProject(id)}
        onTranslate={() => translate()}
        busy={busy}
        disabled={!detail}
      />
      <div className="app-body">
        {view === "workspace" && (
          <Explorer
            projects={bootstrap.projects}
            detail={detail}
            chapterIndex={chapterIndex}
            onChapter={setChapterIndex}
            onProject={selectProject}
            onImport={importFile}
          />
        )}
        <main className="main-panel">
          {view === "workspace" && detail && chapter && (
            <Workspace
              detail={detail}
              chapter={chapter}
              chapterIndex={chapterIndex}
              segments={segments}
              hasMoreSegments={matchingSegments.length > segments.length}
              onShowMore={() =>
                setDisplaySegmentCount((count) => count + visibleSegmentCount)
              }
              tray={tray}
              setTray={setTray}
              search={search}
              onSearch={setSearch}
              translating={busy === "翻译"}
              busy={Boolean(busy)}
              ready={detail.taskInitialized}
              inspectorOpen={inspectorOpen}
              onToggleInspector={() => setInspectorOpen((open) => !open)}
              onTranslate={() => translate(chapterIndex)}
              onExport={setExportFormat}
              onOpenTerms={() => setView("terms")}
            />
          )}
          {view === "projects" && (
            <ProjectGallery
              projects={bootstrap.projects}
              busy={Boolean(busy)}
              onSelect={selectProject}
              onDelete={deleteProject}
              onImport={importFile}
            />
          )}
          {view === "history" && (
            <ProjectGallery
              projects={bootstrap.projects}
              busy={Boolean(busy)}
              onSelect={selectProject}
              onImport={importFile}
            />
          )}
          {view === "terms" && (
            <TermsView
              detail={detail}
              taskBusy={Boolean(busy)}
              retranslationProgress={retranslationProgress}
              onRetranslate={retranslateItems}
              onReload={() => reload()}
            />
          )}
          {view === "settings" && (
            <SettingsView
              config={bootstrap.config}
              credential={bootstrap.credential}
              configPath={bootstrap.configPath}
              busy={busy}
              onSaveModel={saveModel}
              onSaveGeneral={saveGeneral}
            />
          )}
          {view === "workspace" && (!detail || !chapter) && (
            <EmptyState onImport={importFile} />
          )}
        </main>
        {view === "workspace" && detail && inspectorOpen && (
          <Inspector
            config={bootstrap.config}
            detail={detail}
            busy={busy}
            retranslationProgress={retranslationProgress}
            translationTiming={activeTiming}
            now={now}
            draft={taskDraft}
            onDraftChange={setTaskDraft}
            onPolish={savePipeline}
            onPolishStart={(retryFailed) => void startPolish(retryFailed)}
            onCancel={() => void cancelTask()}
            polishing={busy === "润色" || busy === "重试润色"}
            onInitialize={initializeTask}
            onReanalyze={reanalyze}
            onOpenTerms={() => setView("terms")}
            onOpenSettings={() => setView("settings")}
            onClose={() => setInspectorOpen(false)}
          />
        )}
      </div>
      <footer className="statusbar">
        <span>TransItPls</span>
        <i />
        <span>{bootstrap.projects.length} 个项目</span>
        <span className="status-spacer" />
        <span className={`status-state ${busy ? "busy" : ""}`}>
          {busy ? <LoaderCircle className="spin" /> : <i className="status-dot ok" />}
          {busy ? `${busy}…` : "就绪"}
        </span>
      </footer>
      {busy && busyPercent === null && <div className="busy-line" />}
      {busy && (
        <ActivityDock
          label={busy}
          detail={busyDetail}
          percent={busyPercent}
          cancellable={cancellable}
          onCancel={() => void cancelTask()}
        />
      )}
      {exportFormat && (
        <ExportDialog
          format={exportFormat}
          onClose={() => setExportFormat(null)}
          onExport={(options) => {
            setExportFormat(null);
            void exportBook(exportFormat, options);
          }}
        />
      )}
      {notice && (
        <div className="toast" role="status">
          <span>{notice}</span>
          <button
            className="toast-close"
            aria-label="关闭消息"
            onClick={() => setNotice(null)}
          >
            <X aria-hidden="true" />
          </button>
        </div>
      )}
    </div>
  );
}

function ExportDialog({ format, onClose, onExport }: {
  format: "txt" | "epub";
  onClose: () => void;
  onExport: (options: ExportOptions) => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [bilingual, setBilingual] = useState(false);
  const [order, setOrder] = useState<ExportOptions["order"]>("target-first");
  useEffect(() => { dialog.current?.showModal(); }, []);
  return (
    <dialog ref={dialog} className="export-dialog" onCancel={onClose} aria-labelledby="export-title">
      <form onSubmit={(event) => { event.preventDefault(); onExport(bilingual ? { bilingual, order } : { bilingual }); }}>
        <h2 id="export-title">导出 {format.toUpperCase()}</h2>
        <label className="field">
          <b>导出内容</b>
          <select value={bilingual ? "bilingual" : "target"} onChange={(event) => setBilingual(event.target.value === "bilingual")}>
            <option value="target">仅译文</option>
            <option value="bilingual">双语对照</option>
          </select>
        </label>
        {bilingual && (
          <label className="field">
            <b>段落顺序</b>
            <select value={order} onChange={(event) => setOrder(event.target.value as ExportOptions["order"])}>
              <option value="target-first">译文在前</option>
              <option value="source-first">原文在前</option>
            </select>
            <small>每段原文与当前译文上下排列，标题仅保留译文。</small>
          </label>
        )}
        <div className="export-dialog-actions">
          <button type="button" className="btn btn-quiet" onClick={onClose}>取消</button>
          <button type="submit" className="btn btn-primary"><Download />导出</button>
        </div>
      </form>
    </dialog>
  );
}

function Logo() {
  return <img className="logo-mark" src="/transitpls_icon.png" alt="" />;
}
function ProgressBar({
  value,
  label,
  detail,
  size = "md",
}: {
  value: number;
  label?: string;
  detail?: string;
  size?: "md" | "lg";
}) {
  const percent = Math.max(0, Math.min(100, Math.round(value)));
  return (
    <div className={`progress-block ${size}`}>
      {label && (
        <div className="progress-head">
          <span>{label}</span>
          <b>{percent}%</b>
        </div>
      )}
      <div
        className="progress-track"
        role="progressbar"
        aria-valuenow={percent}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-label={label ?? "进度"}
      >
        <i style={{ width: `${percent}%` }} />
      </div>
      {detail && <small>{detail}</small>}
    </div>
  );
}
function ActivityDock({
  label,
  detail,
  percent,
  cancellable,
  onCancel,
}: {
  label: string;
  detail: string | null;
  percent: number | null;
  cancellable: boolean;
  onCancel: () => void;
}) {
  return (
    <div className="activity-dock" role="status" aria-live="polite">
      <LoaderCircle className="spin" aria-hidden="true" />
      <div className="activity-info">
        <b>{label}</b>
        {detail && <small>{detail}</small>}
        <div className={`progress-track ${percent === null ? "indeterminate" : ""}`}>
          <i style={percent === null ? undefined : { width: `${percent}%` }} />
        </div>
      </div>
      {cancellable && (
        <button type="button" className="btn btn-danger sm" onClick={onCancel}>
          <SquareStop />
          取消
        </button>
      )}
    </div>
  );
}
function useAppWindow() {
  return useMemo(() => (isTauri() ? getCurrentWindow() : null), []);
}
function WindowControls() {
  const appWindow = useAppWindow();
  const [maximized, setMaximized] = useState(false);
  useEffect(() => {
    if (!appWindow) return;
    let dispose: (() => void) | undefined;
    const sync = () => {
      appWindow
        .isMaximized()
        .then(setMaximized)
        .catch(() => {});
    };
    sync();
    void appWindow
      .onResized(sync)
      .then((unlisten) => {
        dispose = unlisten;
      })
      .catch(() => {});
    return () => dispose?.();
  }, [appWindow]);
  if (!appWindow) return null;
  return (
    <div className="window-controls">
      <button
        type="button"
        className="window-button"
        aria-label="最小化"
        title="最小化"
        onClick={() => void appWindow.minimize()}
      >
        <Minus />
      </button>
      <button
        type="button"
        className="window-button"
        aria-label={maximized ? "向下还原" : "最大化"}
        title={maximized ? "向下还原" : "最大化"}
        onClick={() => void appWindow.toggleMaximize()}
      >
        {maximized ? <Copy /> : <Square />}
      </button>
      <button
        type="button"
        className="window-button close"
        aria-label="关闭"
        title="关闭"
        onClick={() => void appWindow.close()}
      >
        <X />
      </button>
    </div>
  );
}
function TitleBar({
  project,
  projects,
  model,
  view,
  onView,
  onProject,
  onTranslate,
  busy,
  disabled,
}: {
  project?: Project;
  projects: Project[];
  model: string;
  view: View;
  onView: (v: View) => void;
  onProject: (id: string) => void;
  onTranslate: () => void;
  busy: string | null;
  disabled: boolean;
}) {
  const [menuOpen, setMenuOpen] = useState(false);
  const retranslating = busy?.startsWith("重译") ?? false;
  const translateLabel = retranslating
    ? busy
    : busy === "翻译"
      ? "正在翻译"
      : busy === "项目初始化" || busy === "重新分析"
        ? busy === "重新分析"
          ? "正在重新分析"
          : "正在初始化"
        : busy === "导入书籍"
          ? "正在导入"
          : "开始翻译";
  return (
    <header className="titlebar" data-tauri-drag-region="deep">
      <span className="titlebar-logo">
        <Logo />
      </span>
      <div className="project-switcher">
        <button
          type="button"
          className="project-trigger"
          aria-expanded={menuOpen}
          disabled={!project || Boolean(busy)}
          onClick={() => setMenuOpen(!menuOpen)}
        >
          <b>{project?.title ?? "未选择项目"}</b>
          {menuOpen ? <ChevronUp /> : <ChevronDown />}
        </button>
        {menuOpen && (
          <>
            <button
              className="menu-backdrop"
              aria-label="关闭项目菜单"
              onClick={() => setMenuOpen(false)}
            />
            <div className="project-menu" data-tauri-drag-region="false">
              <header>
                <b>切换项目</b>
                <small>{projects.length} 个项目</small>
              </header>
              <div>
                {projects.map((item) => (
                  <button
                    type="button"
                    className={`project-option ${item.id === project?.id ? "active" : ""}`}
                    key={item.id}
                    onClick={() => {
                      setMenuOpen(false);
                      if (item.id !== project?.id) onProject(item.id);
                    }}
                  >
                    <ProjectCover project={item} className="mini-cover" />
                    <span>
                      <b>{item.title}</b>
                      <small>
                        {item.chapters_completed} / {item.chapters_total} 章 ·{" "}
                        {projectStatusText(item)}
                      </small>
                    </span>
                    {item.id === project?.id && <em>当前</em>}
                  </button>
                ))}
              </div>
            </div>
          </>
        )}
      </div>
      <span className="titlebar-suffix">— TransItPls</span>
      <nav className="titlebar-nav">
        {navItems.map((item) => (
          <button
            type="button"
            key={item.id}
            className={view === item.id ? "active" : ""}
            onClick={() => onView(item.id)}
          >
            {item.label}
          </button>
        ))}
      </nav>
      <div className="titlebar-drag" />
      <div className="titlebar-actions">
        <span className="model-chip" title={`当前模型：${model}`}>
          <i />
          {model}
        </span>
        <button
          type="button"
          className={`icon-button ${view === "settings" ? "active" : ""}`}
          aria-label="设置"
          title="设置"
          onClick={() => onView("settings")}
        >
          <Settings />
        </button>
        <button
          type="button"
          className="btn btn-primary sm"
          disabled={disabled || Boolean(busy)}
          onClick={onTranslate}
        >
          {busy ? <LoaderCircle className="spin" /> : <Play />}
          {translateLabel}
        </button>
      </div>
      <WindowControls />
    </header>
  );
}
function Explorer({
  projects,
  detail,
  chapterIndex,
  onChapter,
  onProject,
  onImport,
}: {
  projects: Project[];
  detail: Detail | null;
  chapterIndex: number;
  onChapter: (i: number) => void;
  onProject: (id: string) => void;
  onImport: () => void;
}) {
  const project = detail?.project;
  const progress = project
    ? Math.round(
        (project.chapters_completed / Math.max(1, project.chapters_total)) * 100,
      )
    : 0;
  return (
    <aside className="explorer">
      <div className="explorer-head">
        <b>项目文件</b>
        <button type="button" className="btn btn-quiet sm" onClick={onImport}>
          <Plus />
          导入
        </button>
      </div>
      {project ? (
        <>
          <div className="project-summary">
            <ProjectCover
              key={project.id}
              project={projects.find((item) => item.id === project.id) ?? project}
              className="cover-sm"
            />
            <div className="project-summary-info">
              <b>{project.title}</b>
              <small>{fileName(project.source_file)}</small>
            </div>
          </div>
          <div className="explorer-progress">
            <ProgressBar
              value={progress}
              label="全书进度"
              detail={`${project.chapters_completed} / ${project.chapters_total} 章 · ${projectStatusText(project)}`}
            />
          </div>
          <div className="chapter-list">
            <div className="chapter-list-head">
              <span>章节</span>
              <span>{detail?.chapters.length ?? 0} 章</span>
            </div>
            {detail?.chapters.map((chapter, index) => {
              const done = chapter.segments.filter(
                (s) => s.status === "translated",
              ).length;
              const percent = Math.round(
                (done / Math.max(1, chapter.segments.length)) * 100,
              );
              return (
                <button
                  key={chapter.id}
                  className={`chapter-item ${index === chapterIndex ? "active" : ""}`}
                  title={statusText[chapter.status]}
                  onClick={() => onChapter(index)}
                >
                  <i className={`status-dot ${statusClass(chapter.status)}`} />
                  <b>{chapter.target_title || chapter.title}</b>
                  <em>{percent}%</em>
                </button>
              );
            })}
          </div>
        </>
      ) : (
        <div className="panel-empty">尚无项目</div>
      )}
      {projects.length > 1 && (
        <div className="other-projects">
          <h4>其他项目</h4>
          {projects
            .filter((p) => p.id !== project?.id)
            .map((p) => (
              <button key={p.id} type="button" onClick={() => onProject(p.id)}>
                <FolderKanban />
                <span>{p.title}</span>
              </button>
            ))}
        </div>
      )}
    </aside>
  );
}
function Workspace({
  detail,
  chapter,
  chapterIndex,
  segments,
  hasMoreSegments,
  onShowMore,
  tray,
  setTray,
  search,
  onSearch,
  translating,
  busy,
  ready,
  inspectorOpen,
  onToggleInspector,
  onTranslate,
  onExport,
  onOpenTerms,
}: {
  detail: Detail;
  chapter: Chapter;
  chapterIndex: number;
  segments: Segment[];
  hasMoreSegments: boolean;
  onShowMore: () => void;
  tray: TrayName;
  setTray: (t: TrayName) => void;
  search: string;
  onSearch: (v: string) => void;
  translating: boolean;
  busy: boolean;
  ready: boolean;
  inspectorOpen: boolean;
  onToggleInspector: () => void;
  onTranslate: () => void;
  onExport: (f: "txt" | "epub") => void;
  onOpenTerms: () => void;
}) {
  const [drawerOpen, setDrawerOpen] = useState(false);
  const done = chapter.segments.filter((s) => s.status === "translated").length;
  const chapterPercent = Math.round(
    (done / Math.max(1, chapter.segments.length)) * 100,
  );
  return (
    <div className="editor-layout">
      <div className="work-toolbar">
        <div className="work-title">
          <b>{chapter.target_title || chapter.title}</b>
          <small>
            第 {chapterIndex + 1} / {detail.chapters.length} 章 ·{" "}
            {chapter.segments.length} 段 · 已译 {chapterPercent}%
          </small>
        </div>
        <label className="search-field">
          <Search />
          <input
            value={search}
            onChange={(e) => onSearch(e.target.value)}
            placeholder="搜索原文或译文…"
            aria-label="搜索原文或译文"
          />
          {search && (
            <button
              type="button"
              className="search-clear"
              aria-label="清除搜索"
              onClick={() => onSearch("")}
            >
              <X />
            </button>
          )}
        </label>
        <div className="toolbar-actions">
          <button
            type="button"
            className="btn btn-quiet sm"
            disabled={busy}
            title="导出 TXT"
            onClick={() => onExport("txt")}
          >
            <Download />
            <span className="btn-label">TXT</span>
          </button>
          <button
            type="button"
            className="btn btn-quiet sm"
            disabled={busy}
            title="导出 EPUB"
            onClick={() => onExport("epub")}
          >
            <Download />
            <span className="btn-label">EPUB</span>
          </button>
          <button
            type="button"
            className={`btn btn-quiet sm ${inspectorOpen ? "active" : ""}`}
            aria-pressed={inspectorOpen}
            onClick={onToggleInspector}
          >
            <SlidersHorizontal />
            <span className="btn-label">任务配置</span>
          </button>
          <button
            type="button"
            className="btn btn-primary sm"
            disabled={translating || busy}
            onClick={onTranslate}
          >
            {translating ? <LoaderCircle className="spin" /> : <Play />}
            {translating ? "正在翻译" : ready ? "翻译本章" : "初始化并翻译"}
          </button>
        </div>
      </div>
      <div className="column-head">
        <div>
          <b>原文</b>
          <span>{languageName(detail.project.source_language)}</span>
        </div>
        <div>
          <b>译文</b>
          <span>{languageName(detail.project.target_language)}</span>
        </div>
      </div>
      <section className="segments">
        {segments.length ? (
          segments.map((s) => <SegmentCard key={s.id} segment={s} />)
        ) : (
          <div className="no-results">没有匹配的段落</div>
        )}
        {hasMoreSegments && (
          <button
            type="button"
            className="load-more-segments"
            onClick={onShowMore}
          >
            <ChevronDown />
            显示更多段落
          </button>
        )}
      </section>
      <Tray
        detail={detail}
        tray={tray}
        setTray={setTray}
        open={drawerOpen}
        onToggle={() => setDrawerOpen((value) => !value)}
        onOpenTerms={onOpenTerms}
      />
    </div>
  );
}
function SegmentCard({ segment }: { segment: Segment }) {
  const words = segment.source.trim().split(/\s+/).filter(Boolean).length;
  return (
    <article className={`segment-card ${segment.status}`}>
      <div className="segment-source">
        <header>
          <span className="seg-index">#{segment.ordinal + 1}</span>
          <span className="seg-meta">
            {segment.kind === "heading" ? "标题" : "段落"} · {words} 词
          </span>
        </header>
        <p>{segment.source}</p>
      </div>
      <div className="segment-target">
        <header>
          <span className="seg-meta">{segment.target?.length ?? 0} 字</span>
          {segment.polish_status && (
            <em className={`status-chip ${polishClass(segment.polish_status)}`}>
              {polishStatusText[segment.polish_status]}
            </em>
          )}
          <em className={`status-chip ${statusClass(segment.status)}`}>
            {statusText[segment.status]}
          </em>
        </header>
        {segment.target ? (
          <p>{segment.target}</p>
        ) : (
          <div className="target-empty">
            <span>等待翻译</span>
            <small>运行本章翻译后将在此显示译文</small>
          </div>
        )}
      </div>
    </article>
  );
}
const MIN_DRAWER_HEIGHT = 120;
const DEFAULT_DRAWER_HEIGHT = 200;

function drawerMaxHeight(element: HTMLElement | null) {
  const available = element?.parentElement?.clientHeight ?? window.innerHeight;
  return Math.max(MIN_DRAWER_HEIGHT, Math.min(available * 0.8, available - 140));
}

function Tray({
  detail,
  tray,
  setTray,
  open,
  onToggle,
  onOpenTerms,
}: {
  detail: Detail;
  tray: TrayName;
  setTray: (t: TrayName) => void;
  open: boolean;
  onToggle: () => void;
  onOpenTerms: () => void;
}) {
  const drawerRef = useRef<HTMLElement | null>(null);
  const dragRef = useRef<{ startY: number; startHeight: number } | null>(null);
  const [height, setHeight] = useState(DEFAULT_DRAWER_HEIGHT);
  const [dragging, setDragging] = useState(false);
  const clampHeight = (value: number) =>
    Math.min(drawerMaxHeight(drawerRef.current), Math.max(MIN_DRAWER_HEIGHT, value));
  useEffect(() => {
    const onResize = () => setHeight((value) => clampHeight(value));
    onResize();
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);
  const startResize = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (!open) return;
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    dragRef.current = { startY: event.clientY, startHeight: height };
    setDragging(true);
    document.body.style.cursor = "row-resize";
    document.body.style.userSelect = "none";
  };
  const resize = (event: ReactPointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag) return;
    setHeight(clampHeight(drag.startHeight - (event.clientY - drag.startY)));
  };
  const endResize = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (!dragRef.current) return;
    dragRef.current = null;
    event.currentTarget.releasePointerCapture(event.pointerId);
    setDragging(false);
    document.body.style.cursor = "";
    document.body.style.userSelect = "";
  };
  return (
    <section
      ref={drawerRef}
      className={`drawer ${open ? "open" : ""}`}
      style={open ? { height } : undefined}
    >
      {open && (
        <div
          className={`drawer-resize ${dragging ? "dragging" : ""}`}
          role="separator"
          aria-label="拖动调整底部面板高度"
          aria-orientation="horizontal"
          onPointerDown={startResize}
          onPointerMove={resize}
          onPointerUp={endResize}
          onPointerCancel={endResize}
          onDoubleClick={() => setHeight(clampHeight(DEFAULT_DRAWER_HEIGHT))}
        />
      )}
      <header className="drawer-head">
        <div className="drawer-tabs">
          <button
            type="button"
            className={tray === "tasks" ? "active" : ""}
            onClick={() => {
              setTray("tasks");
              if (!open) onToggle();
            }}
          >
            章节任务 <b>{detail.chapters.length}</b>
          </button>
          <button
            type="button"
            className={tray === "issues" ? "active" : ""}
            onClick={() => {
              setTray("issues");
              if (!open) onToggle();
            }}
          >
            问题 <b>{detail.pendingConflicts}</b>
          </button>
          <button
            type="button"
            className={tray === "logs" ? "active" : ""}
            onClick={() => {
              setTray("logs");
              if (!open) onToggle();
            }}
          >
            运行日志
          </button>
        </div>
        <button type="button" className="drawer-toggle" onClick={onToggle}>
          {open ? <ChevronDown /> : <ChevronUp />}
          {open ? "收起" : "展开"}
        </button>
      </header>
      {open && (
        <div className="drawer-body">
          {tray === "tasks" && <TaskTable detail={detail} />}
          {tray === "issues" && (
            <IssueList detail={detail} onOpenTerms={onOpenTerms} />
          )}
          {tray === "logs" && <LogList logs={detail.logs} />}
        </div>
      )}
    </section>
  );
}
function TaskTable({ detail }: { detail: Detail }) {
  return (
    <table className="data-table">
      <thead>
        <tr>
          <th>#</th>
          <th>章节</th>
          <th>状态</th>
          <th>进度</th>
          <th>段落</th>
        </tr>
      </thead>
      <tbody>
        {detail.chapters.map((chapter, index) => {
          const done = chapter.segments.filter(
            (s) => s.status === "translated",
          ).length;
          const progress = Math.round(
            (done / Math.max(1, chapter.segments.length)) * 100,
          );
          return (
            <tr key={chapter.id}>
              <td className="num">{index + 1}</td>
              <td>{chapter.target_title || chapter.title}</td>
              <td>
                <em className={`status-chip ${statusClass(chapter.status)}`}>
                  {statusText[chapter.status]}
                </em>
              </td>
              <td>
                <div className="table-progress">
                  <div className="progress-track sm">
                    <i style={{ width: `${progress}%` }} />
                  </div>
                  <span>{progress}%</span>
                </div>
              </td>
              <td className="num">
                {done} / {chapter.segments.length}
              </td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}
function IssueList({
  detail,
  onOpenTerms,
}: {
  detail: Detail;
  onOpenTerms: () => void;
}) {
  const conflicts = detail.termConflicts.filter(
    (item) => item.unresolved_events > 0,
  );
  return conflicts.length ? (
    <div className="issue-list">
      {conflicts.map((item) => (
        <button type="button" key={item.source} onClick={onOpenTerms}>
          <b>术语冲突</b>
          <span>
            {item.source} · 当前固定译名：{item.current_target}
          </span>
          <em>{item.unresolved_events} 个待处理事件</em>
        </button>
      ))}
    </div>
  ) : (
    <div className="panel-empty">当前没有术语冲突</div>
  );
}
function LogList({ logs }: { logs: LogEntry[] }) {
  return logs.length ? (
    <div className="log-list">
      {logs.map((log, index) => (
        <div key={`${log.timestamp}-${index}`}>
          <time>{formatDate(log.timestamp)}</time>
          <b>{eventText(log.event)}</b>
          <code>{JSON.stringify(log.details)}</code>
        </div>
      ))}
    </div>
  ) : (
    <div className="panel-empty">暂无运行日志</div>
  );
}

function Inspector({
  config,
  detail,
  busy,
  retranslationProgress,
  translationTiming,
  now,
  draft,
  onDraftChange,
  onPolish,
  onPolishStart,
  onCancel,
  polishing,
  onInitialize,
  onReanalyze,
  onOpenTerms,
  onOpenSettings,
  onClose,
}: {
  config?: Config;
  detail: Detail;
  busy: string | null;
  retranslationProgress: RetranslationProgress | null;
  translationTiming: TranslationTiming | null;
  now: number;
  draft: TaskConfigDraft | null;
  onDraftChange: (value: TaskConfigDraft) => void;
  onPolish: (v: boolean) => Promise<void>;
  onPolishStart: (retryFailed: boolean) => void;
  onCancel: () => void;
  polishing: boolean;
  onInitialize: () => void;
  onReanalyze: () => Promise<void>;
  onOpenTerms: () => void;
  onOpenSettings: () => void;
  onClose: () => void;
}) {
  const [tab, setTab] = useState<"task" | "memory">("task");
  const progress = Math.round(
    (detail.project.chapters_completed /
      Math.max(1, detail.project.chapters_total)) *
      100,
  );
  const conflictCount = detail.pendingConflicts;
  const translationElapsed = translationTiming
    ? (translationTiming.finishedAt ?? now) - translationTiming.startedAt
    : 0;
  const translationRemaining = translationTiming
    ? translationTiming.totalRequests - translationTiming.completedRequests
    : 0;
  const estimatedRemaining =
    translationTiming?.completedRequests
      ? (translationElapsed / translationTiming.completedRequests) *
        translationRemaining
      : null;
  const polish = detail.polish;
  const translationComplete = detail.project.status === "translated";
  const activeRetranslation =
    retranslationProgress?.projectId === detail.project.id
      ? retranslationProgress
      : null;
  return (
    <aside className="inspector">
      <div className="inspector-head">
        <b>任务配置</b>
        <button
          type="button"
          className="icon-button"
          aria-label="关闭任务配置"
          title="关闭"
          onClick={onClose}
        >
          <X />
        </button>
      </div>
      <div className="inspector-tabs">
        <button
          type="button"
          className={tab === "task" ? "active" : ""}
          onClick={() => setTab("task")}
        >
          任务配置
        </button>
        <button
          type="button"
          className={tab === "memory" ? "active" : ""}
          onClick={() => setTab("memory")}
        >
          术语与记忆
        </button>
      </div>
      <div className="inspector-body">
        {!draft ? (
          <div className="panel-empty">
            <LoaderCircle className="spin" />
            正在读取任务配置…
          </div>
        ) : tab === "task" ? (
          <>
            <section className="inspector-section">
              <ProgressBar
                value={progress}
                label="项目进度"
                detail={`已完成 ${detail.project.chapters_completed} / ${detail.project.chapters_total} 章`}
                size="lg"
              />
              {translationTiming && (
                <p
                  className={`status-line ${translationTiming.finishedAt ? "done" : "active"}`}
                >
                  <Clock3 />
                  已用 {formatDuration(translationElapsed)} ·{" "}
                  {translationTiming.finishedAt
                    ? "本次翻译已结束"
                    : estimatedRemaining === null
                      ? "正在估算剩余时间"
                      : `预计还需 ${formatDuration(estimatedRemaining)}`}
                </p>
              )}
              {activeRetranslation && (
                <ProgressBar
                  value={
                    (activeRetranslation.completed /
                      Math.max(1, activeRetranslation.total)) *
                    100
                  }
                  label="本次重译"
                  detail={`${activeRetranslation.completed} / ${activeRetranslation.total} 项 · 成功 ${activeRetranslation.succeeded} · 失败 ${activeRetranslation.failed}`}
                />
              )}
              <p
                className={`status-line ${
                  detail.project.status === "failed"
                    ? "error"
                    : detail.project.status === "translated"
                      ? "done"
                      : "active"
                }`}
              >
                {detail.project.status === "translated" ? (
                  <CheckCircle2 />
                ) : (
                  <CircleDashed />
                )}
                {detail.taskInitialized
                  ? statusText[detail.project.status]
                  : "等待初始化任务"}
              </p>
            </section>
            <Field label="语言方向">
              <div className="direction">
                <select
                  value={draft.sourceLanguage}
                  disabled={detail.taskInitialized || Boolean(busy)}
                  onChange={(event) =>
                    onDraftChange({
                      ...draft,
                      sourceLanguage: event.target.value,
                    })
                  }
                >
                  <option value="auto">自动检测</option>
                  <option value="ja">日本語</option>
                  <option value="en">English</option>
                  <option value="ko">한국어</option>
                  <option value="zh-CN">中文（简体）</option>
                </select>
                <ArrowRight />
                <span>{languageName(detail.project.target_language)}</span>
              </div>
              <small>
                {detail.taskInitialized
                  ? `初始化结果：${languageName(detail.project.source_language)}`
                  : "初始化时确定源语言，自动检测会调用模型。"}
              </small>
            </Field>
            <Field label="模型">
              <div className="field-value">
                <span>{config?.llm.model ?? "—"}</span>
                <button
                  type="button"
                  className="text-action inline"
                  onClick={onOpenSettings}
                >
                  更换
                </button>
              </div>
              <small>提供商：{config?.llm.provider ?? "—"}</small>
            </Field>
            <section className="inspector-section">
              <b>初始化选项</b>
              <label className="switch-row">
                <span>全书译前分析</span>
                <button
                  type="button"
                  className={`toggle ${draft.fullBook ? "on" : ""}`}
                  disabled={Boolean(busy)}
                  aria-pressed={draft.fullBook}
                  onClick={() =>
                    onDraftChange({ ...draft, fullBook: !draft.fullBook })
                  }
                >
                  <i />
                </button>
              </label>
            </section>
            <section className="inspector-section">
              <b>分段策略</b>
              <div className="compact-fields">
                <NumberField
                  label="每段字符数"
                  value={draft.maxCharsPerSegment}
                  disabled={Boolean(busy)}
                  onChange={(value) =>
                    onDraftChange({ ...draft, maxCharsPerSegment: value })
                  }
                />
                <NumberField
                  label="每批字符数"
                  value={draft.maxCharsPerBatch}
                  disabled={Boolean(busy)}
                  onChange={(value) =>
                    onDraftChange({ ...draft, maxCharsPerBatch: value })
                  }
                />
              </div>
              <small className="field-hint">
                每段字符数只影响之后新导入的项目；每批字符数会用于后续翻译。
              </small>
            </section>
            <section className="inspector-section">
              <b>译后润色</b>
              <label className="switch-row">
                <span>全书翻译完成后自动润色</span>
                <button
                  type="button"
                  className={`toggle ${config?.pipeline.polish ? "on" : ""}`}
                  disabled={!config || Boolean(busy)}
                  aria-pressed={Boolean(config?.pipeline.polish)}
                  aria-label="全书翻译完成后自动润色"
                  onClick={() => void onPolish(!config?.pipeline.polish)}
                >
                  <i />
                </button>
              </label>
              {polish ? (
                <>
                  <p
                    className={`status-line ${
                      polish.failed > 0
                        ? "error"
                        : polish.pending > 0
                          ? "active"
                          : "done"
                    }`}
                  >
                    <CircleDashed />
                    润色共 {polish.total} 批 · 成功 {polish.succeeded} · 失败{" "}
                    {polish.failed} · 待处理 {polish.pending}
                    {polish.pendingSegments > 0 &&
                      ` · 待润色段落 ${polish.pendingSegments}`}
                  </p>
                  {polish.lastError && (
                    <p className="status-line error">
                      <CircleDashed />
                      {polish.lastError}
                    </p>
                  )}
                </>
              ) : (
                <p className="status-line active">
                  <CircleDashed />
                  {translationComplete
                    ? "全书初稿已完成，可开始润色"
                    : "全书翻译完成后可开始润色"}
                </p>
              )}
              <div className="polish-actions">
                {polishing ? (
                  <button
                    type="button"
                    className="btn btn-danger sm"
                    onClick={onCancel}
                  >
                    <SquareStop />
                    停止润色
                  </button>
                ) : (
                  <>
                    {polish && !polish.finished && (
                      <button
                        type="button"
                        className="btn btn-primary sm"
                        disabled={Boolean(busy)}
                        onClick={() => onPolishStart(false)}
                      >
                        <Play />
                        继续润色
                      </button>
                    )}
                    {(!polish ||
                      (polish.finished &&
                        polish.failed === 0 &&
                        polish.pendingSegments > 0)) && (
                      <button
                        type="button"
                        className="btn btn-primary sm"
                        disabled={Boolean(busy) || !translationComplete}
                        onClick={() => onPolishStart(false)}
                      >
                        <Play />
                        开始润色
                      </button>
                    )}
                    {polish && polish.failed > 0 && (
                      <button
                        type="button"
                        className="btn btn-quiet sm"
                        disabled={Boolean(busy)}
                        onClick={() => onPolishStart(true)}
                      >
                        <RotateCcw />
                        重试失败批次
                      </button>
                    )}
                    {polish &&
                      polish.finished &&
                      polish.failed === 0 &&
                      polish.pendingSegments === 0 && (
                        <span className="polish-done">
                          <CheckCircle2 />
                          全部段落已润色
                        </span>
                      )}
                  </>
                )}
              </div>
            </section>
            <details className="advanced-config">
              <summary>高级模型配置</summary>
              <div className="compact-fields">
                <NumberField
                  label="超时（秒）"
                  value={draft.timeoutSecs}
                  disabled={Boolean(busy)}
                  onChange={(value) =>
                    onDraftChange({ ...draft, timeoutSecs: value })
                  }
                />
                <NumberField
                  label="重试次数"
                  value={draft.maxRetries}
                  min={0}
                  disabled={Boolean(busy)}
                  onChange={(value) =>
                    onDraftChange({ ...draft, maxRetries: value })
                  }
                />
              </div>
            </details>
            {detail.taskInitialized ? (
              <button
                type="button"
                className="btn btn-quiet block"
                disabled={Boolean(busy) || !validTaskConfig(draft)}
                onClick={() => void onReanalyze()}
              >
                {busy === "重新分析" ? (
                  <LoaderCircle className="spin" />
                ) : (
                  <RotateCcw />
                )}
                {busy === "重新分析" ? "正在重新分析" : "重新分析"}
              </button>
            ) : (
              <button
                type="button"
                className="btn btn-primary block"
                disabled={Boolean(busy) || !validTaskConfig(draft)}
                onClick={onInitialize}
              >
                {busy === "项目初始化" ? (
                  <LoaderCircle className="spin" />
                ) : (
                  <Play />
                )}
                {busy === "项目初始化" ? "正在初始化任务" : "初始化任务"}
              </button>
            )}
          </>
        ) : (
          <>
            <section className="memory-summary">
              <div>
                <strong>{detail.terms.length}</strong>
                <span>术语总数</span>
              </div>
              <div className={conflictCount ? "has-conflicts" : ""}>
                <strong>{conflictCount}</strong>
                <span>待处理冲突</span>
              </div>
            </section>
            <Field label="近期译文上下文">
              <div className="number-with-unit">
                <input
                  type="number"
                  min="1"
                  step="100"
                  value={draft.recentContextChars}
                  disabled={Boolean(busy)}
                  onChange={(event) =>
                    onDraftChange({
                      ...draft,
                      recentContextChars: Number(event.target.value),
                    })
                  }
                />
                <span>字符</span>
              </div>
              <small>翻译下一批时携带的近期已译内容上限。</small>
            </Field>
            <button
              type="button"
              className="btn btn-quiet block"
              onClick={onOpenTerms}
            >
              打开术语库
              {conflictCount > 0 && ` · ${conflictCount} 个冲突`}
            </button>
          </>
        )}
      </div>
    </aside>
  );
}
function NumberField({
  label,
  value,
  min = 1,
  disabled,
  onChange,
}: {
  label: string;
  value: number;
  min?: number;
  disabled: boolean;
  onChange: (value: number) => void;
}) {
  return (
    <label>
      <span>{label}</span>
      <input
        type="number"
        min={min}
        step="1"
        value={value}
        disabled={disabled}
        onChange={(event) => onChange(Number(event.target.value))}
      />
    </label>
  );
}
function taskConfigFromConfig(config: Config): TaskConfigDraft {
  return {
    sourceLanguage: config.language.source,
    maxCharsPerSegment: config.segment.max_chars_per_segment,
    maxCharsPerBatch: config.segment.max_chars_per_batch,
    recentContextChars: config.pipeline.recent_context_chars,
    timeoutSecs: config.llm.timeout_secs,
    maxRetries: config.llm.max_retries,
    fullBook: config.analysis.full_book,
  };
}
function validTaskConfig(value: TaskConfigDraft) {
  return Boolean(value.sourceLanguage.trim()) &&
    Number.isInteger(value.maxCharsPerSegment) && value.maxCharsPerSegment > 0 &&
    Number.isInteger(value.maxCharsPerBatch) && value.maxCharsPerBatch > 0 &&
    Number.isInteger(value.recentContextChars) && value.recentContextChars > 0 &&
    Number.isInteger(value.timeoutSecs) && value.timeoutSecs > 0 &&
    Number.isInteger(value.maxRetries) && value.maxRetries >= 0;
}
function Field({
  label,
  hint,
  wide,
  children,
}: {
  label: string;
  hint?: string;
  wide?: boolean;
  children: ReactNode;
}) {
  return (
    <label className={`field ${wide ? "wide" : ""}`}>
      <b>{label}</b>
      {children}
      {hint && <small>{hint}</small>}
    </label>
  );
}
function SettingRow({
  label,
  hint,
  wide,
  children,
}: {
  label: string;
  hint?: string;
  wide?: boolean;
  children: ReactNode;
}) {
  return (
    <div className={`setting-row ${wide ? "wide" : ""}`}>
      <div className="setting-text">
        <b>{label}</b>
        {hint && <small>{hint}</small>}
      </div>
      <div className="setting-control">{children}</div>
    </div>
  );
}
function ProjectGallery({
  projects,
  busy,
  onSelect,
  onDelete,
  onImport,
}: {
  projects: Project[];
  busy: boolean;
  onSelect: (id: string) => void;
  onDelete?: (project: Project) => void;
  onImport: () => void;
}) {
  return (
    <div className="page-view">
      <header className="page-head">
        <div>
          <h1>翻译项目</h1>
          <p>管理本机状态目录中的所有书籍。</p>
        </div>
        <button type="button" className="btn btn-primary" onClick={onImport}>
          <Plus />
          新建项目
        </button>
      </header>
      {projects.length ? (
        <div className="project-grid">
          {projects.map((project) => {
            const progress = Math.round(
              (project.chapters_completed /
                Math.max(1, project.chapters_total)) *
                100,
            );
            return (
              <article className="project-tile" key={project.id}>
                <button
                  type="button"
                  className="project-open"
                  onClick={() => onSelect(project.id)}
                >
                  <ProjectCover project={project} className="cover" />
                  <div className="project-card-body">
                    <h3>{project.title}</h3>
                    <p>{fileName(project.source_file)}</p>
                    <ProgressBar value={progress} />
                    <footer>
                      <span>
                        {project.chapters_completed} / {project.chapters_total} 章
                      </span>
                      <em className={`status-chip ${statusClass(project.status)}`}>
                        {projectStatusText(project)}
                      </em>
                    </footer>
                  </div>
                </button>
                {onDelete && (
                  <button
                    type="button"
                    className="project-delete"
                    disabled={busy}
                    aria-label={`删除项目 ${project.title}`}
                    title="删除项目"
                    onClick={() => onDelete(project)}
                  >
                    <Trash2 />
                  </button>
                )}
              </article>
            );
          })}
        </div>
      ) : (
        <EmptyState onImport={onImport} />
      )}
    </div>
  );
}
function ProjectCover({
  project,
  className,
}: {
  project: Project;
  className: string;
}) {
  const [failed, setFailed] = useState(false);
  return (
    <span className={className}>
      {project.cover_data_url && !failed ? (
        <img
          src={project.cover_data_url}
          alt=""
          onError={() => setFailed(true)}
        />
      ) : (
        "文"
      )}
    </span>
  );
}
function TermsView({
  detail,
  taskBusy,
  retranslationProgress,
  onRetranslate,
  onReload,
}: {
  detail: Detail | null;
  taskBusy: boolean;
  retranslationProgress: RetranslationProgress | null;
  onRetranslate: (itemIds: string[]) => Promise<void>;
  onReload: () => Promise<void>;
}) {
  const [showResolved, setShowResolved] = useState(false);
  const [conflictIndex, setConflictIndex] = useState(0);
  const [editing, setEditing] = useState<string | null>(null);
  const [target, setTarget] = useState("");
  const [typeFilter, setTypeFilter] = useState("all");
  const [impact, setImpact] = useState<AffectedContent[]>([]);
  const [impactSource, setImpactSource] = useState<string | null>(null);
  const [allConflictImpacts, setAllConflictImpacts] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [working, setWorking] = useState<string | null>(null);
  const [scanning, setScanning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const activeRetranslation =
    retranslationProgress?.projectId === detail?.project.id
      ? retranslationProgress
      : null;
  const conflicts = (detail?.termConflicts ?? []).filter(
    (item) => showResolved || item.unresolved_events > 0,
  );
  const pendingConflictCount = (detail?.termConflicts ?? []).filter(
    (item) => item.unresolved_events > 0,
  ).length;
  const filteredTerms = (detail?.terms ?? []).filter(
    (term) => typeFilter === "all" || term.type === typeFilter,
  );
  const conflict =
    conflicts[Math.min(conflictIndex, Math.max(0, conflicts.length - 1))];
  useEffect(() => {
    setConflictIndex((index) =>
      Math.min(index, Math.max(0, conflicts.length - 1)),
    );
  }, [conflicts.length]);
  useEffect(() => {
    setTarget(conflict?.manual_target ?? conflict?.current_target ?? "");
  }, [conflict?.source, conflict?.manual_target, conflict?.current_target]);

  const scan = async (source: string) => {
    if (!detail) return;
    setScanning(true);
    try {
      const items = await invoke<AffectedContent[]>("ui_scan_term_impact", {
        projectId: detail.project.id,
        source,
      });
      setImpact(items);
      setImpactSource(source);
      setAllConflictImpacts(false);
      setSelected(new Set());
    } finally {
      setScanning(false);
    }
  };
  const scanAll = async (selectAll = true) => {
    if (!detail) return [];
    setScanning(true);
    try {
      const sources = detail.termConflicts
        .filter((item) => item.policy === "fixed")
        .map((item) => item.source);
      const groups = await Promise.all(
        sources.map((source) =>
          invoke<AffectedContent[]>("ui_scan_term_impact", {
            projectId: detail.project.id,
            source,
          }),
        ),
      );
      const items = [
        ...new Map(groups.flat().map((item) => [item.id, item])).values(),
      ];
      setImpact(items);
      setImpactSource(null);
      setAllConflictImpacts(true);
      setSelected(selectAll ? new Set(items.map((item) => item.id)) : new Set());
      return items;
    } finally {
      setScanning(false);
    }
  };
  useEffect(() => {
    if (!conflict) return;
    void scan(conflict.source).catch((value) => setError(String(value)));
  }, [conflict?.source, detail?.project.id]);
  const run = async (label: string, action: () => Promise<void>) => {
    setWorking(label);
    setError(null);
    try {
      await action();
    } catch (value) {
      setError(String(value));
    } finally {
      setWorking(null);
    }
  };
  const resolve = (source: string) =>
    run("保存裁定", async () => {
      if (!detail || !target.trim()) return;
      await invoke("ui_resolve_term", {
        projectId: detail.project.id,
        source,
        target: target.trim(),
      });
      setEditing(null);
      await scan(source);
      await onReload();
    });
  const setPolicy = (source: string, policy: TermPolicy) =>
    run("更新规则", async () => {
      if (!detail) return;
      await invoke("ui_set_term_policy", {
        projectId: detail.project.id,
        source,
        policy,
      });
      await scan(source);
      await onReload();
    });
  const undo = (source: string) =>
    run("撤销裁定", async () => {
      if (!detail) return;
      await invoke("ui_undo_term_resolution", {
        projectId: detail.project.id,
        source,
      });
      setImpact([]);
      await onReload();
    });
  const remove = (source: string) =>
    run("删除术语", async () => {
      if (!detail) return;
      const confirmed = await confirmDialog(
        `删除术语「${source}」？相关证据、冲突与人工规则会一并删除。`,
        { title: "确认删除术语", kind: "warning" },
      );
      if (!confirmed) return;
      await invoke("ui_delete_term", {
        projectId: detail.project.id,
        source,
      });
      setEditing((current) => (current === source ? null : current));
      await onReload();
    });
  const retranslate = () =>
    run("重译", async () => {
      if (!detail || !selected.size) return;
      const chapters = new Set(
        impact.filter((item) => selected.has(item.id)).map((item) => item.chapter),
      ).size;
      const confirmed = await confirmDialog(
        `将重译 ${selected.size} 项、涉及 ${chapters} 章；已有润色结果会失效，可在之后重新润色。此操作会消耗 API Token，是否继续？`,
        { title: "确认选择性重译", kind: "warning" },
      );
      if (!confirmed) return;
      await onRetranslate([...selected]);
      if (allConflictImpacts) await scanAll(false);
      else if (impactSource) await scan(impactSource);
      await onReload();
    });
  const retranslateResolved = () =>
    run("重译已处理冲突", async () => {
      const items = await scanAll(false);
      if (items.length) await onRetranslate(items.map((item) => item.id));
      await onReload();
    });
  const restore = (item: AffectedContent) =>
    run("恢复译文", async () => {
      if (!detail) return;
      await invoke("ui_restore_translation", {
        projectId: detail.project.id,
        itemId: item.id,
      });
      if (allConflictImpacts) await scanAll(false);
      else if (impactSource) await scan(impactSource);
      await onReload();
    });
  return (
    <div className="page-view terms-page">
      <header className="page-head">
        <div>
          <h1>术语库</h1>
          <p>
            {detail
              ? `${detail.project.title} · ${detail.terms.length} 条术语 · ${pendingConflictCount} 个待处理冲突`
              : "选择项目后查看术语"}
          </p>
        </div>
        <div className="page-head-actions">
          <select
            className="term-filter"
            aria-label="按类型筛选术语"
            value={typeFilter}
            onChange={(event) => setTypeFilter(event.target.value)}
          >
            <option value="all">全部</option>
            {Object.entries(termTypeText).map(([value, label]) => (
              <option key={value} value={value}>
                {label}
              </option>
            ))}
          </select>
          <label className="history-toggle">
            <input
              type="checkbox"
              checked={showResolved}
              onChange={(event) => setShowResolved(event.target.checked)}
            />
            查看已处理记录
          </label>
          {detail && (
            <button
              type="button"
              className="btn btn-primary sm"
              disabled={taskBusy || Boolean(working) || pendingConflictCount > 0}
              onClick={() => void retranslateResolved()}
            >
              {activeRetranslation || working === "重译已处理冲突" ? (
                <LoaderCircle className="spin" />
              ) : (
                <RotateCcw />
              )}
              {activeRetranslation
                ? `正在重译 ${activeRetranslation.completed}/${activeRetranslation.total}`
                : working === "重译已处理冲突"
                  ? "正在扫描影响范围…"
                  : pendingConflictCount > 0
                    ? `还有 ${pendingConflictCount} 个冲突待处理`
                    : "重译全部已处理冲突"}
            </button>
          )}
        </div>
      </header>
      <div className="page-body">
        {error && <div className="term-error">{error}</div>}
        {(working || scanning) && !error && (
          <div className="pending-banner">
            <LoaderCircle className="spin" />
            <span>{scanning ? "正在扫描影响范围…" : working}</span>
          </div>
        )}
        {detail && conflict && (
          <section className="conflict-panel">
            <header>
              <div>
                <small className="eyebrow">
                  译名冲突 · {conflict.unresolved_events} 个待处理事件
                </small>
                <h2>{conflict.source}</h2>
                <p>
                  当前固定译名：<b>{conflict.current_target}</b>
                </p>
              </div>
              <nav className="stepper">
                <button
                  type="button"
                  className="btn btn-quiet sm"
                  disabled={conflictIndex === 0}
                  onClick={() => setConflictIndex((value) => value - 1)}
                >
                  上一个
                </button>
                <span>
                  {conflictIndex + 1} / {conflicts.length}
                </span>
                <button
                  type="button"
                  className="btn btn-quiet sm"
                  disabled={conflictIndex >= conflicts.length - 1}
                  onClick={() => setConflictIndex((value) => value + 1)}
                >
                  下一个
                </button>
              </nav>
            </header>
            <div className="candidate-grid">
              {conflict.candidates.map((candidate) => (
                <button
                  type="button"
                  key={candidate.target}
                  onClick={() => setTarget(candidate.target)}
                  className={target === candidate.target ? "active" : ""}
                >
                  <b>{candidate.target}</b>
                  <span>
                    {candidate.occurrences} 次 ·{" "}
                    {candidate.chapters
                      .map((chapter) => `第 ${chapter + 1} 章`)
                      .join("、")}
                  </span>
                  {candidate.evidence.slice(0, 3).map((evidence, index) => (
                    <small key={index}>
                      {evidence.source_excerpt || "旧数据库无原文片段"}
                      <br />
                      {evidence.target_excerpt || "旧数据库无译文片段"}
                    </small>
                  ))}
                </button>
              ))}
            </div>
            <div className="conflict-actions">
              <input
                className="text-input"
                value={target}
                placeholder="选择候选或输入新的固定译名"
                onChange={(event) => setTarget(event.target.value)}
              />
              <button
                type="button"
                className="btn btn-primary sm"
                disabled={!target.trim() || Boolean(working)}
                onClick={() => void resolve(conflict.source)}
              >
                保存人工裁定
              </button>
              <button
                type="button"
                className="btn btn-quiet sm"
                disabled={Boolean(working)}
                onClick={() => void setPolicy(conflict.source, "non_fixed")}
              >
                标记为非固定术语
              </button>
              <button
                type="button"
                className="btn btn-quiet sm"
                disabled={Boolean(working)}
                onClick={() => void setPolicy(conflict.source, "ignored")}
              >
                忽略术语
              </button>
              {conflict.policy !== "automatic" && (
                <button
                  type="button"
                  className="btn btn-quiet sm"
                  disabled={Boolean(working)}
                  onClick={() => void undo(conflict.source)}
                >
                  撤销并恢复待处理
                </button>
              )}
            </div>
          </section>
        )}
        {detail && !conflict && detail.terms.length === 0 && (
          <div className="panel-empty">
            当前没有{showResolved ? "冲突记录" : "待处理冲突"}
          </div>
        )}
        {impact.length > 0 && (
          <section className="impact-panel">
            <header>
              <div>
                <h2>
                  {allConflictImpacts
                    ? "全部已裁定冲突可能影响的内容"
                    : "当前术语可能影响的内容"}
                </h2>
                <p>
                  按与术语提示一致的边界规则扫描，不代表精确调用追踪；重复命中的内容只显示一次。
                </p>
              </div>
              <button
                type="button"
                className="btn btn-quiet sm"
                onClick={() =>
                  setSelected(
                    selected.size === impact.length
                      ? new Set()
                      : new Set(impact.map((item) => item.id)),
                  )
                }
              >
                {selected.size === impact.length ? "取消全选" : "选择当前列表全部"}
              </button>
            </header>
            {impact.map((item) => (
              <div className="impact-row" key={item.id}>
                <input
                  type="checkbox"
                  checked={selected.has(item.id)}
                  aria-label={`选择第 ${item.chapter + 1} 章内容`}
                  onChange={() =>
                    setSelected((current) => {
                      const next = new Set(current);
                      next.has(item.id)
                        ? next.delete(item.id)
                        : next.add(item.id);
                      return next;
                    })
                  }
                />
                <span>
                  <b>
                    第 {item.chapter + 1} 章 · {item.kind}
                  </b>
                  <small>{item.source}</small>
                  <em>{item.currentTarget}</em>
                  {item.retranslationError && (
                    <strong>重译失败：{item.retranslationError}</strong>
                  )}
                </span>
                <button
                  type="button"
                  className="btn btn-quiet sm"
                  disabled={!item.previousTarget || Boolean(working)}
                  onClick={() => void restore(item)}
                >
                  恢复旧译文
                </button>
              </div>
            ))}
            {activeRetranslation && (
              <ProgressBar
                value={
                  (activeRetranslation.completed /
                    Math.max(1, activeRetranslation.total)) *
                  100
                }
                label="重译进度"
                detail={`${activeRetranslation.completed} / ${activeRetranslation.total} 项 · 成功 ${activeRetranslation.succeeded} · 失败 ${activeRetranslation.failed}`}
              />
            )}
            <footer>
              <span>
                {activeRetranslation
                  ? `正在重译，已处理 ${activeRetranslation.completed}/${activeRetranslation.total} 项`
                  : `已选择 ${selected.size} 项；默认不会自动重译。`}
              </span>
              <button
                type="button"
                className="btn btn-primary sm"
                disabled={!selected.size || taskBusy || Boolean(working)}
                onClick={() => void retranslate()}
              >
                {activeRetranslation ? (
                  <LoaderCircle className="spin" />
                ) : (
                  <Play />
                )}
                {activeRetranslation
                  ? `正在重译 ${activeRetranslation.completed}/${activeRetranslation.total}`
                  : taskBusy
                    ? "其他任务结束后可重译"
                    : "重译所选内容"}
              </button>
            </footer>
          </section>
        )}
        {detail && filteredTerms.length ? (
          <div className="term-table">
            <div className="term-row term-head">
              <span>原文</span>
              <span>固定译名</span>
              <span>类型</span>
              <span>首次出现</span>
              <span>状态</span>
              <span>操作</span>
            </div>
            {filteredTerms.map((term) => {
              const state = termState(term);
              return (
                <div className="term-row" key={term.source}>
                  <b>{term.source}</b>
                  {editing === term.source ? (
                    <input
                      autoFocus
                      value={target}
                      aria-label={`编辑 ${term.source} 的译名`}
                      onChange={(e) => setTarget(e.target.value)}
                    />
                  ) : (
                    <span>{term.target}</span>
                  )}
                  <span>{termTypeText[term.type] ?? "其他"}</span>
                  <span>第 {term.first_chapter + 1} 章</span>
                  <em className={`status-chip ${state.cls}`}>{state.text}</em>
                  {editing === term.source ? (
                    <button
                      type="button"
                      className="btn btn-primary sm"
                      onClick={() => void resolve(term.source)}
                    >
                      保存
                    </button>
                  ) : (
                    <span className="term-actions">
                      <button
                        type="button"
                        className="btn btn-quiet sm"
                        onClick={() => {
                          if (
                            term.policy === "ignored" ||
                            term.policy === "non_fixed"
                          ) {
                            void setPolicy(term.source, "automatic");
                          } else {
                            setEditing(term.source);
                            setTarget(term.target);
                          }
                        }}
                      >
                        {term.policy === "ignored" || term.policy === "non_fixed"
                          ? "恢复"
                          : "修改"}
                      </button>
                      <button
                        type="button"
                        className="btn btn-danger sm"
                        disabled={Boolean(working)}
                        onClick={() => void remove(term.source)}
                      >
                        删除
                      </button>
                    </span>
                  )}
                </div>
              );
            })}
          </div>
        ) : (
          <div className="panel-empty">
            {detail?.terms.length ? "该类型下暂无术语" : "暂无术语数据"}
          </div>
        )}
      </div>
    </div>
  );
}
function SettingsView({
  config,
  credential,
  configPath,
  busy,
  onSaveModel,
  onSaveGeneral,
}: {
  config?: Config;
  credential?: CredentialStatus;
  configPath?: string;
  busy: string | null;
  onSaveModel: (value: Config, key: string) => Promise<void>;
  onSaveGeneral: (
    visibleSegments: number,
    retranslationConcurrency: number,
    polishConcurrency: number,
  ) => Promise<void>;
}) {
  const [tab, setTab] = useState<"model" | "general">("model");
  const [draft, setDraft] = useState(config);
  const [visibleSegments, setVisibleSegments] = useState(
    config?.general.visible_segments ?? 100,
  );
  const [retranslationConcurrency, setRetranslationConcurrency] = useState(
    config?.general.retranslation_concurrency ?? 3,
  );
  const [polishConcurrency, setPolishConcurrency] = useState(
    config?.general.polish_concurrency ?? 3,
  );
  const [apiKey, setApiKey] = useState("");
  const [showKey, setShowKey] = useState(false);
  const [presetName, setPresetName] = useState("");
  const [editedFields, setEditedFields] = useState<Set<string>>(new Set());
  useEffect(() => {
    setDraft(config);
    setVisibleSegments(config?.general.visible_segments ?? 100);
    setRetranslationConcurrency(config?.general.retranslation_concurrency ?? 3);
    setPolishConcurrency(config?.general.polish_concurrency ?? 3);
    setApiKey("");
    setShowKey(false);
    const preset = providerPresets.find(
      (item) => item.base_url === config?.llm.base_url?.replace(/\/$/, ""),
    );
    setPresetName(preset?.name ?? "");
    setEditedFields(
      new Set(
        ["model"].filter((key) => {
          const value = config?.llm[key as keyof Config["llm"]];
          return (
            value !== undefined &&
            value !== preset?.[key as keyof typeof preset]
          );
        }),
      ),
    );
  }, [config]);
  if (!draft) {
    return (
      <div className="page-view">
        <div className="panel-empty">
          <LoaderCircle className="spin" />
          正在读取设置…
        </div>
      </div>
    );
  }
  const field = (key: keyof Config["llm"], value: string) => {
    setEditedFields(new Set([...editedFields, key]));
    setDraft({ ...draft, llm: { ...draft.llm, [key]: value } });
  };
  const applyPreset = (name: string, reset = false) => {
    setPresetName(name);
    const preset = providerPresets.find((item) => item.name === name);
    if (!preset) return;
    const llm = { ...draft.llm };
    for (const key of ["provider", "base_url", "model", "api_key_env"] as const) {
      if (key !== "model" || reset || !editedFields.has(key))
        llm[key] = preset[key];
    }
    if (reset) setEditedFields(new Set());
    setDraft({ ...draft, llm });
    setApiKey("");
    setShowKey(false);
  };
  const sameProvider =
    draft.llm.provider.trim().toLowerCase() ===
    config?.llm.provider.trim().toLowerCase();
  let noKey = false;
  try {
    const host = new URL(draft.llm.base_url ?? "").hostname;
    noKey =
      ["openai-chat", "openai-compatible"].includes(
        draft.llm.provider.trim().toLowerCase(),
      ) &&
      !draft.llm.api_key_env.trim() &&
      (host === "localhost" ||
        host === "[::1]" ||
        /^127(?:\.\d{1,3}){3}$/.test(host));
  } catch {
    /* The backend reports invalid URLs when validating. */
  }
  const configured = Boolean(
    sameProvider && credential?.configured && credential.source !== "none",
  );
  const savingModel = busy === "验证模型";
  const savingGeneral = busy === "保存通用设置";
  const generalInvalid =
    visibleSegments < 1 ||
    !Number.isInteger(visibleSegments) ||
    retranslationConcurrency < 1 ||
    !Number.isInteger(retranslationConcurrency) ||
    polishConcurrency < 1 ||
    !Number.isInteger(polishConcurrency);
  return (
    <div className="page-view settings-page">
      <header className="page-head">
        <div>
          <h1>设置</h1>
          <p>配置翻译模型、连接凭据与工作台行为。</p>
        </div>
      </header>
      <div className="settings-layout">
        <nav className="settings-nav">
          <button
            type="button"
            className={tab === "model" ? "active" : ""}
            onClick={() => setTab("model")}
          >
            <Braces />
            模型与 API
          </button>
          <button
            type="button"
            className={tab === "general" ? "active" : ""}
            onClick={() => setTab("general")}
          >
            <PanelTop />
            通用
          </button>
        </nav>
        {tab === "model" ? (
          <section className="settings-card">
            <div className="settings-heading">
              <div>
                <h2>模型与 API</h2>
                <p>选择服务商并验证用于翻译的 API Key。</p>
              </div>
              {configured || noKey ? (
                <span className="auth-chip">
                  <Check />
                  {noKey ? "本地免密" : "API Key 已配置"}
                </span>
              ) : (
                <span className="auth-chip muted">未配置凭据</span>
              )}
            </div>
            <div className="settings-rows">
              <SettingRow
                label="服务商"
                hint={
                  presetName
                    ? "已套用预设默认值。"
                    : "选择内置服务商，或使用自定义中转站。"
                }
              >
                <div className="preset-input">
                  <select
                    aria-label="服务商"
                    value={presetName}
                    onChange={(e) => applyPreset(e.target.value)}
                  >
                    <option value="">自定义(中转站)</option>
                    {providerPresets.map((preset) => (
                      <option key={preset.name} value={preset.name}>
                        {preset.name}
                      </option>
                    ))}
                  </select>
                  <button
                    type="button"
                    className="btn btn-quiet icon-only"
                    title="重置为预设默认值"
                    aria-label="重置为预设默认值"
                    disabled={!presetName}
                    onClick={() => applyPreset(presetName, true)}
                  >
                    <RotateCcw />
                  </button>
                </div>
              </SettingRow>
              <SettingRow label="协议类型" hint="需与服务商的接口兼容。">
                <select
                  aria-label="协议类型"
                  value={
                    draft.llm.provider.trim().toLowerCase() === "openai-chat"
                      ? "openai-compatible"
                      : draft.llm.provider.trim().toLowerCase()
                  }
                  onChange={(e) => {
                    field("provider", e.target.value);
                    setApiKey("");
                  }}
                >
                  <option value="openai-compatible">
                    OpenAI Chat Completions
                  </option>
                  <option value="openai-responses">OpenAI Responses</option>
                  <option value="anthropic">Anthropic (Messages)</option>
                </select>
              </SettingRow>
              <SettingRow label="模型" hint="用于翻译与分析的模型名称。">
                <input
                  value={draft.llm.model}
                  placeholder="例如 gpt-4o-mini"
                  onChange={(e) => field("model", e.target.value)}
                />
              </SettingRow>
              <SettingRow
                label="API Key"
                hint="Key 仅保存在当前用户的 TransItPls 配置目录中，不会写入书籍项目。"
              >
                <div className="secret-input">
                  <input
                    autoFocus={!credential?.configured}
                    type={showKey ? "text" : "password"}
                    disabled={noKey}
                    value={noKey ? "" : apiKey}
                    onChange={(e) => setApiKey(e.target.value)}
                    placeholder={
                      noKey
                        ? "本地免密"
                        : configured
                          ? "已配置，留空则使用现有凭据"
                          : "粘贴 API Key"
                    }
                  />
                  <button
                    type="button"
                    className="btn btn-quiet icon-only"
                    title={showKey ? "隐藏密钥" : "显示密钥"}
                    aria-label={showKey ? "隐藏密钥" : "显示密钥"}
                    onClick={() => setShowKey(!showKey)}
                  >
                    {showKey ? <EyeOff /> : <Eye />}
                  </button>
                </div>
              </SettingRow>
              <SettingRow
                label="API 地址（Base URL）"
                wide
                hint={
                  presetName
                    ? "此预设使用固定地址；如需修改，请选择“自定义(中转站)”。"
                    : "兼容 OpenAI 或 Anthropic 协议的服务地址。"
                }
              >
                <input
                  value={draft.llm.base_url ?? ""}
                  readOnly={Boolean(presetName)}
                  placeholder="https://api.example.com/v1"
                  onChange={(e) => field("base_url", e.target.value)}
                />
              </SettingRow>
            </div>
            <div className="settings-actions">
              <button
                type="button"
                className="btn btn-primary"
                disabled={busy !== null}
                onClick={() => void onSaveModel(draft, noKey ? "" : apiKey)}
              >
                {savingModel && <LoaderCircle className="spin" />}
                {savingModel ? "正在验证连接…" : "测试连接并保存"}
              </button>
            </div>
            <footer className="settings-foot">
              <span>配置文件</span>
              <code>{configPath ?? "保存后创建 transitpls.toml"}</code>
            </footer>
          </section>
        ) : (
          <section className="settings-card">
            <div className="settings-heading">
              <div>
                <h2>通用</h2>
                <p>调整工作台显示和批量重译行为。</p>
              </div>
            </div>
            <div className="settings-rows">
              <SettingRow
                label="每章默认显示段落数"
                hint="章节切换时先显示这些段落，点击“显示更多段落”可继续查看其余内容。"
              >
                <input
                  type="number"
                  min="1"
                  step="1"
                  value={visibleSegments}
                  onChange={(e) => setVisibleSegments(Number(e.target.value))}
                />
              </SettingRow>
              <SettingRow
                label="重译并发数量"
                hint="“重译全部已处理冲突”和选择性重译同时处理的最大项目数。"
              >
                <input
                  type="number"
                  min="1"
                  step="1"
                  value={retranslationConcurrency}
                  onChange={(e) =>
                    setRetranslationConcurrency(Number(e.target.value))
                  }
                />
              </SettingRow>
              <SettingRow
                label="润色并发数量"
                hint="并行润色批次的最大数量，默认 3；独立于重译并发。"
              >
                <input
                  type="number"
                  min="1"
                  step="1"
                  value={polishConcurrency}
                  onChange={(e) => setPolishConcurrency(Number(e.target.value))}
                />
              </SettingRow>
            </div>
            <div className="settings-actions">
              <button
                type="button"
                className="btn btn-primary"
                disabled={busy !== null || generalInvalid}
                onClick={() =>
                  void onSaveGeneral(
                    visibleSegments,
                    retranslationConcurrency,
                    polishConcurrency,
                  )
                }
              >
                {savingGeneral && <LoaderCircle className="spin" />}
                {savingGeneral ? "正在保存…" : "保存通用设置"}
              </button>
            </div>
            <footer className="settings-foot">
              <span>配置文件</span>
              <code>{configPath ?? "保存后创建 transitpls.toml"}</code>
            </footer>
          </section>
        )}
      </div>
    </div>
  );
}
function EmptyState({ onImport }: { onImport: () => void }) {
  return (
    <div className="empty-state">
      <Logo />
      <h1>开始第一个翻译项目</h1>
      <p>
        导入 EPUB 或 TXT 文件。
      </p>
      <button type="button" className="btn btn-primary" onClick={onImport}>
        <Plus />
        选择书籍文件
      </button>
      <small>暂不支持 PDF、DOCX 和字幕文件</small>
    </div>
  );
}
function statusClass(status: Status | ItemStatus) {
  return status === "translated"
    ? "ok"
    : status === "failed"
      ? "danger"
      : status === "translating"
        ? "active"
        : "muted";
}
function polishClass(status: PolishStatus) {
  return status === "succeeded" ? "ok" : status === "failed" ? "danger" : "warn";
}
function termState(term: Term) {
  if (term.policy === "ignored") return { cls: "muted", text: "已忽略" };
  if (term.policy === "non_fixed") return { cls: "muted", text: "非固定" };
  if (term.status === "conflict") return { cls: "warn", text: "有冲突" };
  if (term.status === "resolved") return { cls: "ok", text: "已裁定" };
  return { cls: "muted", text: "正常" };
}
function fileName(path: string) {
  return path.split(/[\\/]/).pop() ?? path;
}
function languageName(code: string) {
  return (
    (
      {
        auto: "自动检测",
        en: "English",
        "zh-CN": "中文（简体）",
        ja: "日本語",
      } as Record<string, string>
    )[code] ?? code
  );
}
function projectStatusText(project: Project) {
  return project.task_initialized === false
    ? "待初始化"
    : statusText[project.status];
}
function formatDate(value: string) {
  try {
    return new Intl.DateTimeFormat("zh-CN", {
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    }).format(new Date(value));
  } catch {
    return value;
  }
}
function countTranslationRequests(
  chapters: Chapter[],
  maxChars: number,
  chapterId?: string,
) {
  const selected = chapterId
    ? chapters.filter((chapter) => chapter.id === chapterId)
    : chapters;
  let total = selected.reduce(
    (sum, chapter) =>
      sum +
      countPendingBatches(
        chapter.segments.map((segment) => ({
          source: segment.source,
          pending: segment.status !== "translated" || segment.target === null,
        })),
        maxChars,
      ),
    0,
  );
  const completesBook = chapters.every(
    (chapter) =>
      chapter.id === chapterId ||
      chapter.segments.every(
        (segment) => segment.status === "translated" && segment.target !== null,
      ),
  );
  if (!chapterId || completesBook) {
    total += countPendingBatches(
      chapters.map((chapter) => ({
        source: chapter.title,
        pending: !chapter.target_title,
      })),
      maxChars,
    );
  }
  return total;
}
function countPendingBatches(
  items: Array<{ source: string; pending: boolean }>,
  maxChars: number,
) {
  let batches = 0;
  let chars = 0;
  let pending = false;
  for (const item of items) {
    const next = [...item.source].length;
    if (chars > 0 && chars + next > maxChars) {
      if (pending) batches += 1;
      chars = 0;
      pending = false;
    }
    chars += next;
    pending ||= item.pending;
    if (chars >= maxChars) {
      if (pending) batches += 1;
      chars = 0;
      pending = false;
    }
  }
  return batches + Number(pending);
}
function formatDuration(milliseconds: number) {
  const seconds = Math.max(0, Math.round(milliseconds / 1_000));
  if (seconds < 60) return `${seconds} 秒`;
  const minutes = Math.floor(seconds / 60);
  const remainder = seconds % 60;
  return remainder ? `${minutes} 分 ${remainder} 秒` : `${minutes} 分钟`;
}
function eventText(event: string) {
  return (
    (
      {
        initialized: "项目已创建",
        analysis_completed: "译前分析完成",
        transit_started: "开始翻译",
        transit_completed: "翻译完成",
        term_resolved: "术语已裁定",
        term_policy_changed: "术语规则已更新",
        term_resolution_undone: "术语裁定已撤销",
        retranslation_started: "开始选择性重译",
        retranslation_completed: "选择性重译完成",
        retranslated: "内容已重译",
        retranslation_failed: "内容重译失败",
        polish_started: "开始润色",
        polish_batch_completed: "润色批次完成",
        polish_batch_failed: "润色批次失败",
        polish_completed: "润色完成",
        polish_failed: "润色失败",
        polish_cancelled: "润色已取消",
        polish_invalidated: "润色快照已失效",
        polish_retry_requested: "重试润色失败批次",
        translation_restored: "旧译文已恢复",
        exported: "成品已导出",
        failed: "任务失败",
      } as Record<string, string>
    )[event] ?? event
  );
}
function browserPreview(): Bootstrap {
  return {
    stateDir: "projects",
    projects: [],
    credential: { configured: false },
    config: {
      language: { source: "auto", target: "zh-CN" },
      llm: {
        provider: "openai-chat",
        model: "gpt-4o-mini",
        api_key_env: "OPENAI_API_KEY",
        base_url: "https://api.openai.com/v1",
        timeout_secs: 60,
        max_retries: 3,
      },
      segment: { max_chars_per_segment: 1200, max_chars_per_batch: 1800 },
      pipeline: { polish: false, recent_context_chars: 2000 },
      analysis: { full_book: true },
      general: {
        visible_segments: 100,
        retranslation_concurrency: 3,
        polish_concurrency: 3,
      },
    },
  };
}
