# 翻译辅助方案审计

对象：`docs/series-layer-design.md` §5 预留的漫画级翻译资料（翻译指导与术语表）。
依据：漫画/章/页三层管理落地后的实际代码。

结论先行：**归属设计成立，注入方案需要修正。** 原文档说「只需把术语表填进 `instructions` 字段」，这句话在单部漫画下能跑通，在多部漫画下会互相污染，而且填进去之后会写进用户的全局设置文件。

上下文的派生、匹配与预算见 `docs/reference/koharu-glossary-design.md`。那份文档补充了本文未覆盖的部分，包括为什么「附加说明」是三个候选字段里唯一可达的那个。

---

## 1. 归属：漫画层，正确，不变

章是内核项目，导入新章时会被删除重建；术语表是跨章资产。放在漫画目录里，生命周期跟着作品走，与 `series.json` 同级。这条判断在新结构下依然成立，而且比放在章项目里更必要——`import_series_chapter` 已经证明章是可丢弃的产物。

索引里的 `SeriesSettings` 目前是空结构体，正好容纳资料引用。它还会承载条漫广告高度——那是导入参数而不是作品资料，同属漫画级但读取时机不同，见 `docs/reference/webtoon-ad-bands-design.md`。

## 2. 注入：`instructions` 是全局的，原方案不成立

### 2.1 事实链

| 环节 | 位置 | 事实 |
| --- | --- | --- |
| 字段 | `crates/koharu-pipeline/src/config.rs:180` | `TranslationConfig.instructions: Option<String>` |
| 读取 | `crates/koharu-pipeline/src/stages/translation.rs:57` | 翻译阶段从 `self.config.instructions` 取值 |
| 拼进 prompt | `crates/koharu-translator/src/prompt.rs:272-279` | 追加到 system prompt 结尾 |
| 配置来源 | `crates/koharu-app/src/commands/preferences.rs:23` | `PipelineConfig::load()`，全局单例，落盘 `~/.koharu/config.toml` |
| 请求结构 | `crates/koharu-pipeline/src/request.rs:65-70` | `Request` **没有** instructions 字段 |

关键点：`Request` 里没有 instructions，翻译阶段只认构造 `StageRunner` 时拿到的 config。所以**没有办法为单次作业单独传术语表**，除非改 `koharu-pipeline`——那是红线。

### 2.2 好消息：运行时替换是官方预留的能力

`Pipeline::from_config`（`crates/koharu-pipeline/src/pipeline.rs:30`）订阅配置变更并热替换 stage 运行器；而 `koharu-config` 提供了不落盘的配置句柄：

```82:88:crates/koharu-config/src/lib.rs
    /// Create a live configuration handle without file persistence.
    /// `save()` succeeds as a no-op, making this suitable for tests.
    #[must_use]
    pub fn memory(value: T) -> Self
```

两者都是公开 API，组合起来就能得到一条作用域受限的处理管线。

## 3. 修正后的注入方案

为每部漫画构造一条独立管线，批量开始前换上，结束后恢复：

```rust
// 1. 以当前配置为基线，用户改过的模型与阈值全部保留
let mut config = PipelineConfig::load()?.read()?.clone();
// 2. 把这部漫画的资料渲染成指令
config.translation.instructions = Some(render_assets(&series));
// 3. 内存配置，不写全局配置文件
let scoped = Config::memory(config);
// 4. 构造只服务这部漫画的管线并换上
handle.manage(Pipeline::from_config(
    scoped,
    koharu_translator::ProvidersConfig::load()?,
    device,
)?);
// 5. 逐章处理，全部结束后恢复
handle.manage(Pipeline::load(device)?);
```

相比原方案的好处：

- **零内核改动**：`from_config` 与 `Config::memory` 都是公开 API，`koharu-pipeline` 与 `koharu-scene` 一行不动
- **不污染用户设置**：`Config::memory` 的 `save()` 是空操作，用户的 `config.toml` 不会被写进术语表
- **按作品隔离**：两部漫画先后处理不会串味
- **保留用户设置**：以当前配置为基线，只覆盖 `instructions` 一个字段

需要接受的代价：

- 每部漫画开始时重建一次 `Translator` 与 `StageRunner`。批量是串行的，一次批量只付一次
- **重建 `Translator` 会导致本地模型权重重新加载**，因为新的翻译器不知道任何已加载模型。选用本地模型时这是一次完整的权重读盘，代价远高于「重建一个结构体」。批量入口按漫画维度触发，因此单次批量只付一次；但如果将来支持跨漫画批量，这条要重新评估
- 处理期间用户在设置里改配置不会影响正在跑的管线，因为内存配置不订阅文件变化。可接受：批量运行本就不该被中途改设置打断
- 崩溃或中断时全局 `Pipeline` 可能停留在带术语表的状态，重启应用即恢复，资料本身不受影响

## 4. 新结构带来的两个设计要求

### 4.1 入口必须在章节管理页

资料归属漫画层，入口就该在第二层（章节管理），而不是设置页（全局）。用户处理某部漫画时看到的应该是「Demo Title 的术语表」，而不是一份不知道属于谁的全局指令。

这也顺带解决隔离问题：编辑入口与批量处理入口在同一页，作用域自然一致。

### 4.2 角色卡不做

原方案里还有第三项：角色卡（人物的姓名、身份、性格、说话方式）。**已决定不做。**

理由是切片无法保证把说话人完整地留在同一页：气泡的尖尾可能落在上一页，人脸可能被刀口分开。**说话人归属本身就不可靠**，因此「谁用什么语气说话」这类资料没有可靠的锚点，注入进去只会占用预算而不产生约束力。

术语表不受这个理由影响：词条靠**文本匹配**定位，出现在待译片段里就注入，与说话人归属无关。这个区分是两项去留不同的原因。

若将来切片策略变化——例如允许相邻切片重叠到足以容纳任何被切断的气泡——角色卡可以重新评估。触发条件是「说话人归属变得可靠」。

## 5. 与官方未来的关系

资料是本地扩展。上游若实现同类功能，迁移路径是把 `glossary.json` 的词条转成官方结构——这也是为什么存储要用结构化词条而不是一段文本：文本无法反向解析回词条，结构化数据可以。

`docs/extending-koharu.md` §5.3 的要求依然成立：放弃本地实现、迁移数据，而不是让两套并存。

## 6. 实施顺序

1. `SeriesSettings` 加 `guidance` 与 `glossary` 两个字段（结构化，不落文件）
2. 读写命令：按漫画存取资料，`glossary.json` 落在漫画目录
3. 渲染函数：把资料转成指令文本，**纯函数、可单测**
4. 批量命令接上作用域管线：处理前换上，结束后恢复
5. 章节管理页的编辑界面（与广告带设置同处一个管理区）
6. 术语抽取辅助（从已有译文里发现候选词）——最后做，且只做建议不做自动写入
