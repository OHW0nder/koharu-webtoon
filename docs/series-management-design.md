# 漫画管理：删除、批量处理与章节导航

本文接续 `docs/series-layer-design.md`，记录该文写作时未覆盖、后来由实际使用暴露出来的四个缺口：

1. 漫画与章都删不掉；
2. 批量处理只有一个名叫「翻译」的下拉，能力被前端限死；
3. 进入某一章之后没有任何回到同一部漫画其他章的入口；
4. 管理口径上「未分组项目」与「一部漫画只导入一话」并存，同一件事有两种归属方式。

三条前提已定：**项目尚在开发、没有正式数据**，因此不做向前兼容与迁移；**不允许建空项目**；**删除的语义是「导错了，重导会拿回原来的位置」**。

---

## 1. 口径：每个 `.khrproj` 都属于且只属于一部漫画

`series-layer-design.md` §3 定的策略是「删除一章不影响漫画定义，删除漫画不影响章项目」，两条生命周期各自独立。这条策略的前提是「未认领的普通项目是合法的一等公民」，而前提已经不成立了。

收口成一句不变式：

> **每一个章项目都恰好被一部漫画的一个章条目认领。**

由此推出三件事，全部是删代码而不是加：

| 结论 | 依据 |
| --- | --- |
| 删章**必然**删章项目 | 否则留下无主项目，而无主项目不再合法 |
| 删漫画**必然**删全部章项目 | 同上；不能留下孤儿 |
| 一部漫画只导入一话 = 单行本 | `plan_series` 的 `OneShot` 分支已经是这个语义，只是没有单独入口 |

「删除的语义」由用户确认：**导入多了或导入错了，删掉，之后会重新导入同一话**。用户明确定义了这一章是「替换内部的漫画文件」而不是「删掉这一章位置」，所以章项目必须重建，而重建的项目名 `<漫画名> Ch<NN>` 会与旧项目同名——删除若不连带删项目，重导就会撞名。这一条同时决定了 §2。

连带删除的清单：

| 删除对象 | 位置 |
| --- | --- |
| `create_project` / `delete_project` / `list_projects` 命令 | `lifecycle.rs` |
| `ProjectLibrary::list()`（`list_projects` 的唯一调用者） | `project.rs:197` |
| `SeriesLibrary::claimed_projects()`（`list_projects` 的唯一调用者） | `series.rs:531` |
| `UngroupedProjects.tsx` 组件与其挂载点 | `packages/koharu/components/start/` |
| `shelf.ungrouped` / `shelf.ungroupedEmpty` / `shelf.blankPlaceholder` 等 i18n | 全部 9 个 locale |
| `welcome.test.tsx` 中依赖 `listProjects` / `createProject` / `deleteProject` 的用例 | `packages/koharu/tests/` |

`delete_project` 的删除顺带修掉一个既有的悬空引用问题：它原先只 `remove_dir_all` 项目目录、不动 `series.json`，删掉章项目后索引里会留下打不开的 `SeriesChapter.project`（`Assets::collect` 用 `else { continue }` 静默跳过，所以不崩，只是界面上章还在、点开失败）。删除权收口到 series 侧之后这条路径不复存在。

**导入命令的去留**：`import` 与 `import_webtoon` 已是孤儿（前端只在 `queries.ts:128/137` 定义了 hook，无人调用；`menu.import` / `importFiles` / `importFolder` / `importWebtoon` 全是孤儿 key）。`series-layer-design.md` §4.1 曾明确「底层命令保留不动」。统一口径后编辑器侧不再有任何导入入口，这两个命令可以一并删掉——但它们是 `docs/reference/koharu-webtoon-support.md` 记录的「上游导入命令」，删之前确认那份参考文档是否要同步收缩。

---

## 2. 序号是可复位的槽位，不是单调计数器

这是本次改动里唯一一处**现有实现与用户语义直接冲突**的地方。

```826:829:crates/koharu-app/src/commands/series.rs
    let seq = series
        .chapters
        .last()
        .map_or(1, |chapter| chapter.seq + 1);
```

`SeriesLibrary::read` 按 `seq` 升序排章，所以 `last()` 是最大的那个。删掉第 22 章之后索引里剩下 `1..21, 23, 24`，此时重新导入 22 章会拿到 **`seq = 25`**：

- 22 章排在 23、24 章之后，章列表顺序错乱；
- 更要命的是上文——`Assets::successor`（`injection.rs:211`）按 `seq` 找「下一章」，而 `context-<N>.json` 按**目标章的序号**命名，所以 25 章的前文会去取 24 章的译文。用户期望的是 22 章从 21 章取。

改成补空洞：

```rust
/// 下一个可用的章序号。
///
/// 序号是作品结构里的**槽位**，不是单调计数器。用户删掉导错的第 22 章之后会重新导入同一话，
/// 那一话必须拿回 22 —— 否则它会排到 24 章之后，上文也会从错误的章取。序号连续不代表作品完整，
/// 空洞本身是有意义的信息：它标着「这里少了一话」。
///
/// 没有空洞才往上接，因此这里是 `min(holes) ?? max + 1`。
fn next_seq(chapters: &[SeriesChapter]) -> u32 {
    let taken = chapters.iter().map(|chapter| chapter.seq).collect::<BTreeSet<_>>();
    // 上界取 `len + 1`：序号互异，`len` 个已占用的序号在 `[1, len + 1]` 里必定留下空位，
    // 而全满时 `len + 1` 正是答案。所以试这么多步一定有答案。
    (1..=taken.len() as u32 + 1)
        .find(|seq| !taken.contains(seq))
        .expect("a hole always exists within the bound")
}
```

`import_series` 的 `plan_series`（`series.rs:971`）用 `index as u32 + 1` 编号，那是全新漫画、序号必然连续，不用改。

**章列表不重排。** 删除章时其余章的 `seq` 一个都不动。理由是 `seq` 是 `context-<seq>.json` 的键，也是「下一章」判定的依据；重排会让已有的译文上文错位到错误的章。删中间章之后序号出现空洞，是正确且可读的结果——界面上 `Ch22` 缺失、`Ch23` 照旧，用户能直接看出少了一话。

**候选列表不再预览序号。** `CandidateChapter.seq` 字段删除（前端从未使用，只用到 `name` 与 `files`）。理由是一次只导入一个候选，第一个导入成功后空洞就被填上，其余候选的预览序号立刻过期；一个不准确又常驻界面的数字比没有更糟。候选显示章名 + 文件数即可。

---

## 3. 章名按自然序

下载器把章目录命名为 `Ch01`…`Ch100`。`scan_series_source` 现在按 `children.sort()` 排，那是**路径字典序**，`Ch100` 会排在 `Ch02` 前面。候选列表照这个顺序显示，用户很容易在「是不是漏了一章」上判断错。

自己实现一个比较器，不新增依赖：

```rust
/// 章名的自然序：数字段按数值比，其余按字符比。
///
/// 章目录是下载器按数字命名的，所以 `Ch100` 必须排在 `Ch02` 之后。`Path`/`str` 的字典序做不到
/// 这一点，而按错误顺序分配序号会把候选映射到错误的槽位。
fn natural_cmp(left: &str, right: &str) -> std::cmp::Ordering
```

适用两处：`scan_series_source` 的候选排序、`SeriesLibrary::read` 的章排序。章列表目前按 `seq` 排（`series.rs:559`），在 §2 的槽位语义下已经等于自然序，不需要改；但按章名导入的漫画（`Ch01`…）与下载器另有命名规则时两者会分叉，因此以自然序为准更稳。

---

## 4. 上文旁挂文件的失效规则

`context-<N>.json` 存的是**「N 章的上一章末尾若干页的原文与译文对照」**，命名按目标章而非处理顺序（`injection.rs:342`）。删掉序号 S 的章之后：

| 文件 | 来源 | 处理 |
| --- | --- | --- |
| `context-<S>.json` | S 的上一章（未受影响） | **保留** |
| `context-<N>.json`，N > S | 原第 S 章（已删除或将被替换） | **删除** |

新章还没建好项目时（`Assets::collect` 里 `projects.open` 失败会 `continue`），残留的 `context-<N>.json` 就是一份陈旧基线。所以删除章时主动清掉 N > S 的文件。

```rust
/// 丢弃某章之后的上文旁挂文件。
///
/// `context-<N>.json` 存的是 N 章上一章末尾的译文，所以删掉序号 S 的章之后，N > S 的文件
/// 来源已不存在或即将被替换。S 自己那份来自 S 的上一章，仍然有效。
pub(crate) fn discard_prior_context_after(directory: &Path, seq: u32) -> Result<()>
```

保留 `context-<S>.json` 是关键：它装的正是重新导入的 S 章要用的前文（来自 S-1 章）。删掉它会让重导的这一章在首次运行前没有上文，虽然下一次 `Assets::collect` 会重建，但用户「单独重新翻译这一章」的动作可能先于任何批量运行。

---

## 5. 命令

### 新增

| 命令 | 职责 |
| --- | --- |
| `delete_series(id)` | 删漫画目录（`series.json` / `glossary.json` / 全部 `context-*.json`）+ 该部漫画的全部章项目。若某一章项目正打开，先关闭它 |
| `delete_series_chapter(id, project)` | 移除章条目 + 删章项目 + 清 `N > seq` 的上文文件。**不重排其余章的 `seq`** |
| `set_series_source(id)` | 重新指定源目录。只换 `source_root`，**不自动导入** |
| `rename_series(id, title)` | 改标题。`id` 由目录名决定，所以同时改目录名与索引里的 `title` |

`set_series_source` 只换目录、不自动导入是刻意的：换源之后新目录里的章名可能与已登记的 `source` 撞名，自动导入会重导正在正常工作的章。让用户点「导入新章」、在候选列表里看到 `files` 数之后再确认。源目录从「导入那一刻由系统对话框决定、之后永久锁定」变成一个可在设置面板改的字段，这正是「不再强制锁定」的落点。

`delete_series` 与 `delete_series_chapter` 都要走 `reject_import_while_processing`；`delete_series` 还需处理「章项目正打开」——`delete_project` 原先的做法是命中活动项目就先 `close_current_project`，沿用同一套。

### 删除

见 §1 的表。另见 §7 的 `wait_for_job`。

---

## 6. 前端

### 6.1 章节管理页

| 位置 | 变更 |
| --- | --- |
| header | 「翻译」改名「处理」，下拉换成与单章一致的分阶段选择（§6.2）；末尾加「…」菜单：重新指定源目录、重命名、删除漫画 |
| `ChapterRow` | hover 出 `Trash2` → 删除该章（`AlertDialog` 二次确认） |
| 全选栏 | 加「删除所选 N 章」 |
| 设置面板 | 加「源目录」行：显示当前路径 + 「重新指定」按钮 |

删除漫画的确认文案必须写明不可逆范围，例如「将永久删除 N 章的译文与导出文件，以及漫画的术语表与设置」。翻译结果是项目里唯一无法重建的产物——源文件还在，但译文要重新跑一遍模型。

### 6.2 分阶段处理（批量与单章共用一套控件）

后端 `process_series_chapters` 已经接受任意 `Operation`，一个阶段的分离命令都不需要加。问题全在前端：`PIPELINES` 硬编码 3 项，入口叫「翻译」但其中 `full` 跑的是全流程，而 `InferenceControl` 有完整的四阶段选择器（`InferenceControl.tsx:301-332`）——两处各写一份必然漂移。

抽出共享组件：

```tsx
/** 管线阶段的选择。两个入口共用它：单章的 InferenceControl 与批量处理下拉。
 *  分开写必然漂移——用户看到的「处理」在两个界面里必须是同一件事。 */
function StagePicker({ stages, onChange }: { stages: Stage[]; onChange: (stages: Stage[]) => void })
```

四个阶段 `detection` / `ocr` / `translation` / `inpainting`（`protocol.ts:801`）做成可勾选，外加「全选」快捷。批量入口随之改名「处理」，与 `TitleBar` 的「处理」菜单对齐。

顺带说明：管线只有这四个阶段，**没有「组装 / 排版」步骤**。排版由导出时的 `Renderer` 负责（`export_snapshot`，`output.rs:94`），不是流水线阶段。所以分步骤按钮给到这四个就是完整的，「检测、OCR、翻译等」的期望是对齐的。

### 6.3 章间跳转

编辑器完全不感知 series：`components/editor/**` 里 `series` 零引用，`seriesId` 只用于关闭项目后的回退。返回路径只有 `文件 > 关闭项目`（`TitleBar.tsx:111`），所以从第 22 章换到第 23 章要「关闭 → 在列表里点 → 打开」，两次操作加两次项目重建。

`open_project` 在项目已打开时会直接 `replace_project`（`lifecycle.rs:298-306`，含停掉在跑的作业、重置 agent、重发 canvas 状态），所以**切章不需要先关闭**，一条命令即可：

```
store: chapter: { seriesId, project } | null   // 在 SeriesView 的 open() 里写入
CanvasCommandBar 左侧: [‹] 章标题 [›]  +  章列表 Popover
```

`useSeriesDetail` 因此多了一个 `enabled` 参数：编辑器在 `chapter` 为 `null` 时不能去查一部不存在的漫画。`chapter` 为 `null` 时整个控件不渲染——项目总是从章节列表打开的，没有别的入口，所以「查不到系列」只可能是索引损坏。

**徽章只显示章号，不显示「n / 总数」。** 序号是槽位，删掉一章之后剩下的会是 `22 / 23` 这种读起来像「全部译完了」的数字，而它其实表示缺了一章。真实的先后在章列表里一眼可见，所以徽章不必去解释它。

章列表 Popover 复用 `ImportChapterDialog` 已有的 popover 形态（`SeriesView.tsx:246-247` 的注释说明为什么用 popover 而非 dropdown：广告带字段要能打字，menu 的 typeahead 会吞掉每个字符）。

**没有做快捷键。** 方案里提过 `Ctrl/Cmd + [ ]`，但快捷键在 `lib/store.ts` 里是一张可重映射的表，加两个动作要同时改类型、默认值和设置面板，而它们是否值得占这个位置应该由实际使用决定。

### 6.4 章号排序的界面副作用

§3 的自然序与 §2 的槽位语义在 UI 上要区分开：章列表按 `seq` 升序（空洞可见），候选列表按名称自然序（无 `seq` 可言）。两处不同序是刻意的，不要在实现时「顺手统一」。

---

## 7. 前置修复：批量等待是死循环

批量分步骤按钮建在这段代码之上，而它现在跑不通。

```1102:1114:crates/koharu-app/src/commands/series.rs
async fn wait_for_job(processing: &Processing, job: JobId) -> Result<()> {
    loop {
        let state = processing.jobs.lock().get(&job).map(|job| job.state.clone());
        match state {
            Some(JobState::Finished) => return Ok(()),
            Some(JobState::Failed) => bail!("the chapter's job failed"),
            Some(JobState::Stopped) => bail!("the chapter's job was stopped"),
            Some(JobState::Running) | None => {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }
}
```

作业结束时是**先从 `jobs` 里 `remove`、再设终态、再经 `JobChannel` 广播**（`processing.rs:330-348`）——终态永远不留在 map 里，所以这个轮询只会读到 `None` 并无限 sleep。更糟的是 `replace_project`（`lifecycle.rs:197`）每切一章就 `jobs.lock().clear()`，即使把终态留在 map 里也会被清掉。

**终态随作业一起交出来，不查共享 map：**

```rust
pub(crate) struct Started {
    pub(crate) id: JobId,
    /// 终态随作业交出来，不从共享 map 里查：`replace_project` 每次切章都会 `jobs.clear()`，
    /// 而作业结束时本来就会把自己从 map 里移除，所以 map 里永远查不到终态。
    terminal: oneshot::Receiver<JobState>,
}
```

`start_job` 返回 `Started` 而不是 `JobId`。单章路径取 `.id` 发回前端（照旧，`InferenceControl` 的停止按钮与 `ActivityCenter` 的进度都靠 `JobChannel`）；批量路径 `.terminal().await` 一次。`wait_for_job` 与轮询一起删掉。

这个改法顺带修掉一个隐患：现在批量若在第一章失败，`wait_for_job` 永远等不到 `Failed`，只能靠用户点停止；而停止会走 `Processing::stops`，于是整批以「the chapter's job was stopped」结束——错误与用户操作混为一谈。

---

## 8. 实施顺序

| # | 内容 | 状态 |
| --- | --- | --- |
| 1 | `Started` / `wait_for_job` 修复 | 完成 |
| 2 | `next_seq` + §4 的上文清理 + `natural_cmp` + 删 `CandidateChapter.seq` | 完成 |
| 3 | `delete_series_chapter` / `delete_series` / `set_series_source` / `rename_series` | 完成 |
| 4 | 删 `create_project` / `delete_project` / `list_projects` / `UngroupedProjects` / 孤儿 i18n 与测试 | 完成 |
| 5 | `StagePicker` + 「处理」改名 + 删除/源目录 UI | 完成 |
| 6 | 章间跳转 | 完成（未含快捷键，见 §6.3） |
| 7 | 删 `import` / `import_webtoon` 及其孤儿文案 | 完成 |

第 7 项顺带清掉了两个只为它们存在的类型：`webtoon::PageImportSlicing`（Tauri 参数类型）与 `import::Slicing::Forced`（编辑器里那个「强制切页」选项）。强制切页随之失去唯一的入口，而导入现在只从漫画柜走、在导入前就选定了章节形态，所以这个选项本来就没有位置。`docs/reference/koharu-webtoon-support.md` 里记着这两个命令的两处也一并改了。

---

## 9. 对既有文档的修正

| 文档 | 修正 |
| --- | --- |
| `series-layer-design.md` §3 | 「删除一章不影响漫画定义，删除漫画不影响章项目」不再成立，见本文 §1 |
| `series-layer-design.md` §4 表格 | `delete_series_chapter` 从「是否连带删除章项目由调用方显式指定」改为**必然删除** |
| `series-layer-design.md` §4.2 | 「等待 Job 结束 // 轮询 `Processing.jobs` 或订阅 `JobChannel`」——两者都不可行，见本文 §7 |
| `series-layer-design.md` §3 的 `"schema": 1` | `Series` 结构体里没有这个字段，实际落盘没有它；`status` 的取值是 `pending` / `ready` / `done`，与文档写的 `pending` / `imported` / `done` 不符 |
| 本文未涉及 | `set_series_cover` / `rename_series_chapter` 仍然未实现；`cover` 字段无任何写入路径 |