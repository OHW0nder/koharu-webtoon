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

## 3. 剩余步骤（已全部完成）

下面记录**实际落地的东西**与**两处偏离文档的决定**。

### 3.1 首次导入的广告高度入口

- [x] `import_series` 增加 `ad: AdBands` 参数，在导入循环开始前写进 `planned.settings.ad`（`series.rs`）
- [x] `StartView` 的 `ImportMenu` 在选完形态后填广告高度，条漫可填、页漫置 0
- [x] 广告带裁剪与「跳过原因」的日志报告在 §1.3 已就位，本期未改

### 3.2 渲染注入内容（纯函数）

`crates/koharu-app/src/injection.rs`（新建，11 个测试）：

- [x] 渲染是纯函数，不读文件也不读场景，四段拼成一段散文
- [x] 段顺序做成 `Injection::order`，默认值在 `Segment::DEFAULT_ORDER`，可覆盖而不改渲染逻辑
- [x] **服务商能力判断在这里**：`accepts_instructions(Provider)` 对 DeepL / Google Cloud / 彩云 返回
      false，此时 `render` 返回空并把 `unsupported` 报给界面，让用户自己发现功能没生效是最差的处理
- [x] **自带「不要翻译/不要输出」约束**：上文的抬头 `CONTEXT_HEADER` 与 `glossary::render` 的
      HEADER 各带一份，两段来源共用一个抬头，所以一份就够
- [x] 字节预算 `BUDGET`（8 KiB）：丢弃顺序为「章内最近→跨章最早」，人工资料永不丢
- [x] `Rendered` 报出 `matched` / `injected` / `dropped`，因为截断本身是静默的
- [x] 术语表顶层 `enabled` 的开关在界面上（`SeriesSettings.tsx`）：新建的表读回来是关闭的，没有这个
      开关的话词条能改一辈子但永远进不了提示词

### 3.3 作用域管线接线

**偏离文档：换掉的是配置，不是管线。**

`docs/series-translation-assets-audit.md` §3 给的五步是「构造作用域管线 → `handle.manage` 换上 → 结束后
`Pipeline::load` 换回」。按字面实现会**在每批批量之后永久泄漏一个翻译器**：`Pipeline::from_config` spawn 的
监听任务同时持有 `config` 句柄与 `translator`，而 `Config::memory` 的 watch 通道因为发送端也在任务手里，
永远不会关闭，于是旧管线连同**已加载的本地模型权重**一直留在内存里。选本地模型时这意味着每跑一批就多占
一份权重。

实际做法：**管线只有一条，跑在一份内存配置句柄上**。

- `app.rs` 的 `live_pipeline_config()` 以文件配置为起点建 `Config::memory`，`Pipeline::from_config` 订阅它
- 每章开始前只改这一个字段（`apply_injection`），现有监听器热替换阶段运行器
- 批内每章跑完刷新下一章的跨章上文
- 全部结束后 `restore_user_config` **重新读一次**用户配置，而不是回滚到批开始时的快照——回滚会把批量期间
  的设置改动从管线里抹掉

代价：管线不再订阅配置文件，所以设置页保存时要显式推一次内存句柄（`preferences.rs` 的
`publish_to_live_pipeline`）。

收益，逐条对上文档里的顾虑：

- 本地模型权重要么只读一次盘，要么一次也不读——文档里那条代价直接消失
- 漫画资料与用户设置**各写各的**，不存在互相覆盖，设置页也不会短暂显示术语表
- 逐章换上下文变成免费的，所以 §3.4 与 §3.5 都能按章生效
- 仍然是公开 API 组合，`koharu-pipeline` 与 `koharu-scene` 一行未动

### 3.4 章内上文派生

**走的是语境条目通道，逐页生效。**

- [x] `koharu-pipeline/src/context.rs`（新）：`preceding_context(snapshot, page, pages, prior)` 按
      `Snapshot::pages()` 的规范顺序取**当前页之前**最近 N 页里成对的原文与译文
- [x] `TranslationConfig.context_pages`（默认 4）由 `koharu-pipeline` 的翻译阶段就地消费
- [x] **窗口跨章连续**：`prior` 是上一章末尾那几页的同一批对照（按页分桶），翻译阶段按窗口余量取它的
      尾部并接在本章那几页前面。窗口恒为 N 页，翻到本章第 k 页时就是「上一章末尾 N−k 页 + 本章前 k 页」
- [x] 遇到没有译文的页就跳过，不为凑满条数继续往前找——回溯距离必须可预期。**空桶仍然占名额**，
      否则章首的回溯距离会悄悄变长
- [x] 硬上限 `MAX_CONTEXT_PAGES = 12`，防止一个填错的值炸掉上下文窗口
- [x] `trailing_pages(snapshot, pages)` 取整章末尾 N 页、每页一个桶，供窗口的章外那一段复用同一套判断
- [x] 12 个单测覆盖窗口滑动与跨章边界、空桶占位、未翻页跳过、冷启动、两种译文来源、锚点缺失、上限截断

**为什么此前判它「不可达」是错的。** 审计 §3 与 `koharu-glossary-design.md` §3 都把语境条目列为不可达，
理由是「赋值点在翻译阶段内部」。那个结论只覆盖了**从外部赋值**这一种方式。填充点内部并不在
`extending-koharu.md` §2.8 列的三个封闭位置（阶段枚举、服务商宏、提示词函数）里，按 §3.2 属黄色。
`koharu-translator` 一行未改——`TranslationRequest.context` 是 `pub` 字段，直接赋值即可，不必用那个
只在测试里存在的 `with_context` 构造器。

**章的那一段为什么能从外面递进来。** 场景里只有当前章，上一章的页不在其中，所以窗口靠章边界的那部分
由漫画层作为 `TranslationConfig.prior_chapter_context` 递进来：每章开始前写一次，翻完一章用刚跑出来的
译文覆盖给下一章。这条通道和 `instructions` 走同一个配置写入点，所以一样不需要在两次 `execute` 之间
重建阶段运行器。

**数据为什么已经在场。** `Execution` 每次提交后 `self.scene = next`，而每个新任务拿的是最新场景，所以
翻第 N 页时第 N−1 页的译文已经提交。`busy_stages` 按**阶段类型**记，因此同一时刻只有一个 Translation
在跑——这不是碰巧成立，是结构性保证，也不依赖 `page_window` 的大小。

**前置约束**：若上游把翻译改成章内跨页并发（`busy_stages` 拆成每页一份），`self.scene` 在启动第 N 页
时可能还没含第 N−1 页的译文，「前 N 页」就不再等于「已翻好的前 N 页」，必须先改为按运行序号维护队列。

**回溯单位是切片后的页。** 一张条漫长图导入时切成多个 band，每个 band 是场景里的一页，所以「前 4 页」
是四个切片页，大约覆盖原长图的三分之一。这与翻译粒度一致。

### 3.5 跨章上文

- [x] 读 `injection::load_prior_context(dir, seq)`，按**目标章**的序号命名 `context-<seq>.json`，与
      `series.json` 同级。载荷是**按页分桶的对照数组**，不再是渲染好的散文
- [x] 写：预扫描逐章打开写基线（支持中断续跑）；批内每章处理完趁它还是活动项目时覆盖写给下一章
- [x] 「上一章」按**章序号**判定（`SeriesAssets::successor`），与用户勾选和执行顺序无关
- [x] 两级降级安静退化：文件不存在或损坏 → 无上文；上一章没译文 → 退回人工资料
- [x] 抽取复用 `koharu_pipeline::trailing_pages`，与章内同一套判断，避免窗口两段取舍不同
- [x] 预扫描与「收集全部原文」共用同一趟，每章两次打开，与是否启用上文无关
- [x] 术语表坏了直接报错，不静默当成空表：用户以为在生效的术语表不见了，比一次失败更难排查

**跨章上文不再走附加说明通道。** 它曾经以散文段落的形式挂在附加说明末尾，代价有两个：整章每一页都
重复看着同一批先例，而那一段与翻译阶段自己取的章内部分各按 `context_pages` 算一遍，总量最多到两倍页。
现在它整个搬进语境条目通道，随窗口一起逐页滑动，`injection.rs` 里的 8 KiB 预算、`CONTEXT_HEADER` 与
`Segment::CrossChapter` 一并退休。

**跨章上文在同批内能吃到鲜度**：窗口的章外那一段在每章开始前写入，而批内每章跑完就覆盖写给下一章，所以
顺序递增时处理第 N 章读到的已是第 N−1 章刚跑完的译文。

### 3.6 前端统一设置面板

入口在**章节管理页**，三个分区并列。`SeriesSettings.tsx` 是设置区本体，`AdBandField.tsx` 是广告高度输入
（设置区与两个导入对话框共用一份，避免三处漂移）。

| 分区 | 控件 | 用的命令 |
| --- | --- | --- |
| 条漫广告 | 首条/尾条两个数值输入 + 清除，旁显示「本作品 N 章适用」 | `getSeriesSettings` / `setSeriesSettings` |
| 翻译指导 | 多行文本域 | 同上 |
| 术语表 | 词条列表（原文/译文/类别/启用/来源标记/备注）+ 添加、导入、导出 | `getGlossary` / `setGlossary` |

要点：

- 状态走 **react-query**，但 query key **不挂在 `seriesDetailKey` 下**：章列表带着同一份设置，挂在它下面
  会让每次写入都把整张列表重取一遍
- 写入模式：`useDebouncedSave`（350ms 防抖 + 卸载 flush），语义照 `OutputPicker.tsx:62-101`。判等与
  「已提交」都按**序列化后的字符串**做，这样一次被拒的写入不会在每次渲染时重试同一份载荷——而用户按键盘
  修不掉的失败（术语重复）是会发生的
- `set_series_settings` 整体替换 `settings` 字段，所以 mutation 在写入时从 cache 读回 `glossary` 文件名。
  发陈旧的 `null` 会抹掉索引里对磁盘上那个文件的引用
- 术语表提交前过滤掉 `source` 为空的占位行：`validate_glossary` 直接拒绝空原文，一个空行会让整表保存失败
- 词条 `id` 必须是 UUID（后端是 `GlossaryEntryId(Uuid)`），所以用 `crypto.randomUUID()` 而不是自造字符串
- **两个导入对话框从 `DropdownMenu` 换成 `Popover`**：Base UI 的 menu typeahead 对任何单字符按键
  `stopEvent`，`DropdownMenuContent` 里的数字输入框打不进字
- 不做滑块：条漫动辄上万像素，滑块刻度精度不够，而且用户是照着画面里的实际位置输入的
- 顶部状态行显示 `SeriesRun` 的命中数、注入数、丢弃数；`unsupported` 时明确提示当前服务商不接受提示词
- 文案加在 `series` 命名空间下（`series.ad` / `series.context` / `series.glossary` / `series.import` /
  `series.importAd` / `series.run` / `series.settings`），共 **52 个 key**，9 个语言文件都补齐。
  `localization.test.ts` 校验的是 **9 个文件 key 集合完全相同**，不是「只校验 en-US」
- 章内上文页数输入的上限镜像后端的 `MAX_CONTEXT_PAGES`，否则输入框能让用户填一个管线会静默截断的值；
  索引里存的值超过上限时也按上限读
- `SeriesSettings` 的 `Default` 是手写的而不是派生的：派生的 `u32` 会给 0，而 0 的语义是「不注入」，
  那样升级过的漫画会静默失去这个功能

### 3.7 导入对话框

- [x] `import_series`（新建漫画）：形态 + 首尾广告高度，**这是唯一无处可存的一次**
- [x] `import_series_chapter`（新增章节）：「沿用本漫画的设置」开关默认开；关掉后露出两个数字输入
- [x] 沿用传 `null`、改动传本次的值，**归属（写不写回索引）由后端决定**，前端不自己塞进 `setSeriesSettings`

---

## 4. 已确定不做

| 项 | 原因 |
| --- | --- |
| 角色卡 | 切片无法保证把说话人完整留在同一页（气泡尖尾可能落在上一页、人脸可能被切开），说话人归属不可靠。术语表不受影响：词条靠文本匹配定位 |
| 上文图片注入 | 翻译请求的图片位是单张且被当前页占用；拼长图会被 2048 上限压到每页约 512px，牺牲当前页细节 |
| 改 `SliceParams` 默认值 | 1600–2400 区间内 40px 气泡进模型后都是 19–29px，全部可用；降低上界是三项权衡里唯一全输的 |
| 术语自动抽取 | 依赖专用 NER 模型（不引入权重），且撞封闭的阶段图；价值全在「人工确认」环节 |
| 逐章覆盖广告高度 | 三项设置只有一份，不存在逐章覆盖，因此没有「某章广告设置是什么」这种需要查询的状态 |
| 跨漫画批量 | 每部漫画都要换一份注入内容，而换的动作是逐章写配置；跨漫画批量要先决定「按作品维度还是按章维度切换上下文」 |

---

## 5. 本期新增的已知限制

1. **一次 API 请求只能翻译一页。** 一张长图切 9–15 片，每片一次请求，所以固定开销（system prompt）
   重复约 8%。真正的重复大头是上文的滑动窗口（约占单次请求的一半）。**多页合并不可行**：
   `StageCompletion` 只带一个 `page`，一次处理多页后调度器只标记第一页完成，后两页会被重新调度，而
   第二次进 `process` 时译文已是 `Origin::User`，`translation.rs` 里的 `continue` 只跳过写入、请求照发。
   要标记后两页完成就得改 `StageOutput` 与 `Committer::commit` 的公开签名。
2. **回溯页数**：页数用户可调（默认 4、上限 12），且已经是唯一的上下文旋钮——窗口跨章连续，章内与章外
   取的都是这个值。段顺序（`Injection::order` 已留好字段）未在界面暴露
3. **广告高度的「清除」就是置 0**，没有「回到未设置」的状态——`AdBands` 只有两个数字，0 既是「没有」也是
   「不设」
4. **上报里没有「上文注入了几条」**：窗口总量由 `context_pages` 直接决定，且不存在静默截断，所以没有可
   报告的量。`SeriesRun` 只剩命中数与「服务商不接受提示词」

---

## 6. 提交前自查

- [ ] `rustup component add rustfmt` 后 `cargo fmt -p koharu-app -p koharu-pipeline`
- [x] `cargo check -p koharu-app` 零错误
- [x] `cargo test -p koharu-app --lib` **48 passed**
- [x] `cargo test -p koharu-pipeline` **64 passed** + 1 个 bin 测试
- [x] 改了 Tauri 命令签名就重跑 `cargo run -p koharu-app --bin generate`
- [x] 前端 `tsc --noEmit` 零错误；`oxlint` 零错误
- [x] `vitest tests/lib/localization.test.ts` **5 passed**（9 个语言文件 key 集合一致）
- [x] 未改 `koharu-scene` / `koharu-translator` / `koharu-ml` 的任何文件
- [x] `koharu-pipeline` 只改了 `context.rs`（新增）、`config.rs` 加字段、`translation.rs` 填 `context`、
      `lib.rs` 导出——全在 §3.2 黄色范围内，无红色改动
- [ ] 新增组件/关系用了非 `dev.koharu.*` 的命名空间

`tests/lib/runtime.test.ts` 有 4 个失败（`subscribe` 在 `waitFor` 的 1s 内没被调用），已用 `git stash` 确认是
**改动前就存在**的失败，与本期无关。
