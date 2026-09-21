# Wenyi AI 审校源码调研与 TransitPLS 设计

调研日期：2026-09-21。上游：<https://github.com/BigDawnGhost/wenyi>；核对默认分支提交 `5f206257ddc05b113b0ec224518e31cb5d7b62ad`，提交时间 2026-09-17 23:32:46 +08:00。以下上游结论来自该提交的实际源码和提示词；本次没有调用付费模型，也没有实测文学翻译质量。流程存在不等于质量提升已被验证。

## 一、Wenyi 实际怎么做

它实现的是“连续块初筛 → 按需证据复核 → 跨块冲突仲裁 → 影子译文修订 → 全书盲审”的有界流程。审校引擎产出问题和候选修改；另一个可配置的 Autofix 阶段负责写入正式译文。**不能据此理解成每项自动写入的修改都已经通过完整盲审。**

### 1. 入口和真实默认行为

- CLI `review` 命令允许 `--autofix/--no-autofix` 覆盖配置；Web `POST /projects/{pid}/review/run` 排队执行同一核心流程。全书审校要求所有章节翻译完成。[CLI](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/cli/wenyi_cli/commands/workflows.py#L333)、[API](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/apps/api/wenyi_api/routers/review.py#L49)
- Core 默认开启证据复核、冲突仲裁、影子修订和 Autofix；并发块数 4，证据最多 2 轮，影子修订最多 2 轮，要求连续 2 轮未发现问题。Web 的“标准翻译”同样开启审校和自动修复；“快速出稿”关闭这两项，不应把快速预设的 false 误认为通用默认。[Core 配置](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/config.py#L112)、[Web 预设](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/apps/api/wenyi_api/strategies.py#L72)
- 默认操作路由：初筛/盲审 `review.scan` 用 cheap 档；复核、仲裁、修订分别用 strong 档。它们是可配置档位，不保证实际调用不同模型。[路由](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/llm/operations.py#L114)

### 2. 初筛：对照原译文，找明确错误

按章将连续段落装成块，预算是 `max_tokens_per_batch × 3`，默认约 **5,400 个原文 token**；预算只计原文，不是整个请求的 token 总量。并行块完成后恢复原顺序。[分块](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/pipeline/review_chunks.py#L40)、[计数](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/pipeline/review_chunks.py#L479)

初筛直接传入的是编号原译文对和术语表；不会每次直接附加整本书或全书梗概。错误只分五类：漏译 `missing`、增译 `added`、误译 `mistranslation`、术语 `terminology`、人称/性别代词 `pronoun`。提示词允许自然意译和重排，要求忽略不确定问题和纯风格差异。[请求组装](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/agents/reviewer.py#L44)、[初筛提示词](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/i18n/data/tasks/reviewer_system.txt#L1)

返回 JSON 包含 `issues[{index,type,detail,suggestion}]`，结尾必须有 `reviewed_segments` 和 `complete:true`。后端检查段落数量、下标范围、类型和非空字段。格式错误先将多段块二分，单段才进行有限重试，默认额外 2 次。**完整性字段只能防截断/协议错误，不能证明模型确实审清每段。**[验证](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/agents/reviewer.py#L106)、[恢复](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/pipeline/review_chunks.py#L335)

### 3. 证据复核：有候选才查上下文

初筛有问题且相关开关开启时才进入证据 agent。它拿到本块原译文、稳定段落引用和候选问题，可直接判断，也可以查询下面四类只读证据；**初筛漏掉的问题不会自动触发此步骤。**复核可以在当前块补报问题，但不能把别章问题混进当前块。[触发条件](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/pipeline/review_chunks.py#L261)、[复核提示词](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/i18n/data/tasks/review_agent_system.txt#L1)

| 工具 | 实际内容与限制 |
| --- | --- |
| `glossary_term` | 单个术语的译名、读音、性别、别名、注释；不返回整张表 |
| `term_occurrences` | 全书原文的术语/别名或字面表达匹配；默认首、中、末，最多指定 8 次出现，邻近范围最多各 2 段 |
| `segment_context` | 指定段落前后原译文，前后各最多 6 段，允许跨章 |
| `book_context` | 文风、全书梗概或章节摘要，单项最多 6,000 字符 |

这是内存证据索引和确定性匹配，不是向量语义检索，也不是联网搜索。证据段落的原文和译文各截到 4,000 字符；影子修改会标注 `target_origin` 并保留正式基线译文，防止把刚改的文本当成独立证据。[索引实现](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/review/evidence.py#L124)、[段落证据](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/review/models.py#L120)

Agent 使用普通 messages + JSON 协议输出 `request_evidence` 或 `final`，本地程序执行查询并把 JSON 结果追加回对话；无需 provider 原生 tool calling。每轮最多 4 个查询、最多 2 轮，限制重复请求和返回体大小。每个候选必须且只能有一个 confirmed/dismissed 决定；引用必须属于程序实际提供的证据。协议/API 失败则保留初筛候选并标记降级，不能假装“无问题”。但**引用合法只证明出处存在，不证明模型结论在语义上正确**。[动作循环](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/agents/review_actions.py#L22)、[候选决策与降级](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/agents/review_loop.py#L140)

### 4. 仲裁：解决不同块给出的相互矛盾建议

复核结果可以标注需要全书一致的主体和类型（term/pronoun/fixed）。系统按术语/别名规范化，同一主体在至少两个块被建议成不同值才形成冲突组。仲裁 agent 查证据后只能选已有候选值或返回 unresolved；不是重新扫描所有人物关系。未解决冲突和复核降级问题会被跳过影子自动修订。[分组](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/review/conflicts.py#L11)、[仲裁提示词](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/i18n/data/tasks/review_arbiter_system.txt#L1)、[修订跳过规则](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/pipeline/review_rounds.py#L243)

### 5. 修订：每段一份候选，然后盲审影子译文

同一段的所有问题合并交给 Fixer。它看到原文、当前译文、确认的问题、文风、全书梗概、章节摘要、相关术语和前后各 4 段原译文。要求只做必要改动，保持人物口吻和未受影响措辞，输出单段完整替代文本。[上下文](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/pipeline/review_rounds.py#L275)、[修订提示词](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/i18n/data/tasks/review_fixer_system.txt#L1)

候选带 `segment_ref`、`before_hash`、`issue_ids`；后端要求全部匹配，拒绝空文本、原样返回和丢失必要对话引号。通过的是协议校验，依旧不是语义正确性证明。[Fixer 校验](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/agents/review_fixer.py#L283)

修改暂存在 `target_overrides`，正式章节不变。下一轮重新扫描全书影子译文，不把上一轮问题描述给初筛。默认最多 2 次修订、连续 2 次干净确认；全书扫描理论上最多 `(2+1)×2=6` 轮，遇到上限、无进展、A/B 循环、修订失败会停止。问题身份包含位置、类型和规范化问题描述/一致性主体，下一轮未再次检出标为 `not_rereported`，并不命名成“已证明正确”。[主循环](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/pipeline/review_workflow.py#L228)、[轮数和状态](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/review/session.py#L68)、[未再次检出](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/review/session.py#L201)

### 6. 发布、状态和复查记录

每次运行有独立目录，保存初筛结果、撤销理由、证据请求、原始响应、冲突、修改历史、剩余问题和用量。中断后在内容、配置、提示词及术语指纹匹配时恢复；多次影子修改折叠成每段一条最终建议。[复用条件](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/pipeline/review_workflow.py#L59)、[结果产物](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/pipeline/review_results.py#L167)

**Autofix 的重要边界：**启用时，代码先接受所有结构有效的最终影子建议，明确不再用 `review_result` 过滤；随后针对遗留问题再次证据复核并生成修订。这个最后的 Fixer 输出后没有再次跑全书盲审。因此不要把 Wenyi 描述成“所有修改只有通过盲审才会自动应用”。[候选处理](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/pipeline/autofix_candidates.py#L45)、[最后修订流程](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/pipeline/autofix_verification.py#L155)

发布前先持久化计划，主流程持书籍锁；实际写入逐段检查当前值/旧值 hash：当前已等于新值则视为幂等恢复，等于旧值才写入，否则记录 `formal_target_changed`。Web 人工修改也使用 `expected_target`，冲突返回 409，写入修订记录并让旧审校状态失效。[编排锁](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/pipeline/orchestrator.py#L128)、[计划先落盘](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/pipeline/review_autofix.py#L131)、[发布校验](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/packages/core/wenyi_core/pipeline/autofix_publish.py#L49)、[人工编辑](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/apps/api/wenyi_api/routers/review.py#L89)

Web 审校页聚合问题、证据、建议和发布状态；证据面板展示的段落来自当前章节数据，源码明确没有把它伪装成历史快照。最终 public issues 字段比内部审校记录少，详细证据由 API 从详细产物补全。[展示聚合](https://github.com/BigDawnGhost/wenyi/blob/5f206257ddc05b113b0ec224518e31cb5d7b62ad/apps/api/wenyi_api/review_presentation.py#L34)

## 二、上游实现能支持的结论

值得借鉴的是错误与风格分开、证据可追溯、有界复核、整段最小修改、正式译文与候选隔离、接受时检测旧版本、失败不能当作无问题。它并没有提供“全书每个细节都理解正确”“多跑几轮必然提高质量”“引用正确就等于判断正确”的保证。是否提升质量及多大程度，仍需在我们的原译文样本上比较误报、漏报和错误修改率。

## 三、TransitPLS 已有基础和必须补齐的地方

本地基准：`6194d6e`。以下来自当前源码；不是沿用旧版功能对比文档。

| 现有能力 | 可复用部分 / 当前缺口 |
| --- | --- |
| [review.rs](../src-tauri/src/review.rs) | 已有问题类型、严重度、证据、报告和 CLI 入口；真实分支仍直接返回未接入，不能作为可运行审校功能 |
| [workflow/editing.rs](../src-tauri/src/workflow/editing.rs)、[revisions.rs](../src-tauri/src/revisions.rs) | 已有修改预览、版本编号、旧译文比较、人工保护、恢复历史；应复用这些写入规则 |
| [llm](../src-tauri/src/llm/mod.rs) | 已有统一客户端、结构化输出与用量记录；增加 review/verify/fix 阶段标识，避免审校消耗被归入 translation |
| [polish](../src-tauri/src/polish/mod.rs)、[state](../src-tauri/src/state/mod.rs) | 已有批次状态、输入摘要和项目锁模式；复用持久化函数和约定，不建设通用任务框架 |
| [App.tsx](../src/App.tsx)、[SegmentCard.tsx](../src/SegmentCard.tsx) | “问题”面板现在只显示术语冲突；段落已有编辑、版本和预览界面，审校需要新增入口及问题详情 |

不能只把 `review.rs:213` 的错误分支替换成一次模型调用。现有骨架还有这些相关缺口：

- `resume` 只是复用目录名，每次仍覆盖输入快照并重跑；`retry_failed` 只被保存到配置，没有驱动重试选择。
- 问题 ID 只有章、段、类型，同一段的两处同类错误会在去重时丢失；应为每个候选分配独立 ID，不按类型粗暴合并。
- 已审段数用“段落总数减未完成章节数”计算，单位不一致，也没有反映失败批次；应由成功批次的实际覆盖段落计算。
- 运行名的时间戳包含冒号，不适合作为 Windows 目录名；改用随机 ID 或无冒号时间戳，读取已有 run 时验证其属于当前项目目录。
- 报告直接 `fs::write`，没有逐批检查点；应使用现有持久化入口并在每个阶段完成后保存。现有 Windows 临时文件替换有先删后改名的间隙，不应对崩溃安全作超过实现的承诺。

## 四、建议首版：发现、核实、预览、人工采用

这是面向当前桌面项目的设计建议，尚未实现，也未证明优于 Wenyi。首版完成一个可用流程，默认不自动改写全书。

```mermaid
flowchart LR
  A[冻结原译文与术语快照] --> B[按连续块初筛]
  B --> C{有候选问题}
  C -- 有 --> D[补充证据并复核一次]
  C -- 无 --> E[保存已审覆盖范围]
  D --> F[问题报告]
  E --> F
  F --> G[按需生成整段修改建议]
  G --> H[人工对比与采用]
  H --> I[检查版本并保存历史]
```

### 1. 审校范围与输入

支持当前章和全书。先记录源语言、目标语言、当前译文版本、术语、文风、摘要、模型和提示词版本。按连续段落及原文/译文总字符预算组批，留出参考上下文和输出余量；不继续固定每 20 段一批。字符预算只是初版估计，不能宣称等于模型 token 预算。单段过长时明确报告超限或专门处理，不能静默截断待审正文。

批次包含本批原译文、稳定段落 ID、相关术语和邻近原译文。邻近段落只作证据，不能在本批重复报问题。空译文由本地检查报告为未翻译，已译段再交给 AI。梗概只能辅助判断，和原文冲突时优先核对原文。

首版 AI 错误类别重点覆盖漏译、增译、误译、术语、指代；纯风格偏好归润色。保留现有枚举兼容旧报告，按需增加 Addition/Pronoun；术语规则复用 `relevant_terms`、Fixed/NonFixed/Ignored 和唯一别名规则，不照搬 mock 的简单 contains 判错。未使用固定译名可作为候选，不能仅凭字符串未出现就判定误译。

### 2. 先用程序收集证据，再让模型复核一次

有候选的批次才做第二次调用。程序围绕候选段落和能在原文中核实的术语/表达收集邻近段、术语备注、全书有限次出现、对应章摘要，再让模型逐条 confirmed/dismissed/needs_context。没有证据时保留待确认状态，不让模型编造出处。

首版用现有术语匹配和章节扫描即可，无需向量数据库或自主多轮工具框架。后续如果数据显示跨章证据选择经常不足，再增加 Wenyi 式 `request_evidence` JSON 循环与查询次数/大小上限。也不需要为了支持审校先改造所有任务的模型路由；先复用当前配置的客户端，并记录具体模型便于比较。

响应应提供完整性标记及实际已审段落 ID。后端核对 ID 集合、问题所属批次、候选覆盖、合法枚举、非空解释、引用来源；失败和待确认不能计为“已确认无问题”。引用文本若使用摘录，必须能在快照中定位；漏译不强求译文中存在被遗漏内容的对应摘录。JSON 合法不代表语义结论正确。

### 3. 问题与建议分开保存

每个问题保存独立 ID、类型、严重度、解释、定位和证据；复核结果与人工处理状态分开。每条修改建议绑定 `run_id`、段落 ID、源文指纹、基础译文版本及文本、关联问题 ID、模型和完整替代译文。报告展示审校时的证据快照；当前内容变化时明确显示已过期。

用户点击生成建议时，把同段选中的有效问题一起交给 Fixer，并附原文、当前译文、相关术语、文风和邻近原译文，要求最小必要修改且只输出该段完整译文。复用单段重译的预览和采用规则，但不能直接调用现有 `request_translation` 当修复：现有重译请求没有把“当前译文和确认的问题”传给模型。

每条建议提供对比、采用、手工编辑、忽略；采用前重新加载当前章，校验源文、基础版本、旧译文及所依赖术语是否仍适用。冲突时保留建议并要求重新生成或人工比较，不覆盖当前内容。多个建议涉及同一段时，一项采用后其他基于旧版本的建议失效。

正式写入复用 `revisions::check_version`、`set_target`、标题同步、润色快照失效与 `state::write_chapter`。可复用 `RevisionKind::Adopt` 表示人工采用，关联审校运行/建议 ID。首次采用记录与译文历史放在同一个章节写入中，报告处理状态后更新；重试通过采用记录核对，避免正文成功而报告失败后重复采用。

### 4. 运行、取消和恢复

使用 `reviews/{run_id}/` 保存冻结快照、批次状态、问题、建议、用量和报告；初筛完成/复核完成分别落检查点，恢复时不重复调用已经完成的阶段。输入、术语、模型配置或提示词版本不匹配时新建运行，保留旧报告，不能覆盖旧快照伪装成续跑。

首版沿用项目锁和任务注册表：同项目翻译、润色、审校不同时启动；审校期间也不开放保存正文修改，和现有任务规则保持一致。支持取消、继续未完成部分、仅重试失败批次。任务取消后持久化已完成结果；崩溃遗留 running 状态在恢复时转成待处理。报告的运行状态与问题数量独立，0 个问题但存在失败批次仍为 partial。

恢复与保存使用 Windows 合法的运行 ID，并只接受当前项目内实际存在的运行。审校详情独立读取，不误用目前 `ProjectDetail.report` 所指的项目根目录报告；模型原始响应等调试信息留在详情日志，主界面展示用户能据以判断的证据和差异。

### 5. 界面与模块安排

- 工作台增加“审校本章 / 审校全书”，底部“问题”区区分术语冲突和审校问题；展示已审段数、失败/待确认数和调用用量。
- 新增一个审校详情组件，筛选类型/严重度/处理状态，点击问题定位正文并展示原译文证据和建议差异；复用段落已有编辑与版本交互。
- `review` 模块负责请求、证据、校验和报告持久化，外部只需启动/恢复、读取报告、生成建议、处理问题几类操作。内部按真实复杂度拆文件，不先搭 Agent 插件体系。
- `workflow` 负责客户端与项目锁；CLI 与 Tauri 调用同一套逻辑。`ui` 层接入任务取消和独立审校进度事件；正式采用通过现有编辑写入规则，不能出现另一套绕过人工保护的写入路径。

## 五、分三步实施和验收

| 独立交付步骤 | 完成条件 |
| --- | --- |
| 1. 真实审校与报告 | 当前章/全书能生成有来源的问题；初筛＋一次证据复核；正确区分失败、待确认和无问题；检查点、取消与恢复可用；CLI 已有过滤参数行为明确 |
| 2. 桌面问题工作台 | 能启动、查看进度、浏览历史报告、筛选问题、跳转正文及查看证据；术语冲突仍保留；过期报告有明确提示 |
| 3. 修改建议与采用 | 同段问题合并生成建议；预览差异；采用时校验版本；保留历史和人工保护；中断恢复不重复应用 |

必要的少量行为测试应覆盖：模型返回越界 ID/伪造引用/不完整结果被拒绝；同段两个同类问题均保留；无问题与请求失败分开；恢复不重跑完成批次；生成建议不改变正文；生成后正文/术语变化不能直接采用；重复采用不新增版本；采用后历史、标题与润色状态同步。旧报告字段升级应向后兼容，无法恢复的旧 mock 运行明确只读，不推测其完成状态。

质量验收另用人工标注的原译文片段：混入确实漏译/增译/指代错误和本来正确的自然意译、日中同形词、称谓变化，衡量误报率、漏报、采纳比例、新引入错误与成本。首版不承诺自动审校能替代人工定稿；跨块仲裁、全书影子多轮盲审和批量自动采用，等这些数据证明必要后再扩展。
