import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { confirm } from "@tauri-apps/plugin-dialog";
import { History, Pencil, RotateCcw, LoaderCircle } from "lucide-react";
import { useEditingGuard } from "./EditingGuard";

export interface EditableSegment {
  id: string;
  ordinal: number;
  source: string;
  target: string | null;
  kind: string;
  status: string;
  polish_status?: string | null;
  meta?: Record<string, unknown>;
}
interface Revision {
  id: number;
  target: string;
  kind: string;
  created_at: string | null;
  model: string | null;
}
interface Preview {
  id: string;
  target: string;
  base_revision: number;
  base_target: string;
  model: string | null;
}
interface SegmentDetail {
  segment: EditableSegment;
  revisions: Revision[];
  revision: number;
  preview: Preview | null;
}
type Edit =
  | { kind: "manual"; target: string }
  | { kind: "restore"; revision_id: number }
  | { kind: "adopt"; preview_id: string };
const kindText: Record<string, string> = {
  translation: "初译",
  polish: "润色",
  retranslation: "重译",
  manual: "人工修改",
  restore: "采用历史版本",
  adopt: "采用重译",
  legacy: "旧项目当前译文",
  legacy_draft: "旧项目保留草稿",
  legacy_previous: "旧项目保留译文",
};

// 以 Unicode 字符比较连续改动，避免把代理对拆开；两侧始终展示完整段落。
export function ChangedText({
  value,
  other,
  addition,
}: {
  value: string;
  other: string;
  addition: boolean;
}) {
  const a = Array.from(value),
    b = Array.from(other);
  let start = 0,
    end = 0;
  while (start < a.length && start < b.length && a[start] === b[start]) start++;
  while (
    end < a.length - start &&
    end < b.length - start &&
    a[a.length - end - 1] === b[b.length - end - 1]
  )
    end++;
  const middle = a.slice(start, a.length - end).join("");
  return (
    <>
      {a.slice(0, start).join("")}
      {middle && (addition ? <ins>{middle}</ins> : <del>{middle}</del>)}
      {end > 0 && a.slice(-end).join("")}
    </>
  );
}

export function SegmentCard({
  segment,
  projectId,
  busy,
  onBusy,
  onChanged,
}: {
  segment: EditableSegment;
  projectId: string;
  busy: boolean;
  onBusy: (label: string | null) => void;
  onChanged: () => Promise<void>;
}) {
  const [data, setData] = useState<SegmentDetail | null>(null);
  const [mode, setMode] = useState<"read" | "edit" | "history" | "preview">(
    "read",
  );
  const [draft, setDraft] = useState("");
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [working, setWorking] = useState(false);
  const workingRef = useRef(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const editor = useRef<HTMLTextAreaElement>(null);
  const mounted = useRef(true);
  const { setDirty, setPending } = useEditingGuard();
  const dirty = mode === "edit" && draft !== (data?.segment.target ?? "");
  const protectedTarget = segment.meta?.translation_protected === true;
  const stale =
    !!data &&
    (data.segment.target !== segment.target ||
      (Array.isArray(segment.meta?.translation_revisions) &&
        (
          segment.meta.translation_revisions[
            segment.meta.translation_revisions.length - 1
          ] as Revision | undefined
        )?.id !== data.revision));
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  useEffect(() => {
    if (!dirty) return;
    setDirty({
      id: segment.id,
      discard: () => {
        setMode("read");
        setDraft("");
      },
    });
    return () => setDirty(null);
  }, [dirty, segment.id, setDirty]);
  useEffect(() => {
    if (mode === "edit") editor.current?.focus();
  }, [mode]);

  async function load() {
    const next = await invoke<SegmentDetail>("ui_segment_history", {
      projectId,
      segmentId: segment.id,
    });
    if (mounted.current) setData(next);
    return next;
  }
  async function run(action: () => Promise<void>, label?: string) {
    if (workingRef.current) return;
    workingRef.current = true;
    setWorking(true);
    setError(null);
    setMessage(null);
    if (label) {
      setPending(segment.id);
      onBusy(label);
    }
    try {
      await action();
    } catch (cause) {
      if (mounted.current) setError(String(cause));
    } finally {
      workingRef.current = false;
      if (mounted.current) setWorking(false);
      if (label) {
        setPending(null);
        onBusy(null);
      }
    }
  }
  function startEdit() {
    void run(async () => {
      const next = await load();
      if (!mounted.current) return;
      setDraft(next.segment.target ?? "");
      setMode("edit");
    });
  }
  function showHistory() {
    void run(async () => {
      const next = await load();
      if (!mounted.current) return;
      setSelectedId(
        next.revisions[next.revisions.length - 2]?.id ?? next.revision,
      );
      setMode("history");
    });
  }
  function save(edit: Edit) {
    if (!data || busy || working || stale) return;
    void run(async () => {
      const next = await invoke<SegmentDetail>("ui_save_segment", {
        request: {
          project_id: projectId,
          segment_id: segment.id,
          expected_revision: data.revision,
          expected_target: data.segment.target ?? "",
          edit,
        },
      });
      if (mounted.current) {
        setData(next);
        setMode("read");
        setDraft("");
        setDirty(null);
        setMessage("已保存，旧版本已保留在历史中");
      }
      try {
        await onChanged();
      } catch (cause) {
        setError(`译文已保存，但刷新失败：${String(cause)}`);
      }
    }, "保存译文");
  }
  function retry() {
    if (busy || working) return;
    void run(async () => {
      if (
        !(await confirm(
          "将调用当前模型生成此段的新译文，消耗 API Token。结果会先供你比较，采用后才替换当前译文。",
          {
            title: "重译此段",
            okLabel: "生成建议",
            cancelLabel: "取消",
          },
        ))
      )
        return;
      const next = await invoke<SegmentDetail>("ui_preview_retranslation", {
        projectId,
        segmentId: segment.id,
      });
      if (mounted.current) {
        setData(next);
        setMode("preview");
      }
    }, "生成重译建议");
  }
  const selected = data?.revisions.find(
    (revision) => revision.id === selectedId,
  );
  const proposed =
    mode === "preview" ? data?.preview?.target : selected?.target;
  const previewStale =
    mode === "preview" &&
    !!data?.preview &&
    (data.preview.base_revision !== data.revision ||
      data.preview.base_target !== data.segment.target);
  const revisions = Array.isArray(segment.meta?.translation_revisions)
    ? (segment.meta.translation_revisions as Revision[])
    : [];
  const latest = revisions[revisions.length - 1];
  const words = segment.source.trim().split(/\s+/).filter(Boolean).length;
  return (
    <article
      className={`segment-card ${segment.status}`}
      data-editing-segment={segment.id}
    >
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
          <span className="seg-meta">
            {protectedTarget
              ? "人工保护 · 批量润色跳过"
              : latest
                ? kindText[latest.kind]
                : segment.polish_status === "succeeded"
                  ? "已润色"
                  : segment.target
                    ? "已翻译"
                    : "待翻译"}
          </span>
          {segment.polish_status === "failed" && (
            <em className="status-chip danger">润色失败</em>
          )}
          {segment.polish_status === "pending" && !protectedTarget && (
            <em className="status-chip warn">待润色</em>
          )}
          {segment.status === "failed" && (
            <em className="status-chip danger">翻译失败</em>
          )}
        </header>
        {mode === "edit" ? (
          <>
            <textarea
              ref={editor}
              className="segment-edit-input"
              aria-label={`编辑段落 ${segment.ordinal + 1} 的译文`}
              value={draft}
              disabled={working}
              onChange={(event) => setDraft(event.target.value)}
              onKeyDown={(event) => {
                if ((event.ctrlKey || event.metaKey) && event.key === "Enter") {
                  event.preventDefault();
                  if (draft.trim() && dirty)
                    save({ kind: "manual", target: draft });
                }
              }}
            />
            <details className="segment-saved-text">
              <summary>查看当前已保存的译文</summary>
              <p>{data?.segment.target}</p>
            </details>
            <div className="segment-edit-hint">
              {dirty ? "有未保存的修改" : "尚未修改"} · Ctrl / ⌘ + Enter 保存
            </div>
            <div className="segment-actions">
              <button
                className="btn btn-quiet sm"
                disabled={working}
                onClick={() => {
                  setMode("read");
                  setDraft("");
                }}
              >
                取消
              </button>
              <button
                className="btn btn-primary sm"
                disabled={busy || working || !dirty || !draft.trim() || stale}
                onClick={() => save({ kind: "manual", target: draft })}
              >
                {working ? <LoaderCircle className="spin" /> : null}保存修改
              </button>
            </div>
          </>
        ) : (
          <>
            {segment.target ? (
              <p>{segment.target}</p>
            ) : (
              <div className="target-empty">
                <span>等待翻译</span>
                <small>运行本章翻译后将在此显示译文</small>
              </div>
            )}
            <div className="segment-actions">
              <button
                className="btn btn-quiet sm"
                disabled={busy || working || !segment.target}
                onClick={startEdit}
              >
                <Pencil />
                编辑
              </button>
              <button
                className="btn btn-quiet sm"
                disabled={working || !segment.target}
                onClick={showHistory}
              >
                <History />
                历史{revisions.length ? ` · ${revisions.length}` : ""}
              </button>
              <button
                className="btn btn-quiet sm"
                disabled={busy || working || !segment.target}
                onClick={retry}
              >
                <RotateCcw />
                重译
              </button>
            </div>
          </>
        )}
      </div>
      {(mode === "history" || mode === "preview") && data && (
        <section className="segment-history" aria-label="段落版本比较">
          <div className="segment-history-heading">
            <strong>
              {mode === "preview" ? "重译建议 · 尚未采用" : "修改历史"}
            </strong>
            <div>
              {data.preview && mode === "history" && (
                <button
                  className="btn btn-quiet sm"
                  onClick={() => setMode("preview")}
                >
                  查看重译建议
                </button>
              )}
              <button
                className="btn btn-quiet sm"
                onClick={() => setMode("read")}
              >
                收起
              </button>
            </div>
          </div>
          <div className="segment-history-body">
            {mode === "history" && (
              <nav className="segment-versions" aria-label="选择历史版本">
                {[...data.revisions].reverse().map((revision) => (
                  <button
                    key={revision.id}
                    aria-pressed={revision.id === selectedId}
                    onClick={() => setSelectedId(revision.id)}
                  >
                    <b>
                      {kindText[revision.kind] ?? revision.kind}
                      {revision.id === data.revision ? " · 当前" : ""}
                    </b>
                    <small>
                      记录 {revision.id} ·{" "}
                      {revision.created_at
                        ? new Date(revision.created_at).toLocaleString()
                        : "时间未知"}
                    </small>
                    {revision.model && <small>{revision.model}</small>}
                  </button>
                ))}
              </nav>
            )}
            <div className="segment-comparison">
              <small>当前译文（标出差异）</small>
              <p>
                <ChangedText
                  value={segment.target ?? ""}
                  other={proposed ?? ""}
                  addition={false}
                />
              </p>
              <small>
                {mode === "preview"
                  ? `建议译文 · ${data.preview?.model ?? "模型未知"}`
                  : `所选版本 · ${kindText[selected?.kind ?? ""] ?? ""}`}
              </small>
              <p>
                <ChangedText
                  value={proposed ?? ""}
                  other={segment.target ?? ""}
                  addition
                />
              </p>
              <div className="segment-actions">
                <button
                  className="btn btn-primary sm"
                  disabled={
                    busy ||
                    working ||
                    stale ||
                    previewStale ||
                    !proposed ||
                    proposed === segment.target
                  }
                  onClick={() => {
                    if (mode === "preview" && data.preview)
                      save({ kind: "adopt", preview_id: data.preview.id });
                    else if (selected)
                      save({ kind: "restore", revision_id: selected.id });
                  }}
                >
                  {mode === "preview" ? "采用重译结果" : "采用此版本"}
                </button>
              </div>
            </div>
          </div>
          {data.revisions.some((revision) => !revision.created_at) && (
            <div className="segment-edit-hint">
              旧项目记录仅包含实际保留下来的文本，记录顺序不代表未知的修改时间。
            </div>
          )}
        </section>
      )}
      {(stale || previewStale) && mode !== "read" && (
        <div className="segment-feedback" role="alert">
          当前译文或建议已过期，不能直接覆盖。
          <button
            className="btn btn-quiet sm"
            disabled={working}
            onClick={() =>
              void run(async () => {
                await onChanged();
                await load();
              })
            }
          >
            刷新当前版本（保留草稿）
          </button>
        </div>
      )}
      {error && (
        <div className="segment-feedback error" role="alert">
          {error}
          <button
            className="btn btn-quiet sm"
            disabled={working}
            onClick={() =>
              void run(async () => {
                await onChanged();
                await load();
              })
            }
          >
            刷新当前版本
          </button>
        </div>
      )}
      {message && (
        <div className="segment-feedback" role="status">
          {message}
        </div>
      )}
    </article>
  );
}
