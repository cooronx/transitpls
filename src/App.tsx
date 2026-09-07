import { useEffect, useMemo, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { confirm as confirmDialog, open } from "@tauri-apps/plugin-dialog";
import { openPath } from "@tauri-apps/plugin-opener";
import {
  ArrowRight, BadgeCheck, BookOpen, Braces, Check, CheckCircle2, ChevronDown,
  ChevronUp, Circle, CircleDashed, Columns2, Download, FileText, FolderKanban,
  History, Languages, LibraryBig, ListTree, LoaderCircle, MessageSquare, PanelTop,
  Play, Plus, RotateCcw, Search, Settings, ShieldCheck, SquareStop,
  Trash2, X, type LucideIcon,
} from "lucide-react";
import "./App.css";

type Status = "initialized" | "translating" | "translated" | "failed";
type ItemStatus = "pending" | "translated" | "failed";
interface Project { id:string; title:string; source_file:string; source_path:string; source_language:string; target_language:string; status:Status; chapters_total:number; chapters_completed:number; updated_at:string; cover_data_url?:string|null }
interface Segment { id:string; ordinal:number; source:string; target:string|null; kind:string; status:ItemStatus }
interface Chapter { id:string; title:string; target_title?:string; status:ItemStatus; segments:Segment[] }
interface Term { source:string; target:string; type:string; aliases:string[]; first_chapter:number; note?:string; status:"ok"|"conflict"|"resolved" }
interface LogEntry { timestamp:string; event:string; details:unknown }
interface Config { language:{source:string;target:string}; llm:{provider:string;model:string;api_key_env:string;base_url?:string}; segment:{max_chars_per_segment:number;max_chars_per_batch:number}; pipeline:{polish:boolean;recent_context_chars:number} }
interface CredentialStatus { configured:boolean; source?:"environment"|"desktop"; lastFour?:string }
interface Bootstrap { config:Config; credential:CredentialStatus; configPath?:string; stateDir:string; projects:Project[] }
interface Candidate { source:string; normalized:string; occurrences:number; first_chapter:number; last_chapter:number; contexts:string[]; sources:string[]; surface_forms:string[]; category:string; variants:string[]; proposed_target:string; confidence:number|null; reason:string; status:"candidate"|"confirmed"|"rejected"|"variant" }
interface Detail { project:Project; chapters:Chapter[]; logs:LogEntry[]; terms:Term[]; candidates:Candidate[]; conflicts:Array<{source:string;target:string;chapter:number}>; report?:unknown }
type View = "workspace"|"projects"|"terms"|"settings"|"review"|"history";
type TrayName = "tasks"|"issues"|"logs";

const navItems:Array<{id:View;icon:LucideIcon;label:string}> = [
  {id:"workspace",icon:Languages,label:"工作台"},{id:"projects",icon:FolderKanban,label:"项目"},
  {id:"terms",icon:LibraryBig,label:"术语库"},
  {id:"review",icon:BadgeCheck,label:"审校"},{id:"history",icon:History,label:"历史"},
];
const statusText:Record<Status|ItemStatus,string> = {initialized:"待翻译",translating:"翻译中",translated:"已完成",failed:"失败",pending:"待处理"};
const termTypeText:Record<string,string> = {person:"人名",place:"地名",organization:"组织机构",term:"术语",appellation:"称谓",speech:"语言习惯",fixed_expr:"固定表达"};

export default function App() {
  const [bootstrap,setBootstrap] = useState<Bootstrap|null>(null);
  const [detail,setDetail] = useState<Detail|null>(null);
  const [chapterIndex,setChapterIndex] = useState(0);
  const [view,setView] = useState<View>("workspace");
  const [tray,setTray] = useState<TrayName>("tasks");
  const [busy,setBusy] = useState<string|null>(null);
  const [notice,setNotice] = useState<string|null>(null);
  const [search,setSearch] = useState("");
  const [mockClient,setMockClient] = useState(false);

  const reload = async (projectId?:string) => {
    const data = await invoke<Bootstrap>("ui_bootstrap");
    setBootstrap(data);
    const id = projectId ?? detail?.project.id ?? data.projects[0]?.id;
    if (!id) { setDetail(null); return; }
    const next = await invoke<Detail>("ui_project",{projectId:id});
    setDetail(next);
    setChapterIndex((current)=>Math.min(current,Math.max(0,next.chapters.length-1)));
  };
  useEffect(()=>{ reload().catch((error)=>{
    if(String(error).includes("invoke")) setBootstrap(browserPreview());
    else setNotice(String(error));
  }); },[]);
  useEffect(()=>{
    let disposed=false;
    let stop:undefined|(()=>void);
    listen<Detail>("translation-progress",({payload})=>{
      setDetail((current)=>current?.project.id===payload.project.id?payload:current);
      setBootstrap((current)=>current?{...current,projects:current.projects.map((project)=>project.id===payload.project.id?{...payload.project,cover_data_url:project.cover_data_url}:project)}:current);
    }).then((unlisten)=>{if(disposed)unlisten();else stop=unlisten}).catch((error)=>{
      if(!String(error).includes("invoke"))setNotice(String(error));
    });
    return()=>{disposed=true;stop?.()};
  },[]);

  const run = async (label:string,action:()=>Promise<Detail|void>) => {
    setBusy(label); setNotice(null);
    try { const result=await action(); if(result){setDetail(result);await reload(result.project.id)} setNotice(`${label}已完成`); }
    catch(error){setNotice(String(error))} finally{setBusy(null)}
  };
  const importFile = async () => {
    if(!bootstrap?.credential.configured&&!mockClient){setView("settings");setNotice("请先在设置中配置并验证 API Key");return}
    const path=await open({multiple:false,filters:[{name:"电子书",extensions:["epub","txt"]}]});
    if(path) await run("项目初始化",async()=>{
      try{return await invoke<Detail>("ui_initialize",{input:path,mockClient})}
      catch(error){
        try{await reload()}catch(reloadError){throw new Error(`${String(error)}；刷新项目失败：${String(reloadError)}`)}
        throw error;
      }
    });
  };
  const selectProject = async (id:string) => {
    setBusy("加载项目");
    try { setDetail(await invoke<Detail>("ui_project",{projectId:id}));setChapterIndex(0);setView("workspace"); }
    catch(error){setNotice(String(error))} finally{setBusy(null)}
  };
  const deleteProject = async (project:Project) => {
    if(busy)return;
    const confirmed=await confirmDialog(`确定删除项目“${project.title}”吗？\n\n翻译进度、术语和日志将被永久删除，原始书籍文件不会受到影响。`,{title:"删除项目",kind:"warning",okLabel:"删除",cancelLabel:"取消"});
    if(!confirmed)return;
    setBusy("删除项目");setNotice(null);
    try{
      const wasCurrent=detail?.project.id===project.id;
      const next=await invoke<Bootstrap>("ui_delete_project",{projectId:project.id});
      setBootstrap(next);
      if(wasCurrent){
        setDetail(null);
        const replacement=next.projects[0];
        if(replacement)setDetail(await invoke<Detail>("ui_project",{projectId:replacement.id}));
        else setDetail(null);
        setChapterIndex(0);
      }
      setNotice(`项目“${project.title}”已删除，原始文件未改动`);
    }catch(error){setNotice(String(error))}finally{setBusy(null)}
  };
  const translate = (chapter?:number) => {
    if(busy)return;
    if(!bootstrap?.credential.configured&&!mockClient){setView("settings");setNotice("请先在设置中配置并验证 API Key");return}
    if(detail) void run("翻译",()=>invoke<Detail>("ui_transit",{projectId:detail.project.id,chapter:chapter??null,mockClient}));
  };
  const saveModel = async (value:Config,apiKey:string) => {
    setBusy("验证模型");setNotice(null);
    try{const next=await invoke<Bootstrap>("ui_verify_and_save_model",{value,apiKey:apiKey||null});setBootstrap(next);setNotice("连接验证成功，模型设置已保存")}
    catch(error){setNotice(String(error))}finally{setBusy(null)}
  };
  const exportBook = async (format:"txt"|"epub") => {
    if(!detail)return; setBusy("导出");
    try { const output=await invoke<string>("ui_export",{projectId:detail.project.id,format});setNotice(`已导出至 ${output}`);await openPath(output); }
    catch(error){setNotice(String(error))} finally{setBusy(null)}
  };
  const cancelTask = async () => {
    const taskId=busy==="项目初始化"?"initialize":detail?.project.id;
    if(!taskId)return;
    const cancelled=await invoke<boolean>("ui_cancel_task",{taskId});
    if(cancelled)setNotice("正在取消任务，已完成的进度会保留");
  };
  const chapter=detail?.chapters[chapterIndex];
  const segments=useMemo(()=>{if(!chapter)return[];const q=search.trim().toLowerCase();return q?chapter.segments.filter((s)=>s.source.toLowerCase().includes(q)||s.target?.toLowerCase().includes(q)):chapter.segments},[chapter,search]);

  return <div className="app-shell">
    <Header project={detail?.project} projects={bootstrap?.projects??[]} model={bootstrap?.config.llm.model??"—"} search={search} onSearch={setSearch} onProject={(id)=>void selectProject(id)} onTranslate={()=>translate()} busy={busy} onCancel={()=>void cancelTask()} disabled={!detail}/>
    <div className="workspace-row">
      <ActivityBar view={view} onChange={setView}/>
      {view==="workspace"&&<Explorer projects={bootstrap?.projects??[]} detail={detail} chapterIndex={chapterIndex} onChapter={setChapterIndex} onProject={selectProject} onImport={importFile}/>}
      <main className="main-panel">
        {view==="workspace"&&detail&&chapter&&<Workspace detail={detail} chapter={chapter} chapterIndex={chapterIndex} segments={segments} tray={tray} setTray={setTray} translating={busy==="翻译"} onTranslate={()=>translate(chapterIndex)} onExport={exportBook}/>}
        {view==="projects"&&<ProjectGallery projects={bootstrap?.projects??[]} busy={Boolean(busy)} onSelect={selectProject} onDelete={deleteProject} onImport={importFile}/>}
        {view==="history"&&<ProjectGallery projects={bootstrap?.projects??[]} busy={Boolean(busy)} onSelect={selectProject} onImport={importFile}/>}
        {view==="terms"&&<TermsView detail={detail} onReload={()=>reload()}/>}
        {view==="settings"&&<SettingsView config={bootstrap?.config} credential={bootstrap?.credential} configPath={bootstrap?.configPath} busy={busy==="验证模型"} onSave={saveModel}/>}
        {view==="review"&&<ReviewPlaceholder/>}
        {view==="workspace"&&(!detail||!chapter)&&<EmptyState onImport={importFile}/>}
      </main>
      {view==="workspace"&&detail&&<Inspector config={bootstrap?.config} detail={detail} mock={mockClient} onMock={setMockClient}/>}
    </div>
    <footer className="statusbar"><span>TransItPls v0.1.0</span><i/><span>Tauri · 跨平台</span><span className="status-spacer"/><span className="icon-label">{busy&&<LoaderCircle className="spin"/>}{busy?`${busy}进行中…`:"就绪"}</span><i/><span>{bootstrap?.projects.length??0} 个项目</span></footer>
    {notice&&<button className="toast" onClick={()=>setNotice(null)}>{notice}<X aria-hidden="true"/></button>}{busy&&<div className="busy-line"/>}
  </div>;
}

function Logo(){return <div className="logo-mark"><span>文</span><b>A</b></div>}
function Header({project,projects,model,search,onSearch,onProject,onTranslate,busy,onCancel,disabled}:{project?:Project;projects:Project[];model:string;search:string;onSearch:(v:string)=>void;onProject:(id:string)=>void;onTranslate:()=>void;busy:string|null;onCancel:()=>void;disabled:boolean}){
  const[menuOpen,setMenuOpen]=useState(false);
  const cancellable=busy==="翻译"||busy==="项目初始化";
  const translateLabel=busy==="翻译"?"正在翻译":busy==="项目初始化"?"正在初始化":"开始翻译";
  return <header className="topbar"><div className="brand"><Logo/><strong>TransItPls</strong><i/><span>项目：</span><div className="project-switcher"><button className="project-trigger" aria-expanded={menuOpen} disabled={!project||Boolean(busy)} onClick={()=>setMenuOpen(!menuOpen)}><b>{project?.title??"未选择项目"}</b>{menuOpen?<ChevronUp/>:<ChevronDown/>}</button>{menuOpen&&<><button className="menu-backdrop" aria-label="关闭项目菜单" onClick={()=>setMenuOpen(false)}/><div className="project-menu"><header><b>切换项目</b><small>{projects.length} 个项目</small></header><div>{projects.map((item)=><button className={`project-option ${item.id===project?.id?"active":""}`} key={item.id} onClick={()=>{setMenuOpen(false);if(item.id!==project?.id)onProject(item.id)}}><ProjectCover project={item} className="mini-cover"/><span><b>{item.title}</b><small>{item.chapters_completed} / {item.chapters_total} 章 · {statusText[item.status]}</small></span>{item.id===project?.id&&<em>当前</em>}</button>)}</div></div></>}</div></div><label className="global-search"><Search/><input value={search} onChange={(e)=>onSearch(e.target.value)} placeholder="搜索原文或译文…"/><kbd>Ctrl K</kbd></label><div className="top-actions"><span>模型: <b>{model}</b></span><span className="local-state"><i/>本地状态</span><button className="primary icon-label" disabled={disabled||Boolean(busy)} onClick={onTranslate}>{busy?<LoaderCircle className="spin"/>:<Play/>}{translateLabel}</button>{cancellable&&<button className="cancel icon-label" onClick={onCancel}><SquareStop/>取消任务</button>}</div></header>
}
function ActivityBar({view,onChange}:{view:View;onChange:(v:View)=>void}){return <nav className="activity-bar"><div>{navItems.map((item)=>{const Icon=item.icon;return <button key={item.id} className={view===item.id?"active":""} onClick={()=>onChange(item.id)} title={item.label}><Icon/><span>{item.label}</span></button>})}</div><button className={view==="settings"?"active":""} onClick={()=>onChange("settings")} title="设置"><Settings/><span>设置</span></button></nav>}
function Explorer({projects,detail,chapterIndex,onChapter,onProject,onImport}:{projects:Project[];detail:Detail|null;chapterIndex:number;onChapter:(i:number)=>void;onProject:(id:string)=>void;onImport:()=>void}){
  const project=detail?.project, progress=project?Math.round(project.chapters_completed/Math.max(1,project.chapters_total)*100):0;
  return <aside className="explorer"><div className="panel-title"><strong>项目文件</strong><button className="icon-label" onClick={onImport}><Plus/>导入文件</button></div>{project?<><div className="project-card"><div><span className="book-icon"><BookOpen/></span><p><b>{project.title}</b><small>{project.chapters_total} 个章节</small></p></div><div className="progress"><i style={{width:`${progress}%`}}/></div><footer><span>{project.chapters_completed} / {project.chapters_total} 章</span><b>{progress}%</b></footer></div><div className="file-row"><ChevronDown/><BookOpen/><strong>{fileName(project.source_file)}</strong></div><div className="chapter-list">{detail?.chapters.map((chapter,index)=><button key={chapter.id} className={index===chapterIndex?"active":""} onClick={()=>onChapter(index)}><FileText/><b>{chapter.target_title||chapter.title}</b><em className={chapter.status}>{statusText[chapter.status]}</em></button>)}</div></>:<div className="explorer-empty">尚无项目</div>}{projects.length>1&&<div className="other-projects"><h4>其他项目</h4>{projects.filter((p)=>p.id!==project?.id).map((p)=><button key={p.id} onClick={()=>onProject(p.id)}><FolderKanban/><span>{p.title}</span></button>)}</div>}</aside>
}
function Workspace({detail,chapter,chapterIndex,segments,tray,setTray,translating,onTranslate,onExport}:{detail:Detail;chapter:Chapter;chapterIndex:number;segments:Segment[];tray:TrayName;setTray:(t:TrayName)=>void;translating:boolean;onTranslate:()=>void;onExport:(f:"txt"|"epub")=>void}){
  return <div className="editor-layout"><div className="editor-tabs"><button className="active icon-label"><Columns2/>对照翻译</button><button className="icon-label"><ListTree/>结构预览</button><button className="icon-label" onClick={()=>setTray("issues")}><ShieldCheck/>质量检查</button><span/><small>第 {chapterIndex+1} / {detail.chapters.length} 章</small><button className="run-chapter icon-label" disabled={translating} onClick={onTranslate}>{translating?<LoaderCircle className="spin"/>:<Play/>}{translating?"正在翻译":"翻译本章"}</button></div><div className="breadcrumb"><FileText/>{fileName(detail.project.source_file)}<i>/</i><b>{chapter.target_title||chapter.title}</b><em>{chapter.segments.length} 个段落</em></div><div className="column-head"><div><b>原文</b><span>{languageName(detail.project.source_language)}</span></div><div><b>译文</b><span>{languageName(detail.project.target_language)}</span></div></div><section className="segments">{segments.length?segments.map((s)=><SegmentCard key={s.id} segment={s}/>):<div className="no-results">没有匹配的段落</div>}</section><Tray detail={detail} tray={tray} setTray={setTray} onExport={onExport}/></div>
}
function SegmentCard({segment}:{segment:Segment}){const words=segment.source.trim().split(/\s+/).filter(Boolean).length;return <article className={`segment-card ${segment.status}`}><div className="segment-source"><header><b>#{segment.ordinal+1}</b><span>{words} 词</span></header><p>{segment.source}</p><footer><PanelTop/>{segment.kind==="heading"?"标题":"源段落"}</footer></div><div className="segment-target"><header><span>{segment.target?.length??0} 字</span><b className={segment.status}>{statusText[segment.status]}</b></header>{segment.target?<p>{segment.target}</p>:<div className="target-empty"><span>等待翻译</span><small>运行本章翻译后将在此显示译文</small></div>}<footer><button disabled className="icon-label"><RotateCcw/>重译</button><button disabled className="icon-label"><Check/>采纳</button><button disabled className="icon-label"><MessageSquare/>注释</button></footer></div></article>}
function Tray({detail,tray,setTray,onExport}:{detail:Detail;tray:TrayName;setTray:(t:TrayName)=>void;onExport:(f:"txt"|"epub")=>void}){return <section className="tray"><header><div><button className={tray==="tasks"?"active":""} onClick={()=>setTray("tasks")}>章节任务 <b>{detail.chapters.length}</b></button><button className={tray==="issues"?"active":""} onClick={()=>setTray("issues")}>问题列表 <b>{detail.conflicts.length}</b></button><button className={tray==="logs"?"active":""} onClick={()=>setTray("logs")}>运行日志</button></div><div className="export-menu"><button className="icon-label" onClick={()=>onExport("txt")}><Download/>TXT</button><button className="icon-label" onClick={()=>onExport("epub")}><Download/>EPUB</button></div></header><div className="tray-content">{tray==="tasks"&&<TaskTable detail={detail}/>} {tray==="issues"&&<IssueList detail={detail}/>} {tray==="logs"&&<LogList logs={detail.logs}/>}</div></section>}
function TaskTable({detail}:{detail:Detail}){return <table><thead><tr><th>#</th><th>章节</th><th>状态</th><th>进度</th><th>段落</th></tr></thead><tbody>{detail.chapters.map((chapter,index)=>{const done=chapter.segments.filter((s)=>s.status==="translated").length,progress=Math.round(done/Math.max(1,chapter.segments.length)*100);return <tr key={chapter.id}><td>{index+1}</td><td>{chapter.target_title||chapter.title}</td><td><em className={chapter.status}>{statusText[chapter.status]}</em></td><td><div className="table-progress"><i style={{width:`${progress}%`}}/><span>{progress}%</span></div></td><td>{done} / {chapter.segments.length}</td></tr>})}</tbody></table>}
function IssueList({detail}:{detail:Detail}){return detail.conflicts.length?<div className="issue-list">{detail.conflicts.map((item,index)=><div key={`${item.source}-${index}`}><b>术语冲突</b><span>{item.source} → {item.target}</span><em>第 {item.chapter+1} 章</em></div>)}</div>:<div className="panel-empty">当前没有术语冲突</div>}
function LogList({logs}:{logs:LogEntry[]}){return logs.length?<div className="log-list">{logs.map((log,index)=><div key={`${log.timestamp}-${index}`}><time>{formatDate(log.timestamp)}</time><b>{eventText(log.event)}</b><code>{JSON.stringify(log.details)}</code></div>)}</div>:<div className="panel-empty">暂无运行日志</div>}

function Inspector({config,detail,mock,onMock}:{config?:Config;detail:Detail;mock:boolean;onMock:(v:boolean)=>void}){const progress=Math.round(detail.project.chapters_completed/Math.max(1,detail.project.chapters_total)*100);return <aside className="inspector"><div className="inspector-tabs"><button className="active">任务配置</button><button>术语与记忆</button></div><div className="inspector-body"><Field label="语言方向"><div className="direction"><span>{languageName(detail.project.source_language)}</span><ArrowRight/><span>{languageName(detail.project.target_language)}</span></div></Field><Field label="模型选择"><div className="select-like">{config?.llm.model??"—"}<ChevronDown/></div><small>提供商：{config?.llm.provider??"—"}</small></Field><Field label="分段策略"><div className="select-like">每段最多 {config?.segment.max_chars_per_segment??0} 字符</div></Field><label className="switch-row"><span>译后润色</span><i className={config?.pipeline.polish?"on":""}/></label><label className="switch-row"><span>离线模拟模式</span><button className={`toggle ${mock?"on":""}`} onClick={()=>onMock(!mock)}><i/></button></label><hr/><section className="flow"><header><b>项目进度</b><strong>{progress}%</strong></header><div className="big-progress"><i style={{width:`${progress}%`}}/></div><p className="done"><CheckCircle2/>已完成 {detail.project.chapters_completed} 章</p><p className={detail.project.status==="failed"?"error":"active"}><CircleDashed/>{statusText[detail.project.status]}</p><p><Circle/>生成校对报告 <em>阶段 8 待实现</em></p></section></div></aside>}
function Field({label,children}:{label:string;children:ReactNode}){return <label className="field"><b>{label}</b>{children}</label>}
function ProjectGallery({projects,busy,onSelect,onDelete,onImport}:{projects:Project[];busy:boolean;onSelect:(id:string)=>void;onDelete?:(project:Project)=>void;onImport:()=>void}){return <div className="page-view"><header><div><h1>翻译项目</h1><p>管理本机状态目录中的所有书籍。</p></div><button className="primary icon-label" onClick={onImport}><Plus/>新建项目</button></header>{projects.length?<div className="project-grid">{projects.map((project)=>{const progress=Math.round(project.chapters_completed/Math.max(1,project.chapters_total)*100);return <article key={project.id}><button className="project-open" onClick={()=>onSelect(project.id)}><ProjectCover project={project} className="cover"/><section className={onDelete?"with-actions":""}><h3>{project.title}</h3><p>{fileName(project.source_file)}</p><div className="progress"><i style={{width:`${progress}%`}}/></div><footer><span>{project.chapters_completed} / {project.chapters_total} 章</span><em className={project.status}>{statusText[project.status]}</em></footer></section></button>{onDelete&&<button className="project-card-delete icon-label" disabled={busy} onClick={()=>onDelete(project)}><Trash2/>删除</button>}</article>})}</div>:<EmptyState onImport={onImport}/>}</div>}
function ProjectCover({project,className}:{project:Project;className:string}){const[failed,setFailed]=useState(false);return <span className={className}>{project.cover_data_url&&!failed?<img src={project.cover_data_url} alt="" onError={()=>setFailed(true)}/>:"文"}</span>}
function TermsView({detail,onReload}:{detail:Detail|null;onReload:()=>Promise<void>}) {
  const [editing,setEditing]=useState<string|null>(null),[target,setTarget]=useState("");
  const [error,setError]=useState(""),[busy,setBusy]=useState(false);
  const run=async(action:()=>Promise<unknown>)=>{setBusy(true);setError("");try{await action();setEditing(null);await onReload()}catch(e){setError(String(e))}finally{setBusy(false)}};
  return <div className="page-view terms-view"><header><div><h1>术语库</h1><p>{detail?`${detail.project.title} · ${detail.terms.length} 条术语`:"选择项目后查看术语"}</p></div>{detail&&<button className="term-scan" disabled={busy} onClick={()=>void run(()=>invoke("ui_scan_terms",{projectId:detail.project.id}))}><Search size={16}/>扫描全文</button>}</header>
    {error&&<p role="alert">{error}</p>}
    {detail&&detail.terms.length>0&&<div className="term-table"><div className="term-row term-head"><span>原文</span><span>固定译名</span><span>类型</span><span>首次出现</span><span>状态</span><span>操作</span></div>{detail.terms.map(term=><div className="term-row" key={term.source}><b>{term.source}</b>{editing===term.source?<input aria-label="固定译名" value={target} onChange={e=>setTarget(e.target.value)}/>:<span>{term.target}</span>}<span>{termTypeText[term.type]??"其他"}</span><span>第 {term.first_chapter+1} 章</span><em>{term.status==="resolved"?"已确认":term.status==="conflict"?"有冲突":"待确认"}</em>{editing===term.source?<button disabled={busy||!target.trim()} onClick={()=>void run(()=>invoke("ui_resolve_term",{projectId:detail.project.id,source:term.source,target}))}>确认</button>:<button disabled={busy} onClick={()=>{setEditing(term.source);setTarget(term.target)}}>修改</button>}</div>)}</div>}
    {detail&&<CandidateReview key={detail.project.id} detail={detail} onReload={onReload}/>}
  </div>;
}

function CandidateReview({detail,onReload}:{detail:Detail;onReload:()=>Promise<void>}) {
  const [query,setQuery]=useState(""),[status,setStatus]=useState("candidate"),[page,setPage]=useState(0);
  const candidates=(detail.candidates??[]).filter(c=>(!status||c.status===status)&&`${c.source} ${c.proposed_target} ${c.sources.join(" ")}`.toLowerCase().includes(query.toLowerCase()));
  const lastPage=Math.max(0,Math.ceil(candidates.length/30)-1),currentPage=Math.min(page,lastPage);
  return <section className="candidate-review"><header><h2>全文候选 · {candidates.length}</h2><input aria-label="搜索候选" placeholder="搜索候选" value={query} onChange={e=>{setQuery(e.target.value);setPage(0)}}/><select aria-label="候选状态" value={status} onChange={e=>{setStatus(e.target.value);setPage(0)}}><option value="candidate">待确认</option><option value="confirmed">已确认</option><option value="rejected">已拒绝</option><option value="variant">变体</option><option value="">全部</option></select></header>
    {candidates.slice(currentPage*30,currentPage*30+30).map(c=><CandidateRow key={JSON.stringify([c.normalized,c.proposed_target,c.sources.includes("translation-drift")])} candidate={c} detail={detail} onReload={onReload}/>)}
    {!candidates.length&&<p>暂无候选</p>}
    <footer><button aria-label="上一页" title="上一页" disabled={currentPage===0} onClick={()=>setPage(currentPage-1)}><ArrowRight className="previous-page" size={16}/></button><span>{currentPage+1} / {lastPage+1}</span><button aria-label="下一页" title="下一页" disabled={currentPage===lastPage} onClick={()=>setPage(currentPage+1)}><ArrowRight size={16}/></button></footer>
  </section>;
}

function CandidateRow({candidate:c,detail,onReload}:{candidate:Candidate;detail:Detail;onReload:()=>Promise<void>}) {
  const [target,setTarget]=useState(c.proposed_target),[canonical,setCanonical]=useState("");
  const [busy,setBusy]=useState(false),[error,setError]=useState("");
  const review=async(status:Candidate["status"])=>{setBusy(true);setError("");try{await invoke("ui_review_candidate",{projectId:detail.project.id,normalized:c.normalized,proposedTarget:c.proposed_target,status,target:status==="variant"?canonical:target,drift:c.sources.includes("translation-drift")});await onReload()}catch(e){setError(String(e))}finally{setBusy(false)}};
  return <article className="candidate-row"><div><strong>{c.source}</strong><span>{termTypeText[c.category]??c.category} · {c.occurrences} 次 · 第 {c.first_chapter+1}–{c.last_chapter+1} 章</span><small>{c.sources.join(", ")} · {c.status}{c.confidence!==null?` · ${Math.round(c.confidence*100)}%`:""}</small></div>
    <fieldset className="candidate-actions" disabled={busy||c.status!=="candidate"}><input aria-label={`${c.source} 的译名`} placeholder="译名" value={target} onChange={e=>setTarget(e.target.value)}/><button title="确认译名" aria-label="确认译名" disabled={!target.trim()} onClick={()=>void review("confirmed")}><Check size={16}/></button><button title="拒绝候选" aria-label="拒绝候选" onClick={()=>void review("rejected")}><X size={16}/></button><select aria-label="归入已确认术语" value={canonical} onChange={e=>setCanonical(e.target.value)}><option value="">归入术语变体</option>{detail.terms.filter(t=>t.status==="resolved"&&t.source!==c.source).map(t=><option key={t.source} value={t.source}>{t.source}</option>)}</select><button title="设为变体" aria-label="设为变体" disabled={!canonical} onClick={()=>void review("variant")}><ArrowRight size={16}/></button></fieldset>
    <details><summary>证据与表面形式</summary><p>{c.surface_forms.join(" / ")}</p><p>{c.reason}</p>{c.contexts.map((context,i)=><blockquote key={i}>{context}</blockquote>)}</details>{error&&<p role="alert">{error}</p>}
  </article>;
}
function SettingsView({config,credential,configPath,busy,onSave}:{config?:Config;credential?:CredentialStatus;configPath?:string;busy:boolean;onSave:(value:Config,key:string)=>Promise<void>}){const[draft,setDraft]=useState(config),[apiKey,setApiKey]=useState(""),[showKey,setShowKey]=useState(false);useEffect(()=>setDraft(config),[config]);if(!draft)return <div className="page-empty">正在读取设置…</div>;const field=(key:keyof Config["llm"],value:string)=>setDraft({...draft,llm:{...draft.llm,[key]:value}}),sameProvider=draft.llm.provider===config?.llm.provider,configured=Boolean(sameProvider&&credential?.configured);return <div className="page-view model-page"><header><div><h1>设置</h1><p>配置翻译模型与连接凭据。</p></div></header><div className="settings-layout"><nav><button className="active icon-label"><Braces/>模型与 API</button><button className="icon-label" disabled><PanelTop/>通用</button><button className="icon-label" disabled><Settings/>高级</button></nav><div className="settings-card"><div className="settings-heading"><div><h2>模型连接</h2><p>选择模型提供商，并验证用于翻译的 API Key。</p></div>{configured&&<span className="credential-ok icon-label"><Check/>已配置 ····{credential?.lastFour}</span>}</div><Field label="提供商"><select value={draft.llm.provider} onChange={(e)=>{field("provider",e.target.value);setApiKey("")}}><option value="openai-chat">OpenAI Chat Completions</option><option value="openai-responses">OpenAI Responses</option><option value="anthropic">Anthropic</option></select></Field><Field label="模型"><input value={draft.llm.model} onChange={(e)=>field("model",e.target.value)}/></Field><Field label="接口地址"><input value={draft.llm.base_url??""} onChange={(e)=>field("base_url",e.target.value)}/></Field><Field label="API Key"><div className="secret-input"><input autoFocus={!credential?.configured} type={showKey?"text":"password"} value={apiKey} onChange={(e)=>setApiKey(e.target.value)} placeholder={configured?`已保存 ····${credential?.lastFour}，留空则不更改`:"粘贴 API Key"}/><button type="button" onClick={()=>setShowKey(!showKey)}>{showKey?"隐藏":"显示"}</button></div><small>Key 仅保存在当前用户的 TransItPls 配置目录中，不会写入书籍项目。</small></Field>{credential?.source==="environment"&&sameProvider&&<div className="config-note">当前优先使用环境变量 {draft.llm.api_key_env}；桌面端保存的 Key 将作为后备。</div>}<div className="settings-actions"><button className="primary" disabled={busy||(!configured&&!apiKey.trim())} onClick={()=>void onSave(draft,apiKey)}>{busy?"正在验证…":"验证并保存"}</button></div><footer>模型配置：<code>{configPath??"保存后创建 transitpls.toml"}</code></footer></div></div></div>}
function ReviewPlaceholder(){return <div className="review-placeholder"><div className="review-icon"><BadgeCheck/></div><h1>审校工作区</h1><p>界面已经就位。按照当前开发安排，阶段 8 的 Review 与报告能力暂不接入。</p><button disabled>运行只读审校</button><small>不会对译文执行自动写回</small></div>}
function EmptyState({onImport}:{onImport:()=>void}){return <div className="empty-state"><Logo/><h1>开始第一个翻译项目</h1><p>导入 EPUB 或 TXT 文件，TransItPls 会沿用 CLI 的项目状态与断点续跑能力。</p><button className="primary icon-label" onClick={onImport}><Plus/>选择书籍文件</button><small>暂不支持 PDF、DOCX 和字幕文件</small></div>}
function fileName(path:string){return path.split(/[\\/]/).pop()??path}
function languageName(code:string){return ({auto:"自动检测",en:"English","zh-CN":"中文（简体）",ja:"日本語"} as Record<string,string>)[code]??code}
function formatDate(value:string){try{return new Intl.DateTimeFormat("zh-CN",{month:"2-digit",day:"2-digit",hour:"2-digit",minute:"2-digit",second:"2-digit"}).format(new Date(value))}catch{return value}}
function eventText(event:string){return ({initialized:"项目已创建",analysis_completed:"译前分析完成",transit_started:"开始翻译",transit_completed:"翻译完成",term_resolved:"术语已裁定",exported:"成品已导出",failed:"任务失败"} as Record<string,string>)[event]??event}
function browserPreview():Bootstrap{return {stateDir:"projects",projects:[],credential:{configured:false},config:{language:{source:"auto",target:"zh-CN"},llm:{provider:"openai-chat",model:"gpt-4o-mini",api_key_env:"OPENAI_API_KEY",base_url:"https://api.openai.com/v1"},segment:{max_chars_per_segment:1200,max_chars_per_batch:1800},pipeline:{polish:false,recent_context_chars:2000}}}}
