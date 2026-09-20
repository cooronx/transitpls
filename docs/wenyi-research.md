# TransitPLS 与 Wenyi 的主要能力差异

调查日期：2026-09-20。Wenyi 默认分支固定到 [`5f206257ddc05b113b0ec224518e31cb5d7b62ad`](https://github.com/BigDawnGhost/wenyi/commit/5f206257ddc05b113b0ec224518e31cb5d7b62ad)（2026-09-17）；本地基准为 `035a10e94193fd0ac0d4d51e594a38a4183eee61` 及当前工作区。以下是源码调查，不是实际模型翻译质量评测。

结论：两者都有长篇翻译的基本上下文、术语和续跑能力。较大的差距集中在翻译完成后的审校与人工修订，以及模型调度、输入输出范围；不应把每个界面选项或上游路线图都算作缺失。

## 1. 真实 AI 审校闭环：当前最明确的缺口

- **Wenyi：核心实现且 Web 已接入。** 全书分块初审后，Agent 可查询术语、跨章命中、邻近原译文和梗概，确认或驳回问题；另有冲突仲裁、整段影子修订、盲复审和可选 Autofix 正式写回。审校记录、证据、运行历史与写回结果可以在界面查看。
- **TransitPLS：报告结构已有，真实审校尚未接入。** `src-tauri/src/review.rs:164` 仅在 mock 分支产生问题；`:213` 明确返回“非 mock review 客户端尚未接入”。所以缺的是可运行的审校流程，不能只用“增加一个审校按钮”描述。
- **影响：** 目前的重译、润色和术语裁定无法替代翻译后的系统性漏译、误译、上下文矛盾检查。
- **上游证据：** [审校轮次与 Fixer](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/pipeline/review_rounds.py)、[证据查询](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/review/evidence.py)、[发布服务](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/pipeline/review_autofix.py)、[Web 审校入口](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/apps/web/src/features/review/ReviewPage.tsx#L83)。

## 2. 人工校对与修订历史：缺少成品打磨工作台

- **Wenyi：Web 已接入。** 独立人工校阅页可以对照原文编辑译文、查看翻译/润色/人工修改等版本，并把历史版本作为当前译文使用。后端持久化段落修订记录。
- **TransitPLS：可查看原译文、重译和恢复上次译文，但不是完整校对工作流。** `src/App.tsx:1585` 的 `SegmentCard` 是只读展示；目前没有对应的正文人工编辑命令。`src-tauri/src/workflow/retranslate.rs:338` 只保存一个 `previous_target`，`src-tauri/src/ui/terms.rs:168` 用交换方式恢复，不能代表完整修订链。
- **影响：** 用户想直接修正一句话并保留修改依据，目前无法在应用内完成同等流程。
- **上游证据：** [人工校阅组件](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/apps/web/src/features/proofreading/ChapterProofreading.tsx)、[版本界面](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/apps/web/src/features/proofreading/RevisionHistory.tsx)、[人工写回接口](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/apps/api/wenyi_api/routers/review.py#L111)、[历史持久化](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/apps/api/wenyi_api/segment_history.py)。

## 3. 分阶段模型路由：缺少按工作分配模型和成本的能力

- **Wenyi：逐操作选模型已接入 UI。** 提供 strong/cheap/fast 三档默认配置，也可为风格分析、章摘要、翻译、润色、术语抽取、审校、取证、修复等操作独立选择具体模型或档位。后端另外支持 fallback 与连接共享限额；不能把这些后端能力都表述为已有完整图形编辑入口。
- **TransitPLS：已支持多种协议与模型配置，但实际工作流共用当前 LLM 配置。** 参见 `src-tauri/src/config.rs`、`src-tauri/src/workflow/transit.rs:67` 和 `src-tauri/src/workflow/polish.rs:46`。已有 OpenAI Chat、Responses、Anthropic 接入不等于有阶段路由。
- **影响：** 难以设置“便宜模型做摘要和术语、强模型翻译、另一模型独立审校”，成本与质量的调配粒度较粗。
- **上游证据：** [操作注册表](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/llm/operations.py#L72)、[路由配置](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/llm/configuration.py)、[UI 选择模型](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/apps/web/src/features/settings/ModelSelection.tsx)。

## 4. 文件格式与字幕：应用范围明显更窄

- **Wenyi：核心实现且 Web 暴露入口。** 输入 EPUB、FB2、TXT、Markdown、HTML、PDF、DOCX、SRT；输出 EPUB、TXT、HTML、Markdown、PDF、DOCX、SRT。PDF 输出受可用依赖限制；PDF 解析默认依赖 MinerU，也可配置外部 BabelDOC bridge，不能理解为无依赖的完美版式保留。
- **TransitPLS：目前只支持 TXT/EPUB。** `src-tauri/src/parser/mod.rs` 的格式分发和 `src-tauri/src/export/mod.rs` 的 `ExportFormat` 均只列这两种；`src/App.tsx:3262` 也明确说明不支持 PDF、DOCX、字幕。
- **影响：** Word/PDF 文档与字幕使用者必须先自行转换或换工具。SRT 不只是多一个扩展名：Wenyi 有独立字幕窗口翻译、时间轴编辑和续跑路径；该路径不含书籍术语、润色与全书审校。
- **上游证据：** [Web 格式能力](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/apps/api/wenyi_api/routers/configuration.py#L50)、[文档解析分发](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/ingest/segmenter.py)、[输出分发](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/assemble/writer.py)、[字幕流程](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/srt/translate.py#L198)。

## 5. 双语对照成品：已有原译文查看，但缺少对应导出

- **Wenyi：核心实现且 UI 已接入。** 导出可选单语/双语，双语支持原文或译文优先，并有相应样式处理。
- **TransitPLS：当前导出是译文成品。** `src-tauri/src/export/mod.rs`、`src-tauri/src/export/txt.rs` 以及 EPUB 导出路径没有同等双语选项。界面中可以查看原译文，不意味着能生成双语书。
- **影响：** 无法直接交付用于对照阅读、学习或人工审阅的双语 EPUB/TXT。
- **上游证据：** [双语导出 UI](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/apps/web/src/features/export/ExportPage.tsx)、[双语输出参数与分发](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/assemble/writer.py#L32)、[EPUB 双语回填](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/assemble/epub_resources.py#L153)。

## 6. 多目标语言：当前是中文翻译工具，上游面向多方向互译

- **Wenyi：代码和创建项目 UI 已接入，官方标注实验性。** 源/目标语言可分别选择；语言与语言对资源作用于提示词、术语、标题和元数据；目标语言状态隔离。支持中英日韩法德西意葡俄及部分变体，但实际长篇质量不能从“可选语言”推断。
- **TransitPLS：目标固定简体中文。** `src-tauri/src/config.rs:331` 拒绝非 `zh-CN`；解析元数据和导出命名也以中文目标为当前约定。
- **影响：** 中译英、英译日等任务不是更换一个模型即可运行；需打通配置、提示词、状态与输出约定。
- **上游证据：** [项目创建语言选择](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/apps/web/src/features/project-create/CreateProject.tsx)、[语言注册资源](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/i18n/data/languages/registry.json)、[实验性说明](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/docs/zh/README.md)。

## 架构方向差异，不宜直接当成必须补齐的功能

Wenyi 同时提供 CLI 和可部署 Web 服务，采用 FastAPI、PostgreSQL、Redis、独立工作流/导出 Worker；TransitPLS 是 Tauri/Rust 本地桌面工作流。前者适合服务器运行和浏览器访问，后者适合本地使用和较轻部署。除非产品要走远程服务方向，否则不必为了对齐而引入整套服务端架构。上游静态 token 也不等于已经拥有完整多租户、用户权限或多人协作系统。

来源：[部署及服务边界](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/docs/web.md)、[核心模块边界](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/docs/architecture.md)。

## 不应误列为缺失

- 本地已有风格分析、逐章摘要、全书梗概、邻近原文、串行近期译文上下文、术语抽取/冲突裁定、并行翻译、重译、润色、断点续跑与 EPUB 保资源回填。这些不是 Wenyi 独有的大能力。
- Wenyi 的向量语义检索和句子级翻译记忆仍明确列为未实现的路线图，不能据此认定本地缺少已成熟的 RAG/长程记忆系统。来源：[P06](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/docs/project-review/2026-09-05/p06-semantic-evidence-retrieval.md)、[P07](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/docs/project-review/2026-09-05/p07-translation-memory-consistency.md)。
- 未核实上游存在跨书自动共享术语资产库。其现有 Web 术语 API 按项目作用域存储，并支持 JSON/CSV 导入导出；手工搬运不等于跨项目共享机制。来源：[术语 API](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/apps/api/wenyi_api/routers/glossary.py)。
- 上游提示词是仓库内的任务/语言资源，未发现可视化自定义提示模板编辑器，不能把“源码中有模板”当作成熟的用户模板管理功能。来源：[提示资源](https://github.com/BigDawnGhost/wenyi/tree/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/i18n/data/tasks)。

建议先补真实审校和人工修订，再做分阶段模型分配与双语导出；DOCX/PDF/字幕和多目标语言按实际用户需求决定。这个顺序优先把现有小说翻译流程从“能生成译文”补到“能检查、修正并交付”。
