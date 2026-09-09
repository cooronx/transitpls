import { useEffect, useMemo, useState, type ReactNode } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { confirm as confirmDialog, open } from "@tauri-apps/plugin-dialog";
import { openPath } from "@tauri-apps/plugin-opener";
import {
  ArrowRight,
  BadgeCheck,
  BookOpen,
  Braces,
  Check,
  CheckCircle2,
  ChevronDown,
  ChevronUp,
  Circle,
  CircleDashed,
  Columns2,
  Download,
  Eye,
  EyeOff,
  FileText,
  FolderKanban,
  Languages,
  LibraryBig,
  ListTree,
  LoaderCircle,
  MessageSquare,
  PanelTop,
  Play,
  Plus,
  RotateCcw,
  Search,
  Settings,
  ShieldCheck,
  SquareStop,
  Trash2,
  X,
  type LucideIcon,
} from "lucide-react";
import "./App.css";
import providerPresets from "./provider-presets.json";

type Status = "initialized" | "translating" | "translated" | "failed";
type ItemStatus = "pending" | "translated" | "failed";
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
  general: { visible_segments: number };
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
}
type View =
  | "workspace"
  | "projects"
  | "terms"
  | "settings"
  | "review"
  | "history";
type TrayName = "tasks" | "issues" | "logs";

const navItems: Array<{ id: View; icon: LucideIcon; label: string }> = [
  { id: "workspace", icon: Languages, label: "工作台" },
  { id: "projects", icon: FolderKanban, label: "项目" },
  { id: "terms", icon: LibraryBig, label: "术语库" },
  // {id:"review",icon:BadgeCheck,label:"审校"},{id:"history",icon:History,label:"历史"},
];
const statusText: Record<Status | ItemStatus, string> = {
  initialized: "待翻译",
  translating: "翻译中",
  translated: "已完成",
  failed: "失败",
  pending: "待处理",
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

export default function App() {
  const [bootstrap, setBootstrap] = useState<Bootstrap | null>(null);
  const [detail, setDetail] = useState<Detail | null>(null);
  const [chapterIndex, setChapterIndex] = useState(0);
  const [view, setView] = useState<View>("workspace");
  const [tray, setTray] = useState<TrayName>("tasks");
  const [busy, setBusy] = useState<string | null>(null);
  const [retranslationProgress, setRetranslationProgress] =
    useState<RetranslationProgress | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const [mockClient] = useState(false);
  const [visibleSegmentCount, setVisibleSegmentCount] = useState(100);
  const [displaySegmentCount, setDisplaySegmentCount] = useState(100);

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
  useEffect(() => {
    reload().catch((error) => {
      if (String(error).includes("invoke")) setBootstrap(browserPreview());
      else setNotice(String(error));
    });
  }, []);
  useEffect(() => {
    if (!isTauri()) return;
    let disposed = false;
    let stop: undefined | (() => void);
    listen<Detail>("translation-progress", ({ payload }) => {
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
    const next = bootstrap?.config.general.visible_segments ?? 100;
    setVisibleSegmentCount(next);
    setDisplaySegmentCount(next);
  }, [bootstrap?.config.general.visible_segments]);

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
  const initializeTask = (value: TaskConfigDraft) => {
    if (busy || !detail || detail.taskInitialized) return;
    if (!bootstrap?.credential.configured && !mockClient) {
      setView("settings");
      setNotice("请先在设置中配置并验证 API Key");
      return;
    }
    void run("项目初始化", async () => {
      const next = await invoke<Bootstrap>("ui_save_task_config", taskConfigArgs(value));
      setBootstrap(next);
      return invoke<Detail>("ui_initialize", {
          projectId: detail.project.id,
          mockClient,
        });
    });
  };
  const saveTaskConfig = async (value: TaskConfigDraft) => {
    if (busy) return;
    setBusy("保存任务配置");
    setNotice(null);
    try {
      const next = await invoke<Bootstrap>("ui_save_task_config", taskConfigArgs(value));
      setBootstrap(next);
      setNotice("任务配置已保存");
    } catch (error) {
      setNotice(String(error));
    } finally {
      setBusy(null);
    }
  };
  const reanalyze = async (value: TaskConfigDraft) => {
    if (busy || !detail || !detail.taskInitialized) return;
    const confirmed = await confirmDialog(
      "重新分析会再次调用模型，并刷新风格分析、章节摘要与全书梗概。继续吗？",
      { title: "重新分析", kind: "warning", okLabel: "重新分析", cancelLabel: "取消" },
    );
    if (!confirmed) return;
    void run("重新分析", async () => {
      const next = await invoke<Bootstrap>("ui_save_task_config", taskConfigArgs(value));
      setBootstrap(next);
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
  const translate = (chapter?: number) => {
    if (busy) return;
    if (!bootstrap?.credential.configured && !mockClient) {
      setView("settings");
      setNotice("请先在设置中配置并验证 API Key");
      return;
    }
    if (detail)
      void run("翻译", () =>
        invoke<Detail>("ui_transit", {
          projectId: detail.project.id,
          chapter: chapter ?? null,
          mockClient,
        }),
      );
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
  const exportBook = async (format: "txt" | "epub") => {
    if (!detail) return [];
    setBusy("导出");
    try {
      const output = await invoke<string>("ui_export", {
        projectId: detail.project.id,
        format,
      });
      setNotice(`已导出至 ${output}`);
      await openPath(output);
    } catch (error) {
      setNotice(String(error));
    } finally {
      setBusy(null);
    }
  };
  const saveGeneral = async (visibleSegments: number) => {
    setBusy("保存通用设置");
    setNotice(null);
    try {
      const next = await invoke<Bootstrap>("ui_save_general", {
        visibleSegments,
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

  return (
    <div className="app-shell">
      <Header
        project={detail?.project}
        projects={bootstrap?.projects ?? []}
        model={bootstrap?.config.llm.model ?? "—"}
        search={search}
        onSearch={setSearch}
        onProject={(id) => void selectProject(id)}
        onTranslate={() => translate()}
        busy={busy}
        onCancel={() => void cancelTask()}
        disabled={!detail?.taskInitialized}
      />
      <div className="workspace-row">
        <ActivityBar view={view} onChange={setView} />
        {view === "workspace" && (
          <Explorer
            projects={bootstrap?.projects ?? []}
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
              translating={busy === "翻译"}
              ready={detail.taskInitialized}
              onTranslate={() => translate(chapterIndex)}
              onExport={exportBook}
              onOpenTerms={() => setView("terms")}
            />
          )}
          {view === "projects" && (
            <ProjectGallery
              projects={bootstrap?.projects ?? []}
              busy={Boolean(busy)}
              onSelect={selectProject}
              onDelete={deleteProject}
              onImport={importFile}
            />
          )}
          {view === "history" && (
            <ProjectGallery
              projects={bootstrap?.projects ?? []}
              busy={Boolean(busy)}
              onSelect={selectProject}
              onImport={importFile}
            />
          )}
          {view === "terms" && (
            <TermsView
              detail={detail}
              config={bootstrap?.config}
              taskBusy={Boolean(busy)}
              retranslationProgress={retranslationProgress}
              onRetranslate={retranslateItems}
              onReload={() => reload()}
            />
          )}
          {view === "settings" && (
            <SettingsView
              config={bootstrap?.config}
              credential={bootstrap?.credential}
              configPath={bootstrap?.configPath}
              busy={busy}
              onSaveModel={saveModel}
              onSaveGeneral={saveGeneral}
            />
          )}
          {view === "review" && <ReviewPlaceholder />}
          {view === "workspace" && (!detail || !chapter) && (
            <EmptyState onImport={importFile} />
          )}
        </main>
        {view === "workspace" && detail && (
          <Inspector
            config={bootstrap?.config}
            detail={detail}
            busy={busy}
            retranslationProgress={retranslationProgress}
            onPolish={savePipeline}
            onInitialize={initializeTask}
            onSaveConfig={saveTaskConfig}
            onReanalyze={reanalyze}
            onOpenTerms={() => setView("terms")}
          />
        )}
      </div>
      <footer className="statusbar">
        <span>TransItPls v0.1.0</span>
        <i />
        <span>Tauri · 跨平台</span>
        <span className="status-spacer" />
        <span className="icon-label">
          {busy && <LoaderCircle className="spin" />}
          {busy ? `${busy} 进行中…` : "就绪"}
        </span>
        <i />
        <span>{bootstrap?.projects.length ?? 0} 个项目</span>
      </footer>
      {notice && (
        <div className="toast">
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
      {busy && <div className="busy-line" />}
    </div>
  );
}

function Logo() {
  return (
    <div className="logo-mark">
      <span>文</span>
      <b>A</b>
    </div>
  );
}
function Header({
  project,
  projects,
  model,
  search,
  onSearch,
  onProject,
  onTranslate,
  busy,
  onCancel,
  disabled,
}: {
  project?: Project;
  projects: Project[];
  model: string;
  search: string;
  onSearch: (v: string) => void;
  onProject: (id: string) => void;
  onTranslate: () => void;
  busy: string | null;
  onCancel: () => void;
  disabled: boolean;
}) {
  const [menuOpen, setMenuOpen] = useState(false);
  const retranslating = busy?.startsWith("重译") ?? false;
  const cancellable =
    busy === "翻译" ||
    retranslating ||
    busy === "项目初始化" ||
    busy === "重新分析" ||
    busy === "导入书籍";
  const translateLabel =
    retranslating
      ? busy
      : busy === "翻译"
      ? "正在翻译"
      : busy === "项目初始化" || busy === "重新分析"
        ? busy === "重新分析" ? "正在重新分析" : "正在初始化"
        : busy === "导入书籍"
          ? "正在导入"
        : "开始翻译";
  return (
    <header className="topbar">
      <div className="brand">
        <Logo />
        <strong>TransItPls</strong>
        <i />
        <span>项目：</span>
        <div className="project-switcher">
          <button
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
              <div className="project-menu">
                <header>
                  <b>切换项目</b>
                  <small>{projects.length} 个项目</small>
                </header>
                <div>
                  {projects.map((item) => (
                    <button
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
      </div>
      <label className="global-search">
        <Search />
        <input
          value={search}
          onChange={(e) => onSearch(e.target.value)}
          placeholder="搜索原文或译文…"
        />
        <kbd>Ctrl K</kbd>
      </label>
      <div className="top-actions">
        <span>
          模型: <b>{model}</b>
        </span>
        <span className="local-state">
          <i />
          本地状态
        </span>
        <button
          className="primary icon-label"
          disabled={disabled || Boolean(busy)}
          onClick={onTranslate}
        >
          {busy ? <LoaderCircle className="spin" /> : <Play />}
          {translateLabel}
        </button>
        {cancellable && (
          <button className="cancel icon-label" onClick={onCancel}>
            <SquareStop />
            取消任务
          </button>
        )}
      </div>
    </header>
  );
}
function ActivityBar({
  view,
  onChange,
}: {
  view: View;
  onChange: (v: View) => void;
}) {
  return (
    <nav className="activity-bar">
      <div>
        {navItems.map((item) => {
          const Icon = item.icon;
          return (
            <button
              key={item.id}
              className={view === item.id ? "active" : ""}
              onClick={() => onChange(item.id)}
              title={item.label}
            >
              <Icon />
              <span>{item.label}</span>
            </button>
          );
        })}
      </div>
      <button
        className={view === "settings" ? "active" : ""}
        onClick={() => onChange("settings")}
        title="设置"
      >
        <Settings />
        <span>设置</span>
      </button>
    </nav>
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
  const project = detail?.project,
    progress = project
      ? Math.round(
          (project.chapters_completed / Math.max(1, project.chapters_total)) *
            100,
        )
      : 0;
  return (
    <aside className="explorer">
      <div className="panel-title">
        <strong>项目文件</strong>
        <button className="icon-label" onClick={onImport}>
          <Plus />
          导入文件
        </button>
      </div>
      {project ? (
        <>
          <div className="project-card">
            <div>
              <span className="book-icon">
                <BookOpen />
              </span>
              <p>
                <b>{project.title}</b>
                <small>{project.chapters_total} 个章节</small>
              </p>
            </div>
            <div className="progress">
              <i style={{ width: `${progress}%` }} />
            </div>
            <footer>
              <span>
                {project.chapters_completed} / {project.chapters_total} 章
              </span>
              <b>{progress}%</b>
            </footer>
          </div>
          <div className="file-row">
            <ChevronDown />
            <BookOpen />
            <strong>{fileName(project.source_file)}</strong>
          </div>
          <div className="chapter-list">
            {detail?.chapters.map((chapter, index) => (
              <button
                key={chapter.id}
                className={index === chapterIndex ? "active" : ""}
                onClick={() => onChapter(index)}
              >
                <FileText />
                <b>{chapter.target_title || chapter.title}</b>
                <em className={chapter.status}>{statusText[chapter.status]}</em>
              </button>
            ))}
          </div>
        </>
      ) : (
        <div className="explorer-empty">尚无项目</div>
      )}
      {projects.length > 1 && (
        <div className="other-projects">
          <h4>其他项目</h4>
          {projects
            .filter((p) => p.id !== project?.id)
            .map((p) => (
              <button key={p.id} onClick={() => onProject(p.id)}>
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
  translating,
  ready,
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
  translating: boolean;
  ready: boolean;
  onTranslate: () => void;
  onExport: (f: "txt" | "epub") => void;
  onOpenTerms: () => void;
}) {
  return (
    <div className="editor-layout">
      <div className="editor-tabs">
        <button className="active icon-label">
          <Columns2 />
          对照翻译
        </button>
        <button className="icon-label">
          <ListTree />
          结构预览
        </button>
        <button className="icon-label" onClick={() => setTray("issues")}>
          <ShieldCheck />
          质量检查
        </button>
        <span />
        <small>
          第 {chapterIndex + 1} / {detail.chapters.length} 章
        </small>
        <button
          className="run-chapter icon-label"
          disabled={translating || !ready}
          onClick={onTranslate}
        >
          {translating ? <LoaderCircle className="spin" /> : <Play />}
          {translating ? "正在翻译" : ready ? "翻译本章" : "请先初始化"}
        </button>
      </div>
      <div className="breadcrumb">
        <FileText />
        {fileName(detail.project.source_file)}
        <i>/</i>
        <b>{chapter.target_title || chapter.title}</b>
        <em>{chapter.segments.length} 个段落</em>
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
            className="load-more-segments icon-label"
            onClick={onShowMore}
          >
            <ChevronDown />
            显示更多段落
          </button>
        )}
      </section>
      <Tray detail={detail} tray={tray} setTray={setTray} onExport={onExport} onOpenTerms={onOpenTerms} />
    </div>
  );
}
function SegmentCard({ segment }: { segment: Segment }) {
  const words = segment.source.trim().split(/\s+/).filter(Boolean).length;
  return (
    <article className={`segment-card ${segment.status}`}>
      <div className="segment-source">
        <header>
          <b>#{segment.ordinal + 1}</b>
          <span>{words} 词</span>
        </header>
        <p>{segment.source}</p>
        <footer>
          <PanelTop />
          {segment.kind === "heading" ? "标题" : "源段落"}
        </footer>
      </div>
      <div className="segment-target">
        <header>
          <span>{segment.target?.length ?? 0} 字</span>
          <b className={segment.status}>{statusText[segment.status]}</b>
        </header>
        {segment.target ? (
          <p>{segment.target}</p>
        ) : (
          <div className="target-empty">
            <span>等待翻译</span>
            <small>运行本章翻译后将在此显示译文</small>
          </div>
        )}
        <footer>
          <button disabled className="icon-label">
            <RotateCcw />
            重译
          </button>
          <button disabled className="icon-label">
            <Check />
            采纳
          </button>
          <button disabled className="icon-label">
            <MessageSquare />
            注释
          </button>
        </footer>
      </div>
    </article>
  );
}
function Tray({
  detail,
  tray,
  setTray,
  onExport,
  onOpenTerms,
}: {
  detail: Detail;
  tray: TrayName;
  setTray: (t: TrayName) => void;
  onExport: (f: "txt" | "epub") => void;
  onOpenTerms: () => void;
}) {
  return (
    <section className="tray">
      <header>
        <div>
          <button
            className={tray === "tasks" ? "active" : ""}
            onClick={() => setTray("tasks")}
          >
            章节任务 <b>{detail.chapters.length}</b>
          </button>
          <button
            className={tray === "issues" ? "active" : ""}
            onClick={() => setTray("issues")}
          >
            问题列表 <b>{detail.pendingConflicts}</b>
          </button>
          <button
            className={tray === "logs" ? "active" : ""}
            onClick={() => setTray("logs")}
          >
            运行日志
          </button>
        </div>
        <div className="export-menu">
          <button className="icon-label" onClick={() => onExport("txt")}>
            <Download />
            TXT
          </button>
          <button className="icon-label" onClick={() => onExport("epub")}>
            <Download />
            EPUB
          </button>
        </div>
      </header>
      <div className="tray-content">
        {tray === "tasks" && <TaskTable detail={detail} />}{" "}
        {tray === "issues" && <IssueList detail={detail} onOpenTerms={onOpenTerms} />}{" "}
        {tray === "logs" && <LogList logs={detail.logs} />}
      </div>
    </section>
  );
}
function TaskTable({ detail }: { detail: Detail }) {
  return (
    <table>
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
            ).length,
            progress = Math.round(
              (done / Math.max(1, chapter.segments.length)) * 100,
            );
          return (
            <tr key={chapter.id}>
              <td>{index + 1}</td>
              <td>{chapter.target_title || chapter.title}</td>
              <td>
                <em className={chapter.status}>{statusText[chapter.status]}</em>
              </td>
              <td>
                <div className="table-progress">
                  <i style={{ width: `${progress}%` }} />
                  <span>{progress}%</span>
                </div>
              </td>
              <td>
                {done} / {chapter.segments.length}
              </td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}
function IssueList({ detail, onOpenTerms }: { detail: Detail; onOpenTerms: () => void }) {
  const conflicts = detail.termConflicts.filter((item) => item.unresolved_events > 0);
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
  onPolish,
  onInitialize,
  onSaveConfig,
  onReanalyze,
  onOpenTerms,
}: {
  config?: Config;
  detail: Detail;
  busy: string | null;
  retranslationProgress: RetranslationProgress | null;
  onPolish: (v: boolean) => Promise<void>;
  onInitialize: (value: TaskConfigDraft) => void;
  onSaveConfig: (value: TaskConfigDraft) => Promise<void>;
  onReanalyze: (value: TaskConfigDraft) => Promise<void>;
  onOpenTerms: () => void;
}) {
  const [tab, setTab] = useState<"task" | "memory">("task");
  const [draft, setDraft] = useState<TaskConfigDraft | null>(null);
  useEffect(() => {
    if (!config) return;
    setDraft({
      sourceLanguage: detail.taskInitialized
        ? detail.project.source_language
        : config.language.source,
      maxCharsPerSegment: config.segment.max_chars_per_segment,
      maxCharsPerBatch: config.segment.max_chars_per_batch,
      recentContextChars: config.pipeline.recent_context_chars,
      timeoutSecs: config.llm.timeout_secs,
      maxRetries: config.llm.max_retries,
      fullBook: config.analysis.full_book,
    });
  }, [config, detail.project.source_language, detail.taskInitialized]);
  const progress = Math.round(
    (detail.project.chapters_completed /
      Math.max(1, detail.project.chapters_total)) *
      100,
  );
  const conflictCount = detail.pendingConflicts;
  const retranslationPercent = retranslationProgress
    ? Math.round(
        (retranslationProgress.completed /
          Math.max(1, retranslationProgress.total)) *
          100,
      )
    : 0;
  return (
    <aside className="inspector">
      <div className="inspector-tabs">
        <button
          className={tab === "task" ? "active" : ""}
          onClick={() => setTab("task")}
        >
          任务配置
        </button>
        <button
          className={tab === "memory" ? "active" : ""}
          onClick={() => setTab("memory")}
        >
          术语与记忆
        </button>
      </div>
      <div className="inspector-body">
        {!draft ? (
          <div className="panel-empty">正在读取任务配置…</div>
        ) : tab === "task" ? (
          <>
            <Field label="语言方向">
              <div className="direction configurable-direction">
                <select
                  value={draft.sourceLanguage}
                  disabled={detail.taskInitialized || Boolean(busy)}
                  onChange={(event) =>
                    setDraft({ ...draft, sourceLanguage: event.target.value })
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
            <Field label="模型选择">
              <div className="select-like">
                {config?.llm.model ?? "—"}
                <ChevronDown />
              </div>
              <small>提供商：{config?.llm.provider ?? "—"}</small>
            </Field>
            <section className="inspector-section">
              <b>分段策略</b>
              <div className="compact-fields">
                <NumberField
                  label="每段字符数"
                  value={draft.maxCharsPerSegment}
                  disabled={Boolean(busy)}
                  onChange={(value) => setDraft({ ...draft, maxCharsPerSegment: value })}
                />
                <NumberField
                  label="每批字符数"
                  value={draft.maxCharsPerBatch}
                  disabled={Boolean(busy)}
                  onChange={(value) => setDraft({ ...draft, maxCharsPerBatch: value })}
                />
              </div>
              <small className="field-hint">每段字符数只影响之后新导入的项目；每批字符数会用于后续翻译。</small>
            </section>
            <section className="inspector-section">
              <b>初始化选项</b>
              <label className="switch-row">
                <span>全书译前分析</span>
                <button
                  type="button"
                  className={`toggle ${draft.fullBook ? "on" : ""}`}
                  disabled={Boolean(busy)}
                  aria-pressed={draft.fullBook}
                  onClick={() => setDraft({ ...draft, fullBook: !draft.fullBook })}
                >
                  <i />
                </button>
              </label>
            </section>
            <details className="advanced-config">
              <summary>高级模型配置</summary>
              <div className="compact-fields">
                <NumberField
                  label="超时（秒）"
                  value={draft.timeoutSecs}
                  disabled={Boolean(busy)}
                  onChange={(value) => setDraft({ ...draft, timeoutSecs: value })}
                />
                <NumberField
                  label="重试次数"
                  value={draft.maxRetries}
                  min={0}
                  disabled={Boolean(busy)}
                  onChange={(value) => setDraft({ ...draft, maxRetries: value })}
                />
              </div>
            </details>
            <label className="switch-row">
              <span>译后润色</span>
              <button
                type="button"
                className={`toggle ${config?.pipeline.polish ? "on" : ""}`}
                disabled={!config || Boolean(busy)}
                aria-pressed={Boolean(config?.pipeline.polish)}
                aria-label="译后润色"
                onClick={() => void onPolish(!config?.pipeline.polish)}
              >
                <i />
              </button>
            </label>
            <button
              type="button"
              className="secondary save-task-config"
              disabled={Boolean(busy) || !validTaskConfig(draft)}
              onClick={() => void onSaveConfig(draft)}
            >
              {busy === "保存任务配置" ? "正在保存…" : "保存配置"}
            </button>
            {detail.taskInitialized ? (
              <button
                type="button"
                className="initialize-task icon-label"
                disabled={Boolean(busy) || !validTaskConfig(draft)}
                onClick={() => void onReanalyze(draft)}
              >
                {busy === "重新分析" ? <LoaderCircle className="spin" /> : <RotateCcw />}
                {busy === "重新分析" ? "正在重新分析" : "重新分析"}
              </button>
            ) : (
              <button
                type="button"
                className="initialize-task primary icon-label"
                disabled={Boolean(busy) || !validTaskConfig(draft)}
                onClick={() => onInitialize(draft)}
              >
                {busy === "项目初始化" ? <LoaderCircle className="spin" /> : <Play />}
                {busy === "项目初始化" ? "正在初始化任务" : "初始化任务"}
              </button>
            )}
            <hr />
            <section className="flow">
              <header><b>项目进度</b><strong>{progress}%</strong></header>
              <div className="big-progress"><i style={{ width: `${progress}%` }} /></div>
              <p className="done"><CheckCircle2 />已完成 {detail.project.chapters_completed} 章</p>
              {retranslationProgress?.projectId === detail.project.id && (
                <>
                  <header>
                    <b>本次重译进度</b>
                    <strong>{retranslationPercent}%</strong>
                  </header>
                  <div className="big-progress">
                    <i style={{ width: `${retranslationPercent}%` }} />
                  </div>
                  <p className="active">
                    <CircleDashed />
                    已处理 {retranslationProgress.completed} / {retranslationProgress.total} 项
                    · 成功 {retranslationProgress.succeeded} · 失败 {retranslationProgress.failed}
                  </p>
                </>
              )}
              <p className={detail.project.status === "failed" ? "error" : "active"}>
                <CircleDashed />
                {detail.taskInitialized ? statusText[detail.project.status] : "等待初始化任务"}
              </p>
              <p><Circle />生成校对报告 <em>阶段 8 待实现</em></p>
            </section>
          </>
        ) : (
          <>
            <section className="memory-summary">
              <div><strong>{detail.terms.length}</strong><span>术语总数</span></div>
              <div className={conflictCount ? "has-conflicts" : ""}>
                <strong>{conflictCount}</strong><span>待处理冲突</span>
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
                    setDraft({ ...draft, recentContextChars: Number(event.target.value) })
                  }
                />
                <span>字符</span>
              </div>
              <small>翻译下一批时携带的近期已译内容上限。</small>
            </Field>
            <button
              type="button"
              className="secondary save-task-config"
              disabled={Boolean(busy) || !validTaskConfig(draft)}
              onClick={() => void onSaveConfig(draft)}
            >
              {busy === "保存任务配置" ? "正在保存…" : "保存记忆设置"}
            </button>
            <button type="button" className="text-action" onClick={onOpenTerms}>
              打开术语库
              {conflictCount > 0 && ` · ${conflictCount} 个冲突`}
            </button>
          </>
        )}
      </div>
    </aside>
  );
}
function NumberField({ label, value, min = 1, disabled, onChange }: { label: string; value: number; min?: number; disabled: boolean; onChange: (value: number) => void }) {
  return (
    <label>
      <span>{label}</span>
      <input type="number" min={min} step="1" value={value} disabled={disabled} onChange={(event) => onChange(Number(event.target.value))} />
    </label>
  );
}
function validTaskConfig(value: TaskConfigDraft) {
  return Boolean(value.sourceLanguage.trim()) &&
    Number.isInteger(value.maxCharsPerSegment) && value.maxCharsPerSegment > 0 &&
    Number.isInteger(value.maxCharsPerBatch) && value.maxCharsPerBatch > 0 &&
    Number.isInteger(value.recentContextChars) && value.recentContextChars > 0 &&
    Number.isInteger(value.timeoutSecs) && value.timeoutSecs > 0 &&
    Number.isInteger(value.maxRetries) && value.maxRetries >= 0;
}
function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <label className="field">
      <b>{label}</b>
      {children}
    </label>
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
      <header>
        <div>
          <h1>翻译项目</h1>
          <p>管理本机状态目录中的所有书籍。</p>
        </div>
        <button className="primary icon-label" onClick={onImport}>
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
              <article key={project.id}>
                <button
                  className="project-open"
                  onClick={() => onSelect(project.id)}
                >
                  <ProjectCover project={project} className="cover" />
                  <section className={onDelete ? "with-actions" : ""}>
                    <h3>{project.title}</h3>
                    <p>{fileName(project.source_file)}</p>
                    <div className="progress">
                      <i style={{ width: `${progress}%` }} />
                    </div>
                    <footer>
                      <span>
                        {project.chapters_completed} / {project.chapters_total}{" "}
                        章
                      </span>
                      <em className={project.status}>
                        {projectStatusText(project)}
                      </em>
                    </footer>
                  </section>
                </button>
                {onDelete && (
                  <button
                    className="project-card-delete icon-label"
                    disabled={busy}
                    onClick={() => onDelete(project)}
                  >
                    <Trash2 />
                    删除
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
  config,
  taskBusy,
  retranslationProgress,
  onRetranslate,
  onReload,
}: {
  detail: Detail | null;
  config?: Config;
  taskBusy: boolean;
  retranslationProgress: RetranslationProgress | null;
  onRetranslate: (itemIds: string[]) => Promise<void>;
  onReload: () => Promise<void>;
}) {
  const [showResolved, setShowResolved] = useState(false);
  const [conflictIndex, setConflictIndex] = useState(0);
  const [editing, setEditing] = useState<string | null>(null);
  const [target, setTarget] = useState("");
  const [impact, setImpact] = useState<AffectedContent[]>([]);
  const [impactSource, setImpactSource] = useState<string | null>(null);
  const [allConflictImpacts, setAllConflictImpacts] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [working, setWorking] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const activeRetranslation =
    retranslationProgress?.projectId === detail?.project.id
      ? retranslationProgress
      : null;
  const conflicts = (detail?.termConflicts ?? []).filter(
    (item) => showResolved || item.unresolved_events > 0,
  );
  const conflict = conflicts[Math.min(conflictIndex, Math.max(0, conflicts.length - 1))];
  useEffect(() => {
    setConflictIndex((index) => Math.min(index, Math.max(0, conflicts.length - 1)));
  }, [conflicts.length]);
  useEffect(() => {
    setTarget(conflict?.manual_target ?? conflict?.current_target ?? "");
  }, [conflict?.source, conflict?.manual_target, conflict?.current_target]);

  const scan = async (source: string) => {
    if (!detail) return;
    const items = await invoke<AffectedContent[]>("ui_scan_term_impact", {
      projectId: detail.project.id,
      source,
    });
    setImpact(items);
    setImpactSource(source);
    setAllConflictImpacts(false);
    setSelected(new Set());
  };
  const scanAll = async (selectAll = true) => {
    if (!detail) return;
    const sources = detail.termConflicts
      .filter((item) => item.policy === "fixed")
      .map((item) => item.source);
    const groups = await Promise.all(sources.map((source) =>
      invoke<AffectedContent[]>("ui_scan_term_impact", {
        projectId: detail.project.id,
        source,
      }),
    ));
    const items = [...new Map(groups.flat().map((item) => [item.id, item])).values()];
    setImpact(items);
    setImpactSource(null);
    setAllConflictImpacts(true);
    setSelected(selectAll ? new Set(items.map((item) => item.id)) : new Set());
    return items;
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
  const resolve = (source: string) => run("保存裁定", async () => {
    if (!detail || !target.trim()) return;
    await invoke("ui_resolve_term", { projectId: detail.project.id, source, target: target.trim() });
    setEditing(null);
    await scan(source);
    await onReload();
  });
  const setPolicy = (source: string, policy: TermPolicy) => run("更新规则", async () => {
    if (!detail) return;
    await invoke("ui_set_term_policy", { projectId: detail.project.id, source, policy });
    await scan(source);
    await onReload();
  });
  const undo = (source: string) => run("撤销裁定", async () => {
    if (!detail) return;
    await invoke("ui_undo_term_resolution", { projectId: detail.project.id, source });
    setImpact([]);
    await onReload();
  });
  const retranslate = () => run("重译", async () => {
    if (!detail || !selected.size) return;
    const chapters = new Set(impact.filter((item) => selected.has(item.id)).map((item) => item.chapter)).size;
    const confirmed = await confirmDialog(
      `将重译 ${selected.size} 项、涉及 ${chapters} 章${config?.pipeline.polish ? "，并重新执行润色" : ""}。此操作会消耗 API Token，是否继续？`,
      { title: "确认选择性重译", kind: "warning" },
    );
    if (!confirmed) return;
    await onRetranslate([...selected]);
    if (allConflictImpacts) await scanAll(false);
    else if (impactSource) await scan(impactSource);
    await onReload();
  });
  const retranslateResolved = () => run("重译已处理冲突", async () => {
    const items = await scanAll(false);
    if (items.length) await onRetranslate(items.map((item) => item.id));
    await onReload();
  });
  const restore = (item: AffectedContent) => run("恢复译文", async () => {
    if (!detail) return;
    await invoke("ui_restore_translation", { projectId: detail.project.id, itemId: item.id });
    if (allConflictImpacts) await scanAll(false);
    else if (impactSource) await scan(impactSource);
    await onReload();
  });
  return (
    <div className="page-view">
      <header>
        <div>
          <h1>术语库</h1>
          <p>
            {detail
              ? `${detail.project.title} · ${detail.terms.length} 条术语`
              : "选择项目后查看术语"}
          </p>
        </div>
        <label className="history-toggle">
          <input type="checkbox" checked={showResolved} onChange={(event) => setShowResolved(event.target.checked)} />
          查看已处理记录
        </label>
      </header>
      {error && <div className="term-error">{error}</div>}
      {detail && conflict && (
        <section className="conflict-panel">
          <header>
            <div>
              <small>译名冲突 · {conflict.unresolved_events} 个待处理事件</small>
              <h2>{conflict.source}</h2>
              <p>当前固定译名：<b>{conflict.current_target}</b></p>
            </div>
            <nav>
              <button disabled={conflictIndex === 0} onClick={() => setConflictIndex((value) => value - 1)}>上一个</button>
              <span>{conflictIndex + 1} / {conflicts.length}</span>
              <button disabled={conflictIndex >= conflicts.length - 1} onClick={() => setConflictIndex((value) => value + 1)}>下一个</button>
            </nav>
          </header>
          <div className="candidate-grid">
            {conflict.candidates.map((candidate) => (
              <button key={candidate.target} onClick={() => setTarget(candidate.target)} className={target === candidate.target ? "active" : ""}>
                <b>{candidate.target}</b>
                <span>{candidate.occurrences} 次 · {candidate.chapters.map((chapter) => `第 ${chapter + 1} 章`).join("、")}</span>
                {candidate.evidence.slice(0, 3).map((evidence, index) => (
                  <small key={index}>{evidence.source_excerpt || "旧数据库无原文片段"}<br />{evidence.target_excerpt || "旧数据库无译文片段"}</small>
                ))}
              </button>
            ))}
          </div>
          <div className="conflict-actions">
            <input value={target} placeholder="选择候选或输入新的固定译名" onChange={(event) => setTarget(event.target.value)} />
            <button className="primary" disabled={!target.trim() || Boolean(working)} onClick={() => void resolve(conflict.source)}>保存人工裁定</button>
            <button disabled={Boolean(working)} onClick={() => void setPolicy(conflict.source, "non_fixed")}>标记为非固定术语</button>
            <button disabled={Boolean(working)} onClick={() => void setPolicy(conflict.source, "ignored")}>忽略术语</button>
            {conflict.policy !== "automatic" && <button disabled={Boolean(working)} onClick={() => void undo(conflict.source)}>撤销并恢复待处理</button>}
          </div>
        </section>
      )}
      {detail && !conflict && detail.terms.length === 0 && (
        <div className="panel-empty">当前没有{showResolved ? "冲突记录" : "待处理冲突"}</div>
      )}
      {detail && <button className="primary" disabled={taskBusy || Boolean(working)} onClick={() => void retranslateResolved()}>重译全部已处理冲突</button>}
      {impact.length > 0 && (
        <section className="impact-panel">
          <header>
            <div><h2>{allConflictImpacts ? "全部已裁定冲突可能影响的内容" : "当前术语可能影响的内容"}</h2><p>按与术语提示一致的边界规则扫描，不代表精确调用追踪；重复命中的内容只显示一次。</p></div>
            <button onClick={() => setSelected(selected.size === impact.length ? new Set() : new Set(impact.map((item) => item.id)))}>
              {selected.size === impact.length ? "取消全选" : "选择当前列表全部"}
            </button>
          </header>
          {impact.map((item) => (
            <div className="impact-row" key={item.id}>
              <input type="checkbox" checked={selected.has(item.id)} onChange={() => setSelected((current) => {
                const next = new Set(current); next.has(item.id) ? next.delete(item.id) : next.add(item.id); return next;
              })} />
              <span><b>第 {item.chapter + 1} 章 · {item.kind}</b><small>{item.source}</small><em>{item.currentTarget}</em>{item.retranslationError && <strong>重译失败：{item.retranslationError}</strong>}</span>
              <button disabled={!item.previousTarget || Boolean(working)} onClick={() => void restore(item)}>恢复旧译文</button>
            </div>
          ))}
          <footer>
            <span>
              {activeRetranslation
                ? `重译进度 ${activeRetranslation.completed}/${activeRetranslation.total} · 成功 ${activeRetranslation.succeeded} · 失败 ${activeRetranslation.failed}`
                : `已选择 ${selected.size} 项；默认不会自动重译。`}
            </span>
            <button className="primary" disabled={!selected.size || taskBusy || Boolean(working)} onClick={() => void retranslate()}>
              {activeRetranslation
                ? `正在重译 ${activeRetranslation.completed}/${activeRetranslation.total}`
                : taskBusy
                  ? "其他任务结束后可重译"
                  : "重译所选内容"}
            </button>
          </footer>
        </section>
      )}
      {detail && detail.terms.length ? (
        <div className="term-table">
          <div className="term-row term-head">
            <span>原文</span>
            <span>固定译名</span>
            <span>类型</span>
            <span>首次出现</span>
            <span>状态</span>
            <span>操作</span>
          </div>
          {detail.terms.map((term) => (
            <div className="term-row" key={term.source}>
              <b>{term.source}</b>
              {editing === term.source ? (
                <input
                  autoFocus
                  value={target}
                  onChange={(e) => setTarget(e.target.value)}
                />
              ) : (
                <span>{term.target}</span>
              )}
              <span>{termTypeText[term.type] ?? "其他"}</span>
              <span>第 {term.first_chapter + 1} 章</span>
              <em className={term.status}>
                {term.policy === "ignored"
                  ? "已忽略"
                  : term.policy === "non_fixed"
                    ? "非固定"
                    : term.status === "conflict"
                  ? "有冲突"
                  : term.status === "resolved"
                    ? "已裁定"
                    : "正常"}
              </em>
              {editing === term.source ? (
                <button onClick={() => void resolve(term.source)}>保存</button>
              ) : (
                <button
                  onClick={() => {
                    if (term.policy === "ignored" || term.policy === "non_fixed") {
                      void setPolicy(term.source, "automatic");
                    } else {
                      setEditing(term.source);
                      setTarget(term.target);
                    }
                  }}
                >
                  {term.policy === "ignored" || term.policy === "non_fixed" ? "恢复" : "修改"}
                </button>
              )}
            </div>
          ))}
        </div>
      ) : (
        <div className="page-empty">暂无术语数据</div>
      )}
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
  onSaveGeneral: (visibleSegments: number) => Promise<void>;
}) {
  const [tab, setTab] = useState<"model" | "general">("model");
  const [draft, setDraft] = useState(config);
  const [visibleSegments, setVisibleSegments] = useState(
    config?.general.visible_segments ?? 100,
  );
  const [apiKey, setApiKey] = useState("");
  const [showKey, setShowKey] = useState(false);
  const [presetName, setPresetName] = useState("");
  const [editedFields, setEditedFields] = useState<Set<string>>(new Set());
  useEffect(() => {
    setDraft(config);
    setVisibleSegments(config?.general.visible_segments ?? 100);
    setApiKey("");
    setShowKey(false);
    const preset = providerPresets.find((item) =>
      item.provider === config?.llm.provider.trim().toLowerCase() &&
      item.base_url === config?.llm.base_url?.replace(/\/$/, ""));
    setPresetName(preset?.name ?? "");
    setEditedFields(new Set(["base_url", "model"].filter((key) => {
      const value = config?.llm[key as keyof Config["llm"]];
      return value !== undefined && value !== (preset?.[key as keyof typeof preset]);
    })));
  }, [config]);
  if (!draft) return <div className="page-empty">正在读取设置…</div>;
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
      if (reset || !editedFields.has(key)) llm[key] = preset[key];
    }
    if (reset) setEditedFields(new Set());
    setDraft({ ...draft, llm });
    setApiKey("");
    setShowKey(false);
  };
  const sameProvider = draft.llm.provider.trim().toLowerCase() === config?.llm.provider.trim().toLowerCase();
  let noKey = false;
  try {
    const host = new URL(draft.llm.base_url ?? "").hostname;
    noKey = ["openai-chat", "openai-compatible"].includes(draft.llm.provider.trim().toLowerCase()) &&
      !draft.llm.api_key_env.trim() && (host === "localhost" || host === "[::1]" || /^127(?:\.\d{1,3}){3}$/.test(host));
  } catch { /* The backend reports invalid URLs when validating. */ }
  const configured = Boolean(sameProvider && credential?.configured && credential.source !== "none");
  const savingModel = busy === "验证模型";
  const savingGeneral = busy === "保存通用设置";
  return (
    <div className="page-view model-page">
      <header>
        <div>
          <h1>设置</h1>
          <p>配置翻译模型与连接凭据。</p>
        </div>
      </header>
      <div className="settings-layout">
        <nav>
          <button
            className={tab === "model" ? "active" : ""}
            onClick={() => setTab("model")}
          >
            <Braces />
            模型与 API
          </button>
          <button
            className={tab === "general" ? "active" : ""}
            onClick={() => setTab("general")}
          >
            <PanelTop />
            通用
          </button>
          <button className="icon-label" disabled>
            <Settings />
            高级
          </button>
        </nav>
        {tab === "model" ? (
          <div className="settings-card">
            <div className="settings-heading">
              <div>
                <h2>服务预设</h2>
                <p>选择模型提供商，并验证用于翻译的 API Key。</p>
              </div>
              {(configured || noKey) && (
                <span className="credential-ok icon-label">
                  <Check />
                  {noKey ? "本地免密" : "API Key 已配置"}
                </span>
              )}
            </div>
            <Field label="服务预设">
              <div className="preset-input">
                <select aria-label="服务预设" value={presetName} onChange={(e) => applyPreset(e.target.value)}>
                  <option value="">自定义</option>
                  {providerPresets.map((preset) => <option key={preset.name} value={preset.name}>{preset.name}</option>)}
                </select>
                <button type="button" title="重置为预设默认值" aria-label="重置为预设默认值" disabled={!presetName} onClick={() => applyPreset(presetName, true)}><RotateCcw size={16} /></button>
              </div>
            </Field>
            <Field label="协议类型">
              <select
                aria-label="协议类型"
                value={draft.llm.provider.trim().toLowerCase()}
                onChange={(e) => {
                  field("provider", e.target.value);
                  setApiKey("");
                }}
              >
                <option value="openai-compatible">OpenAI-compatible (Chat Completions)</option>
                <option value="openai-responses">OpenAI Responses</option>
                <option value="anthropic">Anthropic(Messages)</option>
              </select>
            </Field>
            <Field label="模型">
              <input
                value={draft.llm.model}
                onChange={(e) => field("model", e.target.value)}
              />
            </Field>
            <Field label="API地址(Base URL)">
              <input
                value={draft.llm.base_url ?? ""}
                onChange={(e) => field("base_url", e.target.value)}
              />
            </Field>
            <Field label="API Key">
              <div className="secret-input">
                <input
                  autoFocus={!credential?.configured}
                  type={showKey ? "text" : "password"}
                  disabled={noKey}
                  value={noKey ? "" : apiKey}
                  onChange={(e) => setApiKey(e.target.value)}
                  placeholder={
                    noKey ? "本地免密" : configured
                      ? "已配置，留空则使用现有凭据"
                      : "粘贴 API Key"
                  }
                />
                <button type="button" title={showKey ? "隐藏密钥" : "显示密钥"} aria-label={showKey ? "隐藏密钥" : "显示密钥"} onClick={() => setShowKey(!showKey)}>
                  {showKey ? <EyeOff size={16} /> : <Eye size={16} />}
                </button>
              </div>
              <small>
                Key 仅保存在当前用户的 TransItPls 配置目录中，不会写入书籍项目。
              </small>
            </Field>
            <div className="settings-actions">
              <button
                className="primary"
                disabled={busy !== null}
                onClick={() => void onSaveModel(draft, noKey ? "" : apiKey)}
              >
                {savingModel ? "正在验证…" : "测试连接并保存"}
              </button>
            </div>
            <footer>
              模型配置：
              <code>{configPath ?? "保存后创建 transitpls.toml"}</code>
            </footer>
          </div>
        ) : (
          <div className="settings-card">
            <div className="settings-heading">
              <div>
                <h2>通用设置</h2>
                <p>调整工作台中每个章节默认加载的段落数量。</p>
              </div>
            </div>
            <Field label="每章默认显示段落数">
              <input
                type="number"
                min="1"
                step="1"
                value={visibleSegments}
                onChange={(e) => setVisibleSegments(Number(e.target.value))}
              />
              <small>
                章节切换时先显示这些段落，点击“显示更多段落”可继续查看其余内容。
              </small>
            </Field>
            <div className="settings-actions">
              <button
                className="primary"
                disabled={
                  busy !== null ||
                  visibleSegments < 1 ||
                  !Number.isInteger(visibleSegments)
                }
                onClick={() => void onSaveGeneral(visibleSegments)}
              >
                {savingGeneral ? "正在保存…" : "保存通用设置"}
              </button>
            </div>
            <footer>
              配置文件：
              <code>{configPath ?? "保存后创建 transitpls.toml"}</code>
            </footer>
          </div>
        )}
      </div>
    </div>
  );
}
function ReviewPlaceholder() {
  return (
    <div className="review-placeholder">
      <div className="review-icon">
        <BadgeCheck />
      </div>
      <h1>审校工作区</h1>
      <p>
        界面已经就位。按照当前开发安排，阶段 8 的 Review 与报告能力暂不接入。
      </p>
      <button disabled>运行只读审校</button>
      <small>不会对译文执行自动写回</small>
    </div>
  );
}
function EmptyState({ onImport }: { onImport: () => void }) {
  return (
    <div className="empty-state">
      <Logo />
      <h1>开始第一个翻译项目</h1>
      <p>
        导入 EPUB 或 TXT 文件，TransItPls 会沿用 CLI 的项目状态与断点续跑能力。
      </p>
      <button className="primary icon-label" onClick={onImport}>
        <Plus />
        选择书籍文件
      </button>
      <small>暂不支持 PDF、DOCX 和字幕文件</small>
    </div>
  );
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
      general: { visible_segments: 100 },
    },
  };
}
