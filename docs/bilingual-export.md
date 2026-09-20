# 双语导出

桌面端点击 TXT 或 EPUB 后，可以选择“仅译文”或“双语对照”。每次打开默认仅译文；双语默认译文在前，也可以选择原文在前。选项只影响本次导出，不写入项目配置。

CLI 示例：

```sh
transitpls-cli export book.txt --format txt --bilingual
transitpls-cli export book.epub --format epub --bilingual --order source-first
transitpls-cli export book.txt --format epub --bilingual --out dist/book.epub
```

省略 `--bilingual` 保持仅译文。`--order` 只接受 `target-first` 和 `source-first`，且必须启用双语。桌面端和 CLI 使用相同的后端参数校验与渲染规则。

默认文件位于源文件同级的 `output` 目录。仅译文使用 `书名.zh.txt` / `书名.zh.epub`，双语使用 `书名.zh-bi.txt` / `书名.zh-bi.epub`；CLI 的 `--out` 优先。

## 内容与兼容性

- 按原段落配对，使用保存的 `source` 和当前 `target`。长段按原文件与导入时的切分上限核对后合并；相同段落按位置匹配。导出不调用模型，不修改原译文或术语。
- 标题和目录只保留译文；空原文不生成原文块；原文与规范化后的译文完全相同则只显示一次。中文标点规范化仅处理输出的译文副本。
- EPUB 原文使用较小字号、淡色与段间距，并提供 `prefers-color-scheme: dark` 样式。原文副本去除原来的 `class` / `style`，保留 ruby 与链接等阅读结构。阅读器自己的主题可能覆盖文字颜色。
- 列表项、引用、嵌套容器中的正文在合法的原元素内部组合；图片、封面等资源保留一份。原文脚注位置保持不变；译文缺少可靠的字符位置映射，脚注引用放在译文段末。
- 原有锚点继续指向原目标。副本使用避重后的独立标识，原文内部链接优先指向对应的原文副本；没有副本时保留有效原目标。目录页不生成第二份目录。
- 缺少译文或标题、源文件哈希变化、段落对齐失败、重复锚点、原文副本中的链接目标缺失时明确报错。输出先写临时文件再替换；Windows 下目标被占用时旧文件保留，临时文件清理。

当前解析器不能完整提取所有混合嵌套结构。例如 `<div>前文<p>正文</p>后文</div>` 的直接文本可能没有保存为段落。双语导出会检查这些遗漏并拒绝输出，不能用未保存的译文补齐。需要先把这类直接文本整理为独立段落并重新导入。旧项目若因行内 `q`、命名空间前缀、实体或 CDATA 的解析修正而无法对齐，也会报错，不猜测合并关系。没有验证任意书籍的全部 CSS、脚本或复杂布局兼容性。

## 验收记录

日期：2026-09-20。环境：Windows，Rust / React / Tauri 项目。

| 检查 | 结果 |
| --- | --- |
| `cargo fmt` | 已运行 |
| `cargo clippy --all-targets -- -D warnings` | 通过 |
| `cargo test` | 102 个单元测试、7 个集成测试通过 |
| 阅读器修正后的 `cargo test --lib export::` | 12 个导出测试通过 |
| `yarn build` | TypeScript 与 Vite 构建通过 |
| CLI 实际运行 | 用隔离的 mock 项目导出双语 EPUB 和原文在前 TXT；显式输出路径和默认 `.zh-bi` 文件名正确 |
| 导出弹窗组件 | 浏览器中检查默认仅译文、双语默认译文在前、切换原文在前、提交参数与重新打开后的默认值 |

Rust 构建中的 MSVC 提示“正在创建库 … 和对象 …”被工具链归类为 `linker_messages` 警告；编译与测试均成功，Clippy 无警告。

测试样例覆盖 TXT 的 BOM / CRLF、重复长段、空原文、完全相同的原译文、当前润色译文与数据不变性；EPUB 覆盖长段合并、ruby / rp / rt、行内 q、实体 / CDATA、列表与引用嵌套、图片去重、`id` / `name`、跨 XHTML 脚注与返回、没有副本的目标、spine 中的目录页，以及无效源数据和输出失败。输出 ZIP、XHTML 结构及全部样例内部链接均有检查。

实际阅读器使用 **EPUB.js 0.3.93**（本地加载书籍，JSZip 3.10.1，Chromium 浏览器承载）。检查了两种排列顺序、浅色和深色主题、150% 字体、ruby 显示、跨页原文脚注及返回、目录往返和原目标回退；也打开了由 TXT 生成的基础 EPUB。视觉检查发现的 CDATA 显示问题已修正并复核。深色主题检查包括阅读器覆盖文字颜色的情况；未验证 Calibre、Thorium 或硬件阅读器。

桌面弹窗的视觉检查使用当前组件源码和项目 CSS 的隔离页面；未在这次验收中启动完整 Tauri 桌面窗口执行端到端导出。后端命令已编译，CLI 与核心导出已执行。

重新运行 `cargo test --lib export::` 后，原 EPUB 验收文件生成于 `src-tauri/target/export-fixtures/`：

- `bilingual-source.epub`：原始小型样例。
- `bilingual-TargetFirst.epub`：译文在前。
- `bilingual-SourceFirst.epub`：原文在前。
