# 条漫支持：设计与实测记录

Koharu 支持超高长图（webtoon / 条漫）的设计与实测记录。阅读顺序建议：先看 [1 为什么必须切页](#1-为什么必须切页)，再看 [3 输入几何的实测结论](#3-输入几何的实测结论)，最后看 [5 从 XianScan 借了什么](#5-从-xianscan-借了什么)。

外部项目的完整调查见 [`xianscan-webtoon-pipeline.md`](./xianscan-webtoon-pipeline.md)。本文只记录 Koharu 侧的设计决定与实测数据。

---

## 1. 为什么必须切页

检测模型 RF-DETR Seg 2XL 的 backbone 断言固定 1152×1152 输入，因此任何页面几何都必须映射到这个正方形。映射方式决定了 checkpoint 看到的输入分布。

条漫是超高纵向长图。实测验收样本（Demo Title Chapter 24）：

| 文件 | 尺寸 | 长宽比 |
| --- | --- | --- |
| `001.jpg` | 720×14317 | 19.9:1 |
| `002.jpg` | 720×13745 | 19.1:1 |
| `003.jpg` | 720×13375 | 18.6:1 |

直接送入检测器，纵向压缩到 `1152 / 14317 = 0.080`。一个 40px 高的对白框进模型后只剩 **3.2px**，低于 DINOv2 patch 的 12px 下限。

**实测基线（不切页，3 张原图）：**

| 输入 | letterbox | stretch |
| --- | --- | --- |
| `001.jpg` | text 0 / bubble 0 / panel 0 | text 8 / **bubble 0** / panel 3 |
| `002.jpg` | text 0 / bubble 0 / panel 0 | text 7 / **bubble 0** / panel 2 |
| `003.jpg` | text 0 / bubble 0 / panel 0 | text 6 / **bubble 0** / panel 2 |

letterbox 下内容被压成 58×1152 的竖条，模型彻底失效。stretch 勉强挤出少量 text，但 **bubble 一个都检不出**——气泡检不出意味着 `panel → bubble → text` 层级与气泡归属判定全部失效，翻译流程无法建立对话结构。

**结论：不切页，条漫检测不可用。** 这不是精度问题，是可用性问题。

---

## 2. 切页设计

切页发生在**导入期**，一次性完成。切出的每一片成为普通 `Page`，走完全相同的下游流程——检测、OCR、翻译、嵌字四个阶段**零改动**，因为它们本来就以单 `Page` 为单位。

### 2.1 为什么不做成 pipeline stage

`StageProcessor::process` 只能修改 `input.page` 指向的那一页。造新页需要 `Edit::add_page`，它会改写 `page_order` 并 bump `page_order_epoch`，而所有 pipeline patch 只 `observe_subtree(page)`，没有 observe 项目层级。让 pipeline 改页序会与并发页的 patch 冲突。

切页因此属于导入期变换，与"打开文件对话框 → 解码 → 建页挂 asset"同属一个用户动作。

### 2.2 规划器

实现位于 `crates/koharu-ml/src/webtoon/`，是纯函数，不依赖模型与 IO：

```
plan_slices(width, height, row_profile, params) -> Option<SlicePlan>
```

`None` 表示不需要切页。参数默认值（`SliceParams::default`）：

| 参数 | 值 | 含义 |
| --- | --- | --- |
| `target_height` | 1600 | 理想页高 |
| `min_height` | 1200 | 允许的最小页高 |
| `max_height` | 2400 | 允许的最大页高 |
| `trigger_aspect` | 3.0 | 长宽比超过此值才视为条漫 |
| `min_sliceable_height` | 2400 | 低于此高度不切，避免切出 1 页 + 1 碎片 |
| `airspace_px` | 3 | 切点上下各需连续留白行数 |

选 1600 的依据：`1152 / 1600 = 0.72`，40px 文字进模型后为 29px，是 patch 阈值的 2.4 倍。切到 2400 则为 19px（1.6 倍），切到 1200 为 38px 但页数翻倍。

### 2.3 上下净空校验是硬要求

一个 y 位置可作为切点，仅当 `[y - airspace_px, y + airspace_px]` 区间内所有行都是留白行。

**单凭"某行平坦"不足以证明可切**——未检出的气泡内部，两行文字之间的空隙同样是平坦的。必须上下都有净空才能排除这种情况。

### 2.4 三级递进选点

1. 留白带（近均匀且足够宽，且上下净空通过），评分 `band_width * 2.5 - |band_center - ideal| * 0.05`
2. 窗口内最平坦行兜底
3. 强制前进到 `max(min_height / 2, 64)`，保证不死循环

### 2.5 实测结果

| 文件 | 切出 | 页高范围 | 最小净空 |
| --- | --- | --- | --- |
| `001.jpg` | 9 | 1200–1877 | 58px |
| `002.jpg` | 9 | 1200–2289 | 6px |
| `003.jpg` | 9 | 1200–2385 | 60px |

27 个切点全部落在留白区内，页高总和精确等于原图高。最小净空 6px 出现在 `002.jpg` 的 y=12400（气泡描边下方 6 行），只是余量偏小，不切断内容。

### 2.6 导入期的并行分解

**判据是"一张源图能不能吃满核"，不是"一章有几张图"。** 早先的实现按源图 `into_par_iter`，于是
扇出等于章里的图数——而一章可能只有一张长条，切出十片，整条导入就跑在一个核上。这个判据不可用。

真正决定形态的是：**哪些工作彼此独立**。

| 阶段 | 可并行 | 依据 |
| --- | --- | --- |
| 每片的裁剪 + 重编码 | 是 | 切片之间没有任何依赖 |
| 行剖面 | 是 | 梯度项在每行开头重新起算，行与行互不参照 |
| 解码 | 否 | JPEG 必须整趟 IDCT，`zune-jpeg` 没有按 MCU 分块的接口 |

实测（724×15120 的合成条带，切成 7 片，16 核）：

| 阶段 | 串行 | 并行 |
| --- | --- | --- |
| 行剖面 | 255–260 ms | 19–20 ms |
| 裁剪 + 重编码 | 1709 ms | 270–282 ms |
| 整条导入（无损源） | 7633 ms | 1749 ms |
| 整条导入（有损源） | 7350 ms | 420 ms |

裁剪与重编码在并行之后交错执行，因此不再分开计时——分开记已经没有意义。

### 2.7 真实数据上的整部导入

10 章连载，114 张条漫长条（720 宽 × 12690–16712 高，长宽比 18.3–24.2，278.2 MiB），
切成 830 片。章间串行、章内并行，与 `build_chapter_projects` 一致。

| | 值 |
| --- | --- |
| 导入合计 | **19.1 s**（平均 1.9 s/章，最慢 2.7 s） |
| 平均占用核数 | **2.48 / 16** |
| 峰值核数 | 3.46 |
| 峰值工作集 | 685 MiB |
| 存储 | 278.2 MiB → 180.7 MiB 切片 |

切片可用性全部通过，0 张图未被切页：页高都落在 `[1200, 2400]` 内（末页余数除外）、切片首尾相
接精确铺满源图、宽度与源图一致，抽查解码的切片不是黑页。

**CPU 只用到 16 核里的 2.5 核这件事仍然没有解释清楚。** 一章有 9–14 张长条，理论上并行得更宽，
峰值却不到 3.5 核。怀疑是读盘成了瓶颈（解码在等 I/O），但未经验证。

### 2.8 dev profile 曾让解码耗时失真 49倍

上面所有数字都是补齐codec crate 的 opt-level 之后测的。补之前，同一条 724×15120 的 JPEG
解码要 **3950 ms**，补之后是 **79.9 ms**——差49 倍。

原因：`Cargo.toml` 的 `[profile.dev.package.*]` 早就把 `image` / `image-webp` / `webp` /
`libwebp-sys` 挑出来单独优化，但 `zune-jpeg`（JPEG 编解码）、`miniz_oxide` 与 `fdeflate`
（PNG deflate/inflate）、`rgb`（通道转换）漏在外面，于是 dev profile 下量到的解码与重编码时间
整体虚高。

**这个失真曾经把优化方向带偏**：未修复时解码占单条导入的 83%~88%，看起来必须换解码器
（mozjpeg 之类）；修好之后 JPEG 解码只占 19%，而重编码变成最大的一段。**所以任何基于 dev
profile 的耗时结论，在 codec crate 未被优化时都不成立。**

顺带记一笔：改 `[profile.dev.package.*]` 会让 `cef-dll-sys` 的构建脚本指纹失效并要求从源码
构建 CEF，需要 `cmake` 与 `ninja` 在 PATH 上。

**剩下的瓶颈是解码**，占并行化后整条导入的 83%（无损源）到 88%（有损源）。要再往下走就必须换解码
器，而解码发生在两处：导入侧（`cut_one`，本地代码）与检测流水线每轮重解码切片（上游代码）。
后者的量级更大。

---

## 3. 输入几何的实测结论

这是本轮最重要的结论，且**与理论预期相反**。

### 3.1 背景：理论预期是 stretch

`processor.rs` 原有注释指出训练时用的是 Albumentations 的 bilinear resize，而 Albumentations 的 `A.Resize` 默认拉伸不填充。若如此，stretch 才是 checkpoint 的原生分布，letterbox 属于分布偏移。

### 3.2 实测：letterbox 在关键类别上更优

对 27 张切页各跑一次两种策略，统计同类匹配框（IoU ≥ 0.5）的置信度差：

| 类别 | 平均差（letterbox − stretch） | letterbox 更高占比 |
| --- | --- | --- |
| text | **+0.047** | **39/53 (74%)** |
| bubble | **+0.040** | **24/29 (83%)** |
| onomatopoeia | +0.091 | 12/15 (80%) |
| **panel** | **−0.051** | **5/19 (26%)** |

检出总数：letterbox 152（text 64 / bubble 32 / onomatopoeia 23 / panel 33），stretch 130（62 / 29 / 16 / 23）。

**结论不是一边倒**：text、bubble、onomatopoeia 三类 letterbox 明显更优，而 **panel 是 stretch 更好**。

目视核验举例（`003_006.png`）：

| 目标 | letterbox | stretch |
| --- | --- | --- |
| "OH, IT'S SOOHYUN-NIM." | text **0.85** | text 0.76 |
| 粉色旁白框 "HELLO." | text **0.91** | onomatopoeia 0.21（误分类） |
| "WHY HE HERE?" | text **0.61** | text 0.33 |
| "WHY SHE HERE?" | bubble + text 0.53 | 仅 text 0.40，漏了气泡 |

text 框中心落在任何 bubble 外的比例：letterbox 45.3%，stretch 50.0%。

### 3.3 决定

**默认 letterbox。** text 与 bubble 直接决定翻译质量，这两类上 letterbox 优势明确；panel 的劣势可接受，因为 panel 参与层级构建用的是 mask 包含度阈值而非分类分数。

代价必须记录：letterbox 下 panel 检出置信度下降约 0.05。若将来 panel 参与更敏感的逻辑，需要重新评估这个取舍。

### 3.4 这个结果的解释价值

**避免 2.8:1 的各向异性畸变，比匹配训练分布更重要。** stretch 把 720×1600 映射成横向 ×1.60、纵向 ×0.576，文字被沿水平方向拉伸；letterbox 用各向同性 0.72 缩放后居中 padding（内容 518×1152），字形比例得以保持。

推论：模型对各向异性畸变的敏感度，高于对整体尺度分布偏移的敏感度。这条经验对后续接入其它固定方形输入的检测器同样适用。

### 3.5 本次测量的限制

- 只有 1 章 27 页，**没有 ground truth**，因此无法计算 precision / recall。结论建立在"同对象置信度对比 + 目视核验 + 相对基线"三条间接证据上。**置信度高不等于更准。**
- 未覆盖其它画风（不同描边风格、不同气泡形状）与其它语言。需要更多样本才能确认这是普适结论还是本样本的偶然。

---

## 4. 数据模型

切页需要在场景里表达"这一页是某张原图的一段"。这个记录是 `PageSlice` 组件，承载 `y_offset`、`slice_height` 与源图的 `BlobId`。

**它不是 `koharu-scene` 的内置组件**，而是 `koharu-app` 里以 `dev.koharu.webtoon.page.slice` 为 kind 声明的自定义组件。内核对未注册的组件 kind 兜底放行，并保留其 schema 版本、引用与指纹，所以官方版本加载本项目不会失败，切片数据也不会丢失。这样内核相对上游只剩 `Assets` 的一处可见性提升——场景状态结构、组件注册表、关系表与导出列表都与上游逐字一致。扩展的形态约束见 `docs/extending-koharu.md`。

溯源信息存 `BlobId` 而非源页 `EntityId`：源页会被用户删除，存实体 ID 会悬空；存 blob 则与实体生命周期解耦。代价是无法从某页反查同源的兄弟页。

源图的字节要进入项目存储、但不需要任何页面显示它。做法是借一个临时载体页挂上资产再立刻删除：内容寻址的 blob 身份可以预先算出，切片的 `blob_refs()` 会把它钉住，因此删页不会让垃圾回收把它带走。**载体页必须先于切片建立**，否则切片组件的 `contains_blob` 校验会认为源图尚不存在。

`slice-of` 跨页关系没有实现。切片刻意锚定 blob 而非实体，所以该关系能回答的"哪一页是源图"在源页被删之后必然失效；做成瞬态关系只多一个需要维护的组件，收益不足。

上游导入命令不参与这件事，也都不再暴露给界面：`import` 与 `import_webtoon` 已随编辑器侧的导入入口一起删除，导入只从漫画柜走（`import_series` / `import_series_chapter`，见 `docs/series-management-design.md` §1）。切割因此只发生在漫画导入路径里，而那条路径在导入前由用户选定章节形态，条漫必然切割、页漫必然不切。

---

## 5. 从 XianScan 借了什么

### 5.1 采纳

| 做法 | 理由 |
| --- | --- |
| 切页优先于切块检测 | 见 §1，长图问题只能在这一层解决 |
| 上下净空校验 | §2.3，真实数据上验证过必要性 |
| 三级递进选点 + 严格前进守卫 | 防止病态输入下死循环 |
| 切点避开文本禁区 | §2.3 的净空校验是它的轻量 CPU 版本 |

XianScan 原本用轻量 OCR 模型建立"禁切文本区"再选切点。Koharu 第一版只用行画像的留白带，因为验收样本的面板间留白足够宽（留白行占 27%–34%）。**密集排版的条漫可能需要补上 OCR 禁区**，这是已知的未覆盖场景。

### 5.2 未采纳

| XianScan 的做法 | 不采纳的原因 |
| --- | --- |
| 检测器切块（tiling） | 它自己的 A/B 结论是净收益为负（77 个样本：+11 框 / −8 框，耗时 2–3×）。切页已经把问题消解，不再需要第二层机制 |
| 5 盒分离（`box` / `ocr_box` / `inpaint_box` / `typeset_box` / `carrier_box`） | Koharu 已有更好的 `panel → bubble → text` 层级与 mask 包含度归属判定，比扁平 region 表更结构化。真正缺的是擦除框与排版框从文本框分离，不是全部五个 |
| 多档位 OCR 检测尺寸阶梯 | XianScan 自己收敛掉了（960/1500/2000 三档在边界处让有效文字尺度跳变 33%）。Koharu 未走这条路，无需回退 |
| 遮罩类回填的多道统计闸门 | Koharu 的纯色回填更简单，尚未在真实数据上暴露问题 |
| 切片重编码用 WebP | 编码收益相近，但下游要读这些切片：`image` crate 的 WebP 解码（`image-webp`，纯 Rust）明显慢于 JPEG 的 `zune-jpeg`，而检测流水线每轮都要重解码全部切片。把体积上的收益换成解码上的损失不划算 |

### 5.3 切片重编码：为什么统一用 JPEG q92

切片一律重编码为 JPEG q92，**与源格式无关**，包括无损的 PNG 源。原因是源图已经完整保存在项目里
（§6 的 `PageSlice.source` 钉住它的 blob），切片是派生数据——对已经压缩过的像素再做一次无损重压
缩，买到的只是更大的字节和更长的等待。

同一批裁片上的实测（724×15120，切成 7 片，dev profile）：

| 切片编码 | 耗时 | 产物 |
| --- | --- | --- |
| PNG 无损（仅无损源走这条） | 5883 ms | 17.6 MiB |
| JPEG q92（现在统一走这条） | 1866 ms | 4.8 MiB |

整条导入从 7.6 s 降到 3.8 s，产物从 17.6 MiB 降到 4.8 MiB。换来的是切片不再与源图逐位相同，
但切片本来就只是给人看的派生数据，检测器读到的是同一段内容的重述。

带透明度的源图先合成到白纸再编码：JPEG 没有 alpha 通道，直接丢会把透明区域变成文件里存的那几个
RGB 值，通常是黑的，于是白纸变黑纸。

### 5.4 Koharu 强于 XianScan 的地方（未改动，作为对照）

调查中发现 Koharu 在若干方面本就更好，借鉴时没有替换它们：

| 能力 | Koharu | XianScan |
| --- | --- | --- |
| CJK 断行 | jieba 词保护 + ICU UAX#14 | 纯逐字 |
| 竖排 | 真竖排（OpenType `vert` / `vrt2` + 标点居中） | 字段传到渲染层但从未被读取 |
| 气泡排版 | 逐行采样气泡轮廓 | 只在矩形内排版 |
| 斜角估计 | 181 角度投影搜索 + 旋转卡壳 | 行角度中位数 |
| 擦除分块 | 纯函数规划器 + 单元测试 | 简单连通域切分 |

---

## 6. 已知的限制

| 限制 | 说明 |
| --- | --- |
| 密集排版条漫未验证 | §5.1，当前只靠行留白。面板紧贴、无 gutter 的条漫可能切错 |
| 切页阈值未经用户验证 | `trigger_aspect = 3.0` 与 `min_sliceable_height = 2400` 是按常理定的，未在多样本上验证 |
| 无 ground truth | §3.5，无法算 precision / recall |
| panel 置信度下降 | §3.3，letterbox 默认的已知代价 |
| 拟声词不参与下游 | 模型可检出 `onomatopoeia`，但区域类型映射未覆盖它，会落入 unknown 被丢弃。这是**有意决策**：不 OCR、不擦除、不翻译。基线显示该类 AP 仅 0.443（对比 text 0.878、bubble 0.909、panel 0.957），是四类中最弱的 |
| 只能按条漫导入，不能重新切页 | 条漫在导入期一次切完，没有"对已导入的长图重新切一遍"的入口。这是有意的取舍：源图 blob 仍然保留在项目里，重新切割的算法也还在 `koharu-ml::webtoon`，缺的只是一个按新边界重切的命令 |
| 章节存储约 2 倍 | 导入时未切原图也作为 patch attachment 存入项目，各切片的 `blob_refs()` 共同钉住它。这是 `koharu-storage` 的 `blobs.persist()` 要求 lease 内每个 blob 已落盘的结果，不是免费的软引用。源图字节因此在项目里多占一份，但没有页面显示它。这条对有损源仍然成立（实测 4.5 MiB 源 + 4.7 MiB 切片）；无损源曾经更糟，切片本身比源还大，§5.3 统一成 JPEG q92 后绝对体积下降了 |
| 条漫只能整章导入 | 条漫在导入期一次切完，没有"对已导入的长图重新切一遍"的入口。这是有意的取舍：源图 blob 仍然保留在项目里，重新切割的算法也还在 `koharu-ml::webtoon`，缺的只是一个按新边界重切的命令。导入入口收敛到漫画柜之后，单张长图也不再是一个可选项——它只能是某一章源目录里的一页 |

### 6.1 本地验证命令

`DOCS_RS=1` 让 `gobject-sys` / `gio-sys` 跳过构建脚本，从而在没有 GTK 的机器上完成类型检查与单元测试（只有链接仍需要 GTK）：

```bash
DOCS_RS=1 cargo check -p koharu-app
DOCS_RS=1 cargo test  -p koharu-app --lib
```

---

## 7. 复现

```bash
# 切页
cargo run -p koharu-ml --bin slice_webtoon -- <input.jpg> <out-dir>

# 单页检测
cargo run -p koharu-ml --bin koharu_layout_rfdetr_seg_2xl -- \
  --input <page.png> --input-fit letter_box

# 批量检测（模型只加载一次）
cargo run -p koharu-ml --bin koharu_layout_rfdetr_seg_2xl -- \
  --input-dir <pages-dir> --output-dir <out-dir> --input-fit letter_box

# 两种策略对照
diff -r <out-letter-box> <out-stretch>
```

模型权重从 `mayocream/koharu-layout-rfdetr-seg-2xl-1152` 下载，revision 钉死在 `koharu_layout_rfdetr_seg_2xl/mod.rs`。该仓库的 `validation_metrics.json` 记录了 checkpoint 在 Manga109 验证集上的基线，可作为分布差异的参照。
