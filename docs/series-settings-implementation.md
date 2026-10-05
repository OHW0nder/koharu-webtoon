# 漫画级翻译辅助：实施计划

本文记录**实施进度与剩余步骤**，供后续会话接续。设计判定在下列文档里，本文不重复：

| 文档 | 内容 |
| --- | --- |
| `docs/series-settings-design.md` | 三项设置的归口：存储位置判定规则、统一管理入口 |
| `docs/reference/webtoon-ad-bands-design.md` | 广告带裁剪的完整判定与降级矩阵 |
| `docs/series-translation-assets-audit.md` | 术语表与翻译指导的注入通道审计 |
| `docs/reference/koharu-glossary-design.md` | 上下文派生、命中过滤、拼接顺序、已知限制 |
| `docs/extending-koharu.md` §2.8 | 行为层没有扩展点，以及三条注入通道 |

---

## 1. 已完成

### 1.1 数据层

`crates/koharu-app/src/commands/series.rs` 新增类型（全部 `pub`，随协议导出到 TS）：

- `AdBands { head, tail }` —— 广告带高度，`Copy`
- `SeriesSettings { ad, guidance, glossary }` —— 替换掉原来的空结构体
- `Glossary` / `GlossaryEntry` / `GlossaryEntryId` / `GlossaryKind` / `GlossaryValueOrigin`
- `normalize_glossary_source`（NFKC + 折叠空白 + 小写）、`validate_glossary`、`GLOSSARY_FILE`

术语表结构**逐字段镜像上游社区实现**（PR #1137 的 `dev.koharu.glossary`），只加一个本地扩展字段 `note`。将来上游若实现同类功能，迁移是字段对字段的复制。

`koharu-app/Cargo.toml` 增加 `icu_normalizer`（工作区已有，含 `compiled_data`）。

### 1.2 术语表逻辑

`crates/koharu-app/src/glossary.rs`（新建，15 个测试）：

- `load(dir)` / `save(dir, glossary)` —— 读缺文件返回空表；写前校验、临时文件改名
- `matched_entries(glossary, haystack)` —— 命中过滤，结果与输入顺序无关
- `render(glossary, matched)` —— 渲染成可追加到附加说明的散文

### 1.3 导入裁剪

`crates/koharu-app/src/commands/import/mod.rs`：

- `import_webtoon(paths, slicing, ad) -> WebtoonImport`（**签名变了**）
- `WebtoonImport { imported, ad_bands }`、`AdBandReport { trimmed, skipped }`、`AdBandSkip { Covered, TooShort, NotSliceable }`
- 三个调用点已修：`lifecycle.rs` 的独立导入命令、`series.rs` 的 `import_series` 与 `import_series_chapter`
- `report_ad_bands` 把跳过写成 warn 日志

### 1.4 命令

`series.rs` 四条命令 + `commands/mod.rs` 注册，协议已重新生成（`protocol.ts` 已有 `getSeriesSettings` / `setSeriesSettings` / `getGlossary` / `setGlossary`）：

| 命令 | 签名 |
| --- | --- |
| `get_series_settings` | `(id, library) -> SeriesSettings` |
| `set_series_settings` | `(id, settings, library) -> SeriesSettings` |
| `get_glossary` | `(id, library) -> Glossary` |
| `set_glossary` | `(id, glossary, library) -> Glossary` |

`set_glossary` **先写术语表文件再写索引**，因为索引只记文件名，反过来会让索引指向一个从未写出的文件。

验证：`cargo check -p koharu-app` 零错误零警告；`cargo test -p koharu-app --lib` **37 passed**。

---

## 2. 实施中发现的坑（后续步骤会再遇到）

**2.1 `RowStat.y` 是绝对行号。** 「把剖面切中段再传给 `plan_slices`」是**错的**——每个切点会提前 `head` 像素。必须**重基**（`y -= head`），见 `middle_rows`。

**2.2 `Imported::Strip.height` 必须是原图高。** 裁剪后 `plan.height` 是中段高，而 `apply()` 把它当 `PageSlice.source_height`。源 blob 仍是完整原图，所以用 `page.height`。

**2.3 「没裁」不等于「丢掉」。** 广告带放不下的图**仍要整张进项目**，只是同时被报告；只有 `Covered` 才什么都不剩。第一版把两者合并成一个 `CutOutcome::Skipped`，导致图被静默丢弃，被测试 `a_middle_shorter_than_one_page_is_imported_whole_and_reported` 抓到。现在是 `Imported` / `ImportedWhole` / `Dropped` 三态。

**2.4 haystack 归一化不能折叠空白。** 折叠会把气泡之间的换行变成空格，于是上一格末尾的 `Ann` 与下一格开头的 `Smith` 拼成 `ann smith` 命中一条双词术语。跨气泡假命中比漏命中坏得多。

**2.5 词边界判据不能用 `is_alphabetic`。** 它对假名为真，用它会让 `アリス` 在 `アリスは` 里判成不命中。判据必须是「这个文字用不用空格分词」——`is_spaced_alphabet` 里写死拉丁/希腊/西里尔码位。

**2.6 术语表全文匹配不做模糊。** `Ａ ＬＩＣＥ` 归一化后是 `a l i c e`，与 `alice` 本来就是两个字符串，**不命中是对的**。

**2.7 `rustfmt` 组件未安装。** `cargo fmt` 报 `cargo-fmt.exe is not installed`，提交前需要 `rustup component add rustfmt`。

---

## 3. 剩余步骤

按依赖排序。每步只碰自己那几个文件，3.2 / 3.4 / 3.6 可并行。

### 3.1 首次导入的广告高度入口

**现状**：`import_series` 用 `planned.settings.ad`，恒为 0/0，所以**首批导入不裁广告**。

**要做的**：`import_series` 增加一个可选参数（广告高度），导入完成后写进 `planned.settings`。这是本项目自己的命令，改签名不违反 `docs/extending-koharu.md` §3.3（那条约束的是上游拥有的命令）。

**判断**：漫画尚不存在时无处可存，所以这一次必须由对话框给出；之后一律从设置区管理。

### 3.2 渲染注入内容（纯函数）

新建 `crates/koharu-app/src/injection.rs`，把四段拼成一段文本：

1. 漫画级翻译指导（`settings.guidance`）
2. 术语表渲染文本（`glossary::render`，只含命中条目）
3. 章内上文（§3.4）
4. 跨章上文（§3.5）
5. 用户全局指导（`PipelineConfig.translation.instructions`，兜底）

要点：

- **服务商能力判断放在这里**。作用域管线在构造之前已读到当前配置，其中的模型选择包含服务商，所以判断渲染期即可，**零 pipeline 改动**。不支持提示词的三家（DeepL / Google Cloud / 彩云）直接不注入。
- **四段的顺序是设计决定，尚未实测**。建议作为漫画级设置里的可调参数暴露，不要写死（见 `koharu-glossary-design.md` §3.2）。
- **必须自带「不要翻译/不要输出这份清单」**。系统提示词里那句约束是随语境条目字段出现的，走附加说明通道时它不会出现。`glossary::render` 已经带了，上文拼进去时要一起带上。
- 字节预算：人工资料 > 章内上文（从近到远丢）> 跨章上文（从最早丢）。截断静默，但界面要显示命中数与注入数。

**测试**：全部纯函数。重点覆盖「服务商不支持时返回空」与「资料为空时不产生空段落」。

### 3.3 作用域管线接线

`crates/koharu-app/src/commands/series.rs` 的 `process_series_chapters`（约 596 行）：

```
批量开始前：
  1. 以 PipelineConfig::load()?.read()?.clone() 为基线
  2. config.translation.instructions = Some(render(...))
  3. let scoped = Config::memory(config)
  4. handle.manage(Pipeline::from_config(scoped, ProvidersConfig::load()?, device)?)
逐章处理，全部结束后：
  5. handle.manage(Pipeline::load(device)?)   // 恢复
```

**已确认的代价**（别重复踩）：`Pipeline::from_config` 会重建 `Translator`，而新的翻译器不知道任何已加载模型，**选用本地模型时这是一次完整的权重读盘**。批量按漫画维度触发，单次批量付一次。

**一次渲染 vs 逐章渲染**：一次渲染需要**全部章的原文**做术语命中（`koharu-glossary-design.md` §4.3），所以批量开始前要预扫描逐章打开收集原文。预扫描同时用于跨章上文的基线（§3.5），两趟合一。

### 3.4 章内上文派生

纯函数，只读场景：从当前页往前回溯 N 页（默认 4，可配），取已成对的原文与译文。

- 「已翻好的」判定：优先 `Translation.text.origin` 为 `User` 的，其次 `Generated`
- 页序用 `Snapshot::pages()` 的规范顺序
- 预算满时从最近一页开始丢

**前置约束**：现在调度器用 `busy_stages: BTreeSet<Stage>` 保证翻译跨页串行（`scheduler.rs:106-108`），所以「上一页已提交」成立。**若将来改成并发（上游 PR #1097 想改成最多 4 页），这一项必须先改为按运行序号维护队列。**

### 3.5 跨章上文

**上一章按章序号判定**，与用户勾选和执行的顺序无关（`koharu-glossary-design.md` §2.4）。

`ProjectLibrary::open(name)` **返回带 `snapshot()` 的 `Project` 但不安装为活动项目**（`replace_project` 是另一次调用），所以可以打开任意章、读它的场景、丢弃，不打扰正在跑的那一章。

两趟写入「目标章」的旁挂文件（与 `series.json` 同级）：

- 预扫描：逐章打开 → 读双语尾部 → 按**序号 +1** 写入（这是「磁盘上已有译文」的基线，支持中断续跑）
- 批量循环中：每章处理完，趁它还是活动项目时读自己的尾部 → 覆盖写入序号 +1 的那一章（鲜度更高）

两级降级：上一章没有译文 → 无跨章上文；超出预算 → 从最早开始丢。**两项都安静退化，不报错。**

### 3.6 前端统一设置面板

入口在**章节管理页**（`packages/koharu/components/series/SeriesView.tsx`），三个分区并列：

| 分区 | 控件 | 用的命令 |
| --- | --- | --- |
| 条漫广告 | 首条/尾条两个数值输入 + 清除 | `getSeriesSettings` / `setSeriesSettings` |
| 翻译指导 | 多行文本域 | 同上 |
| 术语表 | 词条列表（原文/译文/类别/启用/来源）+ 添加、导入、导出 | `getGlossary` / `setGlossary` |

要点：

- 状态走 **react-query**，与 `projectKey` 同级。项目切换时随既有刷新链路失效。**不要放全局 store**——它没有项目维度，切换项目时要额外清理，容易漏。
- 写入模式照 `InferenceControl.tsx:162-179` 的 `saveOutput`（串行队列 + generation 守卫）或 `OutputPicker.tsx:62-101`（350ms 防抖 + 卸载 flush）。
- 不做滑块：条漫动辄上万像素，滑块刻度精度不够，而且用户是照着画面里的实际位置输入的。
- 顶部状态行显示「本作品 N 章适用」（广告带）与「本章命中 N 条术语」。
- 文案新增一个顶层命名空间（如 `glossary`），插在字母序合适位置。**9 个语言文件都要手工加**（`packages/koharu/public/locales/`），没有提取工具。测试只校验 en-US。

### 3.7 导入对话框

- `import_series`（新建漫画）：首尾广告高度字段，**这是唯一无处可存的一次**
- `import_series_chapter`（新增章节）：显示继承来的值 + 「沿用本漫画设置」开关；开关关掉后可编辑，本次使用编辑后的值，**不写回索引**

---

## 4. 已确定不做

| 项 | 原因 |
| --- | --- |
| 角色卡 | 切片无法保证把说话人完整留在同一页（气泡尖尾可能落在上一页、人脸可能被切开），说话人归属不可靠。术语表不受影响：词条靠文本匹配定位 |
| 上文图片注入 | 翻译请求的图片位是单张且被当前页占用；拼长图会被 2048 上限压到每页约 512px，牺牲当前页细节 |
| 改 `SliceParams` 默认值 | 1600–2400 区间内 40px 气泡进模型后都是 19–29px，全部可用；降低上界是三项权衡里唯一全输的 |
| 术语自动抽取 | 依赖专用 NER 模型（不引入权重），且撞封闭的阶段图；价值全在「人工确认」环节 |
| 逐章覆盖广告高度 | 三项设置只有一份，不存在逐章覆盖，因此没有「某章广告设置是什么」这种需要查询的状态 |
| 跨漫画批量 | `Pipeline::from_config` 每部漫画付一次本地模型重读盘 |

---

## 5. 提交前自查

- [ ] `rustup component add rustfmt` 后 `cargo fmt -p koharu-app`
- [ ] `cargo check -p koharu-app` 零警告
- [ ] `cargo test -p koharu-app --lib` 全绿
- [ ] 改了 Tauri 命令签名就重跑 `cargo run -p koharu-app --bin generate`
- [ ] 新增组件/关系用了非 `dev.koharu.*` 的命名空间
- [ ] 未改 `koharu-scene` / `koharu-pipeline` / `koharu-translator` / `koharu-ml` 的任何既有文件
