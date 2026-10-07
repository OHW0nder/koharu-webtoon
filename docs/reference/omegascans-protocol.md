# OmegaScans 生肉源：站点协议

本文记录 OmegaScans（`omegascans.org`）的站点协议调研结果，以及 koharu 侧的实现取舍。

**这份文档是参考输入，不是规格。** 站点形态变了要改 `crates/koharu-source/src/omegascans/`，
并按 §6 重新对活站验证，而不是照着这里改。

上游参考实现是 Python 项目 `manga-auto-translator` 的 `manga_auto_translator/downloader/omega.py`
（该文件自述为 `testscript/download-omega.js` 的移植）。本文引用它只是为了标记「哪些结论来自
既有实现、哪些是本次重新验证的」，其做法本身不作为 koharu 的规范。

---

## 1. 目标与非目标

**目标**：把「站点上有哪些章、某一章有哪些页」这两件事变成结构化结果，交给编排层决定怎么落盘。

**非目标**：

- 不做重试与并发。那是编排层按站点承受度决定的事，协议层只负责一次请求一次结果。
- 不落任何元数据。原图地址、下载时间、章节 slug 都不保留——判据是章名，不是台账。
- 不做增量缓存。没有 ETag、没有页码游标，每次检查都全量重算。
- 不绕过付费墙。站点不公开的章就是拿不到，跳过并报出来。
- 不处理搜索。搜索接口带 `adult=true` 这类站点自有参数，本期不做，也不为它预设参数。

---

## 2. 站点协议

### 2.1 网络面

无官方文档，以下为逆向结果。全部条目于 **2026-10-07** 对活站复验过（§6）。

| 用途 | 地址 | 响应 |
| --- | --- | --- |
| 作品信息 | `GET https://api.omegascans.org/series/<slug>` | `{id, title, ...}` |
| 章节列表 | `GET https://api.omegascans.org/chapter/query?page=N&limit=12&series_id=<id>` | `{data: [...], meta: {last_page, ...}}` |
| 阅读页 | `GET https://omegascans.org/series/<slug>/<chapter_slug>` | HTML，内嵌 RSC payload |
| 图片 | payload 里的 `media.omegascans.org` 绝对地址 | `image/jpeg` 等 |

列表接口条目形如 `{"chapter_name": "Chapter 36", "chapter_slug": "chapter-36", "price": "0"}`。
`price` 未被消费：站点不公开的章表现为图片清单为空，不需要靠 `price` 判断。

站点查询参数是 HeanCms 模板的形状（`status=All&order=desc&orderBy=total_views&series_type=Comic`）。

### 2.2 反爬

**没有。** 上游实现只发一条 `User-Agent: Mozilla/5.0`，无 Cookie、无 Referer、无 token、
无浏览器自动化；实测图片 CDN 直连即可取到（4.9 MB / `image/jpeg`，HTTP 200）。

**有意偏离**：koharu 侧不用共享 HTTP 策略（`koharu-runtime/src/network.rs` 的 UA 是
`koharu/<版本>`，用途是标识自己），而是自己建客户端并装浏览器 UA。**这一层是最可能先坏的地方**：
站点一旦上 Cloudflare，全线失败且没有降级路径。

### 2.3 图片清单藏在阅读页里

这是整套实现唯一跟着 markup 走的地方。payload 结构（已转义状态，站点把引号转义了一层）：

```
self.__next_f.push([1,"...{\"API_Response\":{\"chapter\":{\"id\":14680,...,\"chapter_name\":\"Chapter 36\",
  \"chapter_data\":{\"images\":[\"https://media.omegascans.org/.../01-....jpg\", ... ]}}"])
```

两个必须知道的陷阱：

1. **`chapter_data` 出现两次。** 第一次是 React 流式的延迟引用，形如 `\"chapter_data\":\"$29\"`，
   指向后面真正的 chunk。所以提取标记必须带 `{\"images\":[` 而不只是 `chapter_data`。
2. **清单数组是跨 chunk 连续的一段文本。** 按第一个 `]` 截断即可（上游同样如此）；
   URL 里出现 `]` 会让提取提前结束并报「清单不是合法 JSON」。

`chapter_name` 也在这里，但**不采用**：章节列表接口一次就给全章的名字，为此多请求 N 个阅读页
不划算，而且落盘要用的名字来自列表。

### 2.4 站点侧已知的数据缺陷

| 现象 | 表现 | 处理 |
| --- | --- | --- |
| 地址被截断 | 清单里出现过只到 `htt` 的条目 | 提取时滤掉并报数量；数量突然变多说明站点在改数据格式 |
| 章尚未公开 | 清单字段存在但为空数组 | 合法结果，跳过并报出来；与「markup 变了」严格区分 |
| 页数很少 | 实测有整章只有 7 页 | 正常，不必当异常 |
| 响应是错误页 | HTTP 200 但体积很小或不是 JSON | 体积下限 + JSON 解析各自兜住 |

---

## 3. 增量判定

**判据是章名，没有别的。**

上游那份实现也是纯目录 diff：远端章名归一化后，本地没有同名目录就算缺失。koharu 侧判据换成
`SeriesChapter.title`——它本来就等于来源目录名，`import_series_chapter` 也已经用它查重
（`crates/koharu-app/src/commands/series.rs`）。所以不需要新增台账字段，删掉重下一次也不会串。

代价与上游一致：

- 用户在 koharu 里删掉某章后，再检查会把它当缺失重新下载。这是期望行为。
- 不校验页数完整性。上游「目录里有 ≥1 张图就算完成」的判据没有被采用：koharu 的判据是章名，
  而章名要么在索引里要么不在，不存在半拉子的一章。

**章名归一化是跨路径一致性的前提**：用户手动存下来的文件夹通常也叫 `Chapter 36`，与自动下载
归一之后得到同一个 `title`，两条路径不会把同一章记成两章。

---

## 4. 与上游实现的有意分歧

| 上游做法 | koharu 侧 | 原因 |
| --- | --- | --- |
| 落盘保留原图 `RAW/Chapter N/*.jpg` | 临时目录 → 导入 → 删除 | koharu 不在磁盘上保留生肉，图片进内容寻址 blob。保留原图是双重存储 |
| `.tmp-<章名>` 全部页成功才 rename | 不需要 | 原图不留盘，判据是索引里的章名而非目录。`TempDir` drop 即回滚，原子性免费 |
| 章目录直接命名，图片落在章目录里 | 临时父目录 + 一层章目录 | `collect_importable` 是**递归**的，平级两个章目录会被一起吞进同一章。因此章与章必须串行，只有章内页并发 |
| 认 `gif` / `avif` 扩展名 | 只认 png / jpg / jpeg / webp | koharu 的 `Format` 只认这四种，落成别的扩展名会被 `collect_importable` 静默丢掉，表现为「这章导入进来是空的」 |
| 目录 diff 作为已下载判据 | `SeriesChapter.title` | 见 §3 |
| `SUPPORTED_SITES = {"omegascans"}` 中央白名单 | 无白名单，`SeriesSite` 枚举 + 每站点一个模块 | 站点默认值属于拥有它的模块，不属于中央列表 |
| 并发 4 / 章间 sleep 500 ms / 重试 3 次，硬编码且无配置口 | 编排层的默认值，站点常量只留协议相关的 | 上游把这些固化在默认参数里，`service.py` 只传三个位置参数，因此实际上不可配 |
| 两条正则 | `str::find` 固定标记 | 标记是字面量不是模式；为一个 crate 引入正则引擎不划算，且 Rust regex 无回溯 |
| 整页 `html.replace('\\"', '"')` 后再匹配 | 只还原清单那一小段 | 整页有几十 KB 与清单无关的内容，全页解转义只会引入误伤 |
| 下载任务可暂停但无法恢复（死代码） | 章边界检查取消 | 见 `docs/series-management-design.md` 的批处理约定 |

---

## 5. 已知风险

1. **UA 与反爬**。见 §2.2。这是唯一没有降级路径的失败模式。
2. **RSC 提取是单点**。站点改 markup 就失效。护栏是
   `crates/koharu-source/src/omegascans/testdata/chapter-page.rsc`——一份裁剪过的真实抓取，
   改动后先跑 `cargo test -p koharu-source`。样本里的上传路径与文件名已替换成占位。
3. **没有降级的分页**。`meta.last_page` 缺失时报错而不是继续翻，避免变成不知何时停止的循环。
4. **站点页面体积**。阅读页约 84 KB，一次一个，没有批量接口。

---

## 6. 重新验证

站点形态变化后，按下面几条重跑一遍，再改 `crates/koharu-source/src/omegascans/`：

```powershell
# 1) 作品信息：有 id 与 title
Invoke-RestMethod 'https://api.omegascans.org/series/love-quest'

# 2) 章节列表：有 data 数组与 meta.last_page，条目有 chapter_name / chapter_slug
Invoke-RestMethod 'https://api.omegascans.org/chapter/query?page=1&limit=12&series_id=758'

# 3) 阅读页：两个标记仍然命中，图片清单非空。
#    这里用的是 parse.rs 里那两个标记，不是上游的正则——验的是代码实际依赖的东西。
$r = Invoke-WebRequest 'https://omegascans.org/series/love-quest/chapter-36' -UseBasicParsing
$h = $r.Content
$h.Contains('\"chapter_name\":\"')
$start = $h.IndexOf('\"chapter_data\":{\"images\":[')
if ($start -lt 0) { throw 'images marker missing' }
$h.Substring($start + 26, 200)

# 4) 图片直链：无 Referer 可取
```

第3 步若不再命中，先确认 §2.3 的两个陷阱（`chapter_data` 出现两次、清单跨 chunk）是否变了，
再改 `parse.rs` 的标记，并更新 `testdata/chapter-page.rsc`。