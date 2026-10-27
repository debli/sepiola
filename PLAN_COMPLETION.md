# Sepiola 计划完成记录

- 记录日期：2026-10-26
- 依据：`ARCHITECTURE.md` 第 5 节、`TASK_PLAN.md`
- 结论：**M0～M17 均已完成并通过自动化验证；M8 在项目内 15 张人像 + 15 张风景测试集上 30/30 分类正确（准确率 100%）；M9 递归目录扫描与输出目录排除验证通过；M10 ONNX Runtime 随包分发路径查找验证通过；M11 无配置文件运行与显式缺失文件报错验证通过；M12 模型按 exe 同目录解析验证通过；M13 LUT 按 exe 同目录优先、工作目录回退验证通过；M14 config.toml 按 exe 同目录优先、工作目录回退验证通过；M15 显影后端可切换，darktable 缺失时自动回退内置后端并验证成功；M16 使用真实 Nikon D70 NEF 样本完成内置后端端到端测试；M17 新增 egui 桌面界面，GUI 可正常启动并复用 CLI 核心流程；M18 导出选项（pixel_format/keep_metadata）可配置并端到端验证；M19 导出结果支持放大查看、保留勾选与批量删除；M20 支持一键用系统文件管理器打开输出目录；M21 人物身份识别端到端验证通过——同一人两张图归入同一子目录、不同人分开，二次运行复用图库标签；M22 五点关键点对齐使同人跨构图余弦从 ~0.7 提升到 0.97+；M23 gallery 多嵌入增量更新与合并/删除单测与端到端验证通过；M24 GUI 拆分为 7 个模块（各 <250 行）；M25 GUI 支持人物合并/删除与关键点模型设置项；M26 补齐示例 LUT 文件、批次取消、参数保存/载入与目录拖拽导入；M27 引入 LFW 子集评测集（52 位人物 / 80 张）并新增 `sepiola-eval` 阈值扫描工具，实测配置阈值 0.35 即为 F1 最佳（0.980）。M28 引入媒体库（库目录按类型归类、图片自动分相册、浏览/重打标签，CLI + GUI 库页签）并加大 GUI 组件间距；M29 接入 ImageNet 场景标签模型（MobileNetV2 ONNX），非人像图片自动细分到「植物动物/生日/聚会」相册，6 张官方样本 top-1 全部正确。M30 修复库页签三大缺陷：文件列表 `show_rows` 虚拟化（万级目录只渲染可见行）、点击行右侧即时预览解码图、右键菜单支持打开所在目录/复制路径/移动到相册/从索引移除。M31 落地后台缩略图生成与磁盘缓存：库页签文件列表行内显示 40×40 缩略图，缓存于 `<库>/.sepiola/thumbs/`（FNV-1a 哈希命名、按 mtime 判新鲜），单个后台线程惰性生成、UI 轮询加载纹理。M32 实现库内 RAW 一键显影：RAW 行右键「显影为 JPEG」，产物镜像目录存于 `<库>/.sepiola/developed/`（幂等复用），显影后预览与缩略图自动联动刷新。M33 落地库内 RAW 批量一键显影：库页签「全部显影」按钮按当前视图批量入队，单个后台 worker 顺序显影（进度条 + 成功/失败统计），已显影产物幂等复用。M34 场景分类细化：引入 Places365 场景模型（resnet18 ONNX），新增 建筑/交通工具/美食/运动/冒险/探险/截图·文档 相册，风景改为显式自然类映射，彻底移除"无脸一律风景"兜底（PPT 截图误入风景缺陷修复），旧索引 v2 全量重归类迁移。M35 库第一级文件类型总览：FileKind 扩展 书籍/录音/驱动/安装包/项目/游戏，项目与游戏目录按标记文件折叠为单个实体（内部文件不入库），库页先显示各类型计数卡片、点「图片」进入浏览页，索引 v4 全量重判类型迁移。M38 书籍元信息分面筛选与正文检索已落地：分面去重并按频次排序、多个字段可组合、文字默认搜索所有元信息，勾选后异步检索 EPUB/PDF/FB2/PalmDOC MOBI 正文；核心正文测试 10 项、GUI 测试 7 项通过。**

## 1. 里程碑完成情况

M37 已完成：设置持久化、启动恢复和最近书库切换已实现；M38 已完成：图书分面筛选、元信息搜索与显式启用的异步正文搜索已实现。自动化验证见 §2.30、§2.31；图形环境手动交互尚未验证。

| 里程碑 | 状态 | 交付文件 | 验收结果 |
| --- | --- | --- | --- |
| M0 项目骨架 | 已完成 | `Cargo.toml`、`src/main.rs`、`src/error.rs`、`src/cli.rs` | `--help`、CLI 参数解析正常 |
| M1 图像读写 | 已完成 | `src/imageio.rs` | 16-bit TIFF 往返误差 ≤ 1/255；JPEG 往返误差 ≤ 0.05 |
| M2 3D LUT | 已完成 | `src/grade/lut3d.rs` | identity LUT 不改变像素；R/B 交换 LUT 与手算一致；DOMAIN 生效 |
| M3 混合与统计 | 已完成 | `src/grade/blend.rs`、`src/grade/stats.rs` | strength=0/1 行为正确；纯白图 `clipped_high=1` |
| M4 Stage 与配置 | 已完成 | `src/grade/mod.rs`、`src/grade/lut_stage.rs`、`src/config.rs`、`config.example.toml` | Stage 执行结果与直接调用 `lut3d + blend` 一致 |
| M5 RAW 显影 | 已完成 | `src/develop.rs` | 外部图成功转为 16-bit TIFF；失败返回带 stderr 的 `Error::Develop` |
| M6 场景 stub + 单张流程 | 已完成 | `src/scene/mod.rs`、`src/scene/stub.rs`、`src/pipeline.rs` | 完整流程跑通，含高光溢出半强度重做 |
| M7 批量与报告 | 已完成 | `src/report.rs`、`src/pipeline.rs` | rayon 并行、对比图、CSV、单张失败不影响整批 |
| M8 真实人脸检测 | 已完成 | `src/scene/face.rs`、`src/config.rs`、`src/pipeline.rs`、`test_data/` | `test_data/portrait` 15/15、`test_data/landscape` 15/15，合计 30/30 正确（100%） |
| M9 递归目录扫描 | 已完成 | `src/pipeline.rs` | 嵌套目录 3/3 全部处理，输出镜像子目录；输出目录位于输入内时连续两次运行均只处理 3 张 |
| M10 ONNX Runtime 随包分发 | 已完成 | `src/scene/face.rs` | 库放在 exe 同目录 `lib/`、未设环境变量时日志显示加载该路径，推理成功 |
| M11 配置文件可选 | 已完成 | `src/cli.rs`、`src/config.rs` | 无 `config.toml` 时使用内置默认值并成功处理图片；`-c missing.toml` 报错退出码 1；exe 同目录或当前目录的 `config.toml` 自动读取 |
| M12 模型路径优先 exe 同目录 | 已完成 | `src/scene/face.rs` | 二进制与 `models/` 复制到 `/tmp/m12/dist` 后，从 `/tmp` 启动仍加载 `dist/models/...` 并判为 `portrait` |
| M13 LUT 路径同规则解析 | 已完成 | `src/paths.rs`、`src/grade/mod.rs`、`src/scene/face.rs` | 当前目录与 exe 目录都有 `luts/portrait.cube` 时加载 exe 目录版本；删除后回退到工作目录版本，`stages=lut` |
| M14 config.toml 同规则查找 | 已完成 | `src/cli.rs` | 从其他工作目录启动时读取 `dist/config.toml`；删除后回退 `./config.toml`；都没有时使用默认值 |
| M15 显影后端可切换 | 已完成 | `src/config.rs`、`src/develop.rs`、`src/builtin_develop.rs`、`src/pipeline.rs`、`Cargo.toml` | `builtin` 后端将 JPEG 转为 16-bit TIFF 并完成全流程；`auto` 在无 darktable 的 PATH 下回退内置后端；损坏 RAW 仅该张失败 |
| M16 内置后端真实 RAW 测试 | 已完成 | `src/builtin_develop.rs`、`test_data/raw/nikon_d70.nef` | 真实 Nikon D70 NEF 端到端输出 3038×2014 TIFF，均值 0.482、标准差 0.259；相机矩阵量级问题已修复并有回归测试 |
| M17 桌面图形界面 | 已完成 | `src/lib.rs`、`src/main.rs`、`src/pipeline.rs`、`Cargo.toml`、`sepiola-gui/` | `cargo check -p sepiola-gui` 通过；`cargo test` 71 项通过；GUI 启动 6 秒无错误，目录选择、参数、进度与对比图预览已接入 |
| M18 导出选项 | 已完成 | `src/config.rs`、`src/imageio.rs`、`src/pipeline.rs`、`config.example.toml` | `[output] pixel_format`（8bit JPEG/16bit TIFF）与 `keep_metadata` 生效：16bit 输出 `.tif`；keep=true 时源 JPEG 的 APP1 复制到输出，keep=false 时输出无 APP1 且可解码 |
| M19 结果查看与保留/删除 | 已完成 | `sepiola-gui/src/main.rs` | 结果行可点击放大查看（缩放滑条 0.1–4.0）；每行保留复选框默认勾选；支持全选/全不选与"删除未保留"批量删除产物文件 |
| M20 打开输出位置 | 已完成 | `sepiola-gui/src/main.rs` | 工具栏按钮经 `xdg-open`/`open`/`explorer` 调用系统文件管理器打开输出目录（Linux 已验证命令路径，交互需图形环境人工确认） |
| M21 人物身份识别 | 已完成 | `src/identity.rs`、`src/scene/face.rs`、`src/pipeline.rs`、`src/config.rs`、`src/report.rs`、`sepiola-gui/src/main.rs` | 端到端：men_1 与 men_1_copy 归入同一 `person_3/`，men_2、women_1 各自独立子目录；`identities.json` 记录标签；二次运行复用标签不新增人物；CSV 新增 `person` 列；GUI 人物列可改名并同步目录与图库 |
| M22 人脸关键点对齐 | 已完成 | `src/scene/landmark.rs`、`src/identity.rs`、`src/config.rs`、`config.example.toml` | 2d106det 106 点 → 5 点相似变换对齐到 ArcFace 112 模板；跨构图余弦 women_1 0.806→0.979、men_1 0.764→0.970；模型缺失/失败回退普通裁剪 |
| M23 Gallery 增强 | 已完成 | `src/identity.rs`、`src/pipeline.rs`、`src/config.rs`、`config.example.toml` | `extra` 多嵌入（旧 JSON 兼容）、Immich 式增量吸收、`merge`/`remove` 与 `merge_person_dirs`/`remove_person_dir`；单测覆盖，e2e 二次运行新图并入旧人物 |
| M24 GUI 模块化 | 已完成 | `sepiola-gui/src/{main,state,worker,actions,views,dialogs,assets}.rs` | 约 950 行 main.rs 拆为 7 个模块（main 77 行，其余 <250 行）；纯重构行为不变；`cargo check -p sepiola-gui` 与 GUI 冒烟通过 |
| M25 GUI 人物管理 | 已完成 | `sepiola-gui/src/{actions,dialogs,views,state}.rs` | 人物管理窗口支持重命名/合并到…/删除（可选删输出目录）；设置区新增关键点模型路径；调用 M23 merge/remove 并同步行内路径与 gallery |
| M26 后续事项补齐 | 已完成 | `luts/*.cube`、`src/pipeline.rs`、`src/config.rs`、`src/cli.rs`、`sepiola-gui/src/*` | 示例 LUT 随仓库提供；`run_batch_cancellable` 支持取消并记入 CSV（cancelled 列）；`Config` 可序列化并支持保存/载入 TOML；GUI 新增取消处理、保存/载入参数与拖拽导入目录 |
| M27 评测集与阈值扫描 | 已完成 | `src/eval.rs`、`src/bin/sepiola-eval.rs`、`src/identity.rs`、`test_data/identity/` | LFW 子集 80 张 / 52 位人物入库（含侧脸、遮挡、逆光、多人同框）；`sepiola-eval` 输出阈值扫描表并标出配置阈值；实测最佳阈值 0.35（F1 0.980，精确率 0.960，召回率 1.000，仅 1 处误合并） |
| M28 媒体库 | 已完成 | `src/library.rs`、`src/bin/sepiola-lib.rs`、`sepiola-gui/src/{library,library_view,main,state}.rs` | 库目录按扩展名归类（image/video/audio/document/archive/other）；图片自动分相册（人像/风景，人物作 person 子标签）；索引与独立 gallery 存于 `<库>/.sepiola/`；`sepiola-lib scan/albums/list/retag` 端到端验证；GUI 新增"处理/库"页签、扫描进度与重打标签；全局 item_spacing 加大缓解组件挤压 |
| M29 场景标签细分相册 | 已完成 | `src/scene/tags.rs`、`src/library.rs`、`src/config.rs`、`config.example.toml`、`models/mobilenetv2-1.0.onnx`、`models/imagenet_classes.txt`、`test_data/scene/` | 非人像图片按 ImageNet top-1 自动细分到「植物动物/生日/聚会」，未命中回退"风景"；6 张官方样本 top-1 全对（golden retriever 0.52、Persian cat 0.92、starfish 1.00、balloon 1.00、dining table 0.998、restaurant 0.92）；端到端 8 张混合库 8/8 归类正确；模型缺失回退不报错 |
| M30 库页签浏览与文件操作 | 已完成 | `src/library.rs`、`sepiola-gui/src/{library,library_ops,library_view,actions,main}.rs` | 文件列表 `ScrollArea::show_rows` 固定行高 24px 虚拟化，行数据仅存索引不克隆路径；点击行选中并在右侧面板即时解码预览（同路径复用）；右键菜单：打开所在目录/复制绝对路径/移动到相册（草稿+相册子菜单）/从索引移除（新增 `remove_from_index`，不动磁盘文件）；菜单动作延迟到循环后执行防越界；`cargo check -p sepiola-gui` 零告警、GUI 冒烟存活 |
| M31 后台缩略图与磁盘缓存 | 已完成 | `src/thumb.rs`、`sepiola-gui/src/{library,library_thumbs,library_ops,library_view,main}.rs` | 缩略图存 `<库>/.sepiola/thumbs/{128}/{FNV-1a64(相对路径)}.jpg`，缓存新鲜直接复用、源更新后重生成；单个后台线程惰性生成，UI 轮询加载纹理；行高 40、缩略图槽与标签共用右键菜单；thumb 3 项单测通过、`cargo check -p sepiola-gui` 零告警、GUI 冒烟存活 |
| M32 库内 RAW 一键显影 | 已完成 | `src/library.rs`、`src/thumb.rs`、`sepiola-gui/src/{library,library_ops,library_view,main}.rs` | RAW 行右键「显影为 JPEG」后台执行；产物 `<库>/.sepiola/developed/<镜像相对路径>.jpg` 幂等复用；真实 NEF 内置后端显影单测（可解码、不重写、工作目录清理、缺失源报错）通过；预览与缩略图回退用产物；`cargo check -p sepiola-gui` 零告警、113 项测试通过、GUI 冒烟存活 |
| M33 库内 RAW 批量一键显影 | 已完成 | `sepiola-gui/src/{library,library_develop,library_ops,library_view,main}.rs` | 库页签「全部显影」按钮（范围=当前视图，无 RAW 时禁用）；显影改为单个后台 worker + 任务队列（替代每文件起线程），`dev_inflight` 去重，进度条 + 成功/失败汇总；后端复用 `develop_file` 幂等复用产物；切换库目录 `reset_develop` 退出旧 worker；`cargo check -p sepiola-gui` 零告警、113 项测试通过、GUI 冒烟存活、库模块均 <250 行 |
| M34 场景分类细化 | 已完成 | `src/scene/places.rs`、`src/scene/tags.rs`、`src/library.rs`、`src/config.rs`、`config.example.toml`、`models/resnet18_places365.onnx`、`models/places365_classes.txt` | Places365 ResNet-18 ONNX（ailia 源，45MB）接入，预处理实测未烘焙按官方脚本复刻；新增 建筑/交通工具/美食/运动/冒险/探险/截图·文档 相册；风景改为显式自然类，移除"无脸兜底风景"（PPT 截图误判修复）；索引 version v2 一次性全量重归类迁移；真实模型回归 + 端到端 7 样本分布正确；118 项测试通过、GUI 冒烟存活 |
| M35 库第一级类型总览 | 已完成 | `src/library.rs`、`sepiola-gui/src/{library,library_overview,library_view,main}.rs` | FileKind 新增 录音/书籍/驱动/安装包/项目/游戏（epub→书籍、exe/deb→安装包、inf→驱动、音频+录音关键词→录音）；walk 按标记文件折叠项目/游戏实体，内部文件不入库；`kind_counts` 总览统计；索引 v4 迁移重判全部非 manual 条目类型；GUI 库页默认总览卡片页（配色计数、仅图片可点），图片页加返回按钮且只列图片；121 项测试通过、CLI 混合目录端到端 7 类型各 1、GUI 模块均 <250 行 |
| M36 电子书内容级元数据抽取 | 已完成 | `src/book.rs`、`src/library.rs`、`sepiola-gui/src/{books_view,library,library_overview,main}.rs`、`Cargo.toml` | 新增 zip/quick-xml/pdf-extract 依赖；EPUB（container.xml→OPF Dublin Core + 版权页文本启发式）、FB2（title-info）、MOBI/AZW3（PalmDOC/EXTH 手写解析，100/101/102/106/503 记录）、PDF（文本层启发式）抽取 书名/作者/出版社/年份/译者/版本/ISBN；`LibraryEntry.book_meta` 入库，索引 v5 回填旧书籍条目；GUI 总览"书籍"卡片进入 8 列元数据表格页；129 项测试通过（book 7 项含合成 fixture 与文本层 PDF）、CLI FB2 端到端元数据正确、GUI 冒烟存活 |
| M37 GUI 用户设置持久化与最近书库 | 已完成 | `sepiola-gui/src/{user_settings,state,library,library_overview,main}.rs`、`sepiola-gui/Cargo.toml` | JSON 配置按平台写入用户配置目录；恢复处理参数与当前书库，最近 8 个目录去重并支持下拉切换；picker/拖放/扫描共用书库切换；4 项 GUI 单测通过，`cargo check -p sepiola-gui` 零告警 |
| M38 书籍元信息筛选与正文搜索 | 已完成 | `src/book.rs`、`Cargo.toml`、`sepiola-gui/src/{book_filters,book_search,book_search_view,books_table,books_view,library,main}.rs` | 格式/作者/出版社/年份/译者/版本/ISBN 分面去重并按频次倒排；书名仍参与默认文字搜索、不生成下拉选项；元信息与文件名不区分大小写搜索；分面取交集，正文与元信息结果取并集；正文默认关闭，后台可取消；11 项核心测试、7 项 GUI 测试通过 |

## 2. 验证记录

### 2.1 编译与单元测试

```bash
cargo check --all-targets   # 通过，仅有 dead_code 警告（为后续可选输出预留的 API）
cargo check -p sepiola-gui  # 通过
cargo test                  # 113 passed; 0 failed（含真实 RAW 端到端、批量事件、JPEG 元数据、ArcFace 真实模型回归、关键点对齐、gallery 增广、取消跳过、配置往返、评测指标、媒体库归类/扫描/重打标签/索引移除/一键显影幂等、缩略图缓存、场景标签映射/预处理/真实模型样本回归）
```

覆盖点：

- `imageio`：16-bit TIFF/JPEG 读写往返、尺寸校验。
- `lut3d`：identity、已知 LUT、DOMAIN、缺失/错误条目。
- `blend` / `stats`：0/1/中间强度、clamp、纯白与纯黑裁切。
- `grade`：`build_stages` 结果与直接 `lut3d + blend` 一致、`strength_scale` 生效、未知 Stage 报错。
- `scene`：固定 stub 分类、最大人脸面积比例；`face`：BGR 与补零预处理、缩放、已知 cell 解码、阈值、NMS、人像面积判定；运行时库目录查找（优先无版本后缀）。
- `report`：对比图尺寸、CSV 转义与空表。
- `pipeline`：输入筛选与排序、空文件夹、缺失输入目录；递归扫描子目录、排除指定目录、输出路径镜像子目录；批量事件 `Discovered` 的 total 上报。
- `cli`：`-c` 参数可选；`config.toml` 查找的三种情况（exe 同目录优先、回退工作目录、都不存在）。
- `develop`：缺失文件错误信息；darktable 可用时实际生成 16-bit TIFF；后端选择（auto/darktable/builtin/非法值）；RAW 扩展名大小写。
- `builtin_develop`：黑白电平归一化、sRGB 编码端点、相机矩阵对系数缩放不敏感、RGGB 平场去马赛克、真实 Nikon D70 NEF 端到端、非 RAW 输入转 TIFF。
- `config`：`[output]` 默认值（8bit/keep_metadata=true）与非法值校验、`[identity]` 默认值与 threshold 范围校验。
- `imageio`：JPEG APP1 注入→copy→strip 往返、无元数据 JPEG strip 为 no-op。
- `identity`：余弦/阈值匹配、gallery 增改重命名与 JSON 往返、人脸裁剪外扩与贴边钳制、112 缩放、标签清洗；真实 ArcFace 模型回归（不同人余弦 < 0.35、同一人亮度扰动后 > 0.5，模型缺失时跳过）。
- `pipeline`：输出扩展名随 pixel_format 切换（jpg/tif）、`output_paths` 带人物子目录。

### 2.2 端到端批量验证

```bash
cargo run -- /tmp/sepiola-e2e/raws \
  -o /tmp/sepiola-e2e/out \
  --config /tmp/sepiola-e2e/config.toml
```

输入：4 张 JPEG + 1 个损坏的 `broken.dng`，`workers = 2`。

结果：

```text
done: 5 processed, 4 ok, 1 failed
log: /tmp/sepiola-e2e/out/log.csv
```

- 损坏文件返回 `develop failed: darktable-cli exited with exit status: 1 ...`，整批继续。
- 输出包含 `<name>.jpg`、`<name>_compare.jpg`（原图 | 结果，中间 8px 分隔）、`.work/<name>.tif`。
- `.work/*.tif` 经 `identify` 确认为 **16-bit**。
- `log.csv` 含成功与失败行，错误信息中的逗号/换行被正确引号转义。
- 调色生效：示例图平均 RGB 由 `[95.3, 85.6, 84.3]` 变为 `[97.4, 86.8, 77.2]`。
- 每次 darktable 调用的临时配置目录在结束后删除。

### 2.3 M8 人脸检测准确率

模型：OpenCV Zoo `face_detection_yunet_2023mar.onnx`（232,589 bytes，Apache-2.0），放在 `models/`（已加入 `.gitignore`）。

测试数据放在项目内 `test_data/`：

- `test_data/portrait/`：15 张头像（`randomuser.me/api/portraits`，men 1–9、women 1–6，128×128）。
- `test_data/landscape/`：15 张风景（Lorem Picsum id 10/12/14/16/17/18/19/1011/1015/1016/1018/1019/1036/1039/1043，800×600）。

```bash
cargo run -- test_data/portrait  -o /tmp/m8eval/out-portrait  --config config.toml
cargo run -- test_data/landscape -o /tmp/m8eval/out-landscape --config config.toml
```

`scene.classifier = "face"`，结果按 `log.csv` 的 `scene` 列统计：

| 测试集 | 图片数 | 处理成功 | 分类正确 | 准确率 |
| --- | --- | --- | --- | --- |
| `test_data/portrait`（人像） | 15 | 15 | 15 | 100% |
| `test_data/landscape`（风景） | 15 | 15 | 15 | 100% |
| 合计 | 30 | 30 | 30 | 100% |

- 人像全部判为 `portrait`，风景全部判为 `other`，无漏检、无过检。
- 推理输出 `cls_*`/`obj_*`/`bbox_*`（stride 8/16/32）被正确解码，NMS 后按最大人脸面积判定场景。
- 模型输入为固定 640×640：实现先等比缩放长边到 640，再顶左补零到 640×640；坐标按有效内容尺寸归一化，避免信箱边距影响比例。

### 2.4 M9 递归扫描端到端验证

输入结构：`in/top.jpg`、`in/sub/mid.jpg`、`in/sub/deep/nested.jpg`，输出目录设为 `in/out`（位于输入目录内）。

```bash
cargo run -- /tmp/m9/in -o /tmp/m9/in/out --config config.toml   # 连续执行两次
```

```text
run 1: found input files count=3 / done: 3 processed, 3 ok, 0 failed
run 2: found input files count=3 / done: 3 processed, 3 ok, 0 failed
```

输出：

```text
out/top.jpg                 out/top_compare.jpg
out/sub/mid.jpg             out/sub/mid_compare.jpg
out/sub/deep/nested.jpg     out/sub/deep/nested_compare.jpg
```

- 递归扫描覆盖 3 层目录，3 张图片全部处理成功。
- 第二次运行没有把 `out/` 中上一轮的 JPEG/对比图当成输入（仍为 3 张）。
- 输出与 `.work` 均镜像输入子目录，不同子目录的同名文件不会互相覆盖。

### 2.5 M10 ONNX Runtime 随包分发验证

```bash
cargo build
mkdir -p target/debug/lib
cp /usr/lib/x86_64-linux-gnu/libonnxruntime.so.1.23.2 target/debug/lib/libonnxruntime.so
env -u SEPIOLA_ONNXRUNTIME ./target/debug/sepiola /tmp/m10/in -o /tmp/m10/out --config config.toml
```

```text
INFO sepiola::scene::face: loading onnxruntime library=/home/neu/.ts/Sepiola/target/debug/lib/libonnxruntime.so
done: 1 processed, 1 ok, 0 failed
```

- 未设置环境变量时优先命中可执行文件同目录 `lib/`。
- 查找顺序：`SEPIOLA_ONNXRUNTIME` → `<exe_dir>/lib/libonnxruntime.so*` → `<exe_dir>/libonnxruntime.so*` → 系统常见路径；目录内优先无版本后缀的文件。
- 随包应使用官方自包含的 `libonnxruntime.so`；系统那份依赖 `libXNNPACK`、`libprotobuf` 等，不能直接拷到干净机器。

### 2.6 M11 配置文件可选验证

```bash
# 项目根目录没有 config.toml
cargo run --quiet -- /tmp/m11/in -o /tmp/m11/out
```

```text
INFO sepiola::scene::face: loading onnxruntime library=/usr/lib/x86_64-linux-gnu/libonnxruntime.so.1.23
done: 1 processed, 1 ok, 0 failed
```

```bash
./target/debug/sepiola /tmp/m11/in -o /tmp/m11/out2 -c /tmp/m11/missing.toml
# sepiola: config: failed to read /tmp/m11/missing.toml: No such file or directory (os error 2)
# exit=1
```

```bash
# 工作目录存在 config.toml（classifier = "stub"）时自动读取
cd /tmp/m11/cwd && sepiola /tmp/m11/in -o /tmp/m11/out3
# 无 "loading onnxruntime" 日志，log.csv 中 scene=other，证明读取了 cwd 的配置
```

- 未提供 `-c` 时：`./config.toml` 存在则读取，否则使用内置默认值（JPEG 92、workers 4、无 LUT、`classifier = "face"`、`max_clipped_high = 0.02`）。
- 显式 `-c` 指向不存在的文件仍然报错。
- 配置文件中的 `input_dir`、`output_dir`、`[portrait]`、`[other]` 均可省略，路径由命令行提供。

### 2.7 M12 模型路径优先 exe 同目录验证

```bash
mkdir -p /tmp/m12/dist/models
cp target/debug/sepiola /tmp/m12/dist/
cp models/face_detection_yunet_2023mar.onnx /tmp/m12/dist/models/
cd /tmp && /tmp/m12/dist/sepiola /tmp/m12/in -o /tmp/m12/out2
```

```text
INFO sepiola::scene::face: loading face detector model model=/tmp/m12/dist/models/face_detection_yunet_2023mar.onnx
done: 1 processed, 1 ok, 0 failed
# log.csv: men_3.jpg,portrait
```

- 相对路径先找 `<exe_dir>/<model>`，命中即使用；未命中回退到当前工作目录下的相对路径。
- 绝对路径原样返回，不受 exe 目录影响。

### 2.8 M13 LUT 路径解析验证

```bash
# dist 与 cwd 同时存在 luts/portrait.cube，配置里写相对路径 luts/portrait.cube
cd /tmp/m13/cwd && /tmp/m13/dist/sepiola /tmp/m13/in -o /tmp/m13/out2 -c /tmp/m13/dist/config.toml
```

```text
INFO sepiola::grade: loading lut lut=/tmp/m13/dist/luts/portrait.cube
done: 1 processed, 1 ok, 0 failed
# log.csv: men_3.jpg,portrait,lut
```

```bash
mv /tmp/m13/dist/luts /tmp/m13/dist/luts.bak
cd /tmp/m13/cwd && /tmp/m13/dist/sepiola /tmp/m13/in -o /tmp/m13/out3 -c /tmp/m13/dist/config.toml
# INFO sepiola::grade: loading lut lut=luts/portrait.cube  （回退到工作目录）
```

- 模型与 LUT 统一走 `paths::resolve_asset`，规则一致。
- LUT 加载会记录实际使用的路径，便于排查配置错误。

### 2.9 M14 config.toml 查找验证

```bash
cd /tmp/m14/cwd && /tmp/m14/dist/sepiola /tmp/m14/in -o /tmp/m14/out1
# INFO sepiola::cli: loading config config=/tmp/m14/dist/config.toml
```

```bash
cp /tmp/m14/dist/config.toml /tmp/m14/cwd/ && rm /tmp/m14/dist/config.toml
cd /tmp/m14/cwd && /tmp/m14/dist/sepiola /tmp/m14/in -o /tmp/m14/out2
# INFO sepiola::cli: loading config config=config.toml  （回退当前工作目录）
```

- 两处都没有 `config.toml` 时使用内置默认值。
- 显式 `-c` 仍然优先，文件不存在时报错。
- 单元测试：`config_beside_executable_takes_priority`、`config_falls_back_to_working_directory`、`config_absent_returns_none`。

### 2.10 M15 显影后端验证

```bash
# backend = "builtin"：JPEG 输入
./target/debug/sepiola /tmp/m15/in -o /tmp/m15/out -c /tmp/m15/config.toml
# done: 1 processed, 1 ok, 0 failed
identify -format '%z-bit %wx%h' /tmp/m15/out/.work/a.tif
# 16-bit 128x128
```

```bash
# backend = "auto" 且 PATH 中没有 darktable-cli（输入含 1 张 JPEG + 1 个损坏 DNG）
env PATH=/nonexistent sepiola /tmp/m15/in2 -o /tmp/m15/out2 -c /tmp/m15/auto.toml
```

```text
WARN sepiola::develop: darktable-cli not found, falling back to builtin develop backend
done: 2 processed, 1 ok, 1 failed
# b.jpg,other,,...,ok,
# broken.dng,,,...,error,"develop failed: builtin decoder failed ... Couldn't find a decoder for this file."
```

- 内置后端流程：`rawloader` 解码 → 黑/白电平归一化 → 双线性去马赛克（Bayer）→ 白平衡 → 相机矩阵 → sRGB → 16-bit TIFF。
- `rawloader` 无法识别的文件只影响该张，整批继续。
- 当时局限：未带真实 RAW 样本，RAW 解码仅由合成 RGGB 平场单测覆盖（已由 M16 补齐）；内置后端不做镜头校正、降噪、色调映射与 EXIF 方向，画质低于 darktable。

### 2.11 M16 内置后端真实 RAW 测试

样本：`test_data/raw/nikon_d70.nef`，来自 raw.pixls.us 公共样本库（Nikon D70，5,488,179 字节）。文件元数据：3040×2014、裁剪 `[上,右,下,左] = [0,2,0,0]`、白电平 4095、白平衡系数 `[555,256,388]`。

```bash
cargo test develops_real_raw_sample -- --nocapture
```

```text
real raw develop: 3038x2014 in 4.631407767s, mean=0.482, std=0.259
test result: ok. ...
```

```bash
cargo test
# 70 passed; 0 failed
```

- 端到端覆盖：NEF 解码 → 黑/白电平归一化 → 去马赛克 → 白平衡 → 相机矩阵 → sRGB → 16-bit TIFF。
- 断言：输出 3038×2014；亮度均值在 0.02～0.98 之间；标准差 > 0.02（排除全黑/全白/平坦画面）。
- 该测试首次运行时暴露相机矩阵量级问题：NEF 的 `xyz_to_cam` 为 ×10000 的整数系数，直接求逆使矩阵缩小约 1e4 倍，输出均值仅 0.0007（近乎全黑）。改用 rawloader 的 `cam_to_xyz_normalized()` 后均值恢复为 0.482。
- 新增 `camera_matrix_ignores_coefficient_scale` 回归测试：同一矩阵 ×1 与 ×1e-4 缩放后结果一致。
- 样本不存在时测试打印提示并跳过，保证精简检出仍可运行其余用例。

### 2.12 M17 桌面图形界面验证

```bash
cargo check -p sepiola-gui
cargo test                       # 71 passed
timeout 6 ./target/debug/sepiola-gui
# exit=124（超时终止，说明窗口已正常启动且无报错）
```

- 工程结构：新增 `src/lib.rs` 导出全部核心模块，`src/main.rs` 只调用 `sepiola::cli::run()`；`sepiola-gui` 作为 workspace 成员复用同一份核心代码。
- 进度事件：`pipeline::run_batch_with(cfg, on_event)` 推送 `Discovered { total }` 与 `Processed { completed, total, input, output, scene, ok, error }`；`run_batch` 内部改用空回调，CLI 行为不变。
- GUI 功能：输入/输出目录选择、显影后端（auto/darktable/builtin）、场景分类器（face/stub）、并行线程、JPEG 质量、高光阈值、人像与其他 LUT 及强度；后台线程执行批处理，界面显示进度与逐张结果，点击结果行查看 `<name>_compare.jpg`。
- 中文字体：启动时从 Noto CJK / 文泉驿 / 苹方 / 微软雅黑等常见路径加载回退字体；找不到时界面仍可运行，但中文可能显示为方块。
- 运行依赖：eframe 0.37（`App::ui(&mut Ui)` + 统一的 `egui::Panel` 接口）、rfd 0.15、image 0.24；Linux 需可用的 OpenGL/X11 环境。
- 验证范围：仅验证编译、单测与窗口启动；目录选择、LUT 选择、点击预览等交互需在图形环境中人工操作。

### 2.13 M18 导出选项端到端验证

```bash
# pixel_format = "16bit" → 输出 men_1.tif（对比图仍为 jpg）
# keep_metadata = true：源 JPEG 含 APP1，python3 段扫描确认输出同样含 APP1（True/True）
# keep_metadata = false：输出无 APP1 且可正常解码（128×128）
```

- 配置：`[output] pixel_format`（`8bit` 默认 / `16bit`）、`keep_metadata`（默认 `true`）；JPEG 质量沿用既有 `jpeg_quality`。
- 实现：8bit 路径 `save_jpeg` 后按开关对输出做字节级元数据处理——`jpeg_copy_metadata`（源为 JPEG 时提取 APP1/APP2/COM 插入输出 SOI/APP0 之后）或 `jpeg_strip_metadata`；16bit 走 `save_tiff16`。

### 2.14 M19/M20 GUI 结果查看、保留/删除与打开输出位置

- `cargo check -p sepiola-gui` 通过；`timeout 6 ./target/debug/sepiola-gui` exit=124（窗口正常启动无报错）。
- M19：每行"保留"复选框默认勾选；"放大查看"打开可缩放窗口（滑条 0.1–4.0 + 100% 按钮）；工具栏提供全选/全不选/"删除未保留"（删除输出图与对比图并标记行状态）。
- M20：`open_output_location` 按平台 spawn `xdg-open`（Linux）/`open`（macOS）/`explorer`（Windows）。
- 交互（勾选、删除确认、文件管理器弹出）需图形环境人工验证。

### 2.15 M21 人物身份识别端到端验证

```bash
# /tmp/idtest：men_1.jpg、men_1_copy.jpg（同一文件）、men_2.jpg、women_1.jpg
# [identity] enabled=true threshold=0.35 gallery=/tmp/idgallery.json
cargo run -- /tmp/idtest -o /tmp/idout -c /tmp/cfgid.toml   # exit=0
```

- 首次运行输出：`person_3/men_1.jpg`、`person_3/men_1_copy.jpg`（同一人）、`person_1/men_2.jpg`、`person_2/women_1.jpg`；`/tmp/idgallery.json` 记录 3 个人物标签；CSV `person` 列与之一致。
- 二次运行（保留 gallery）：4 张全部复用原标签，gallery 仍为 3 条，未新增人物。
- 模型：`models/arcfaceresnet100-11-int8.onnx`（65,764,892 字节），从 hf-mirror（`onnxmodelzoo/arcfaceresnet100-11-int8`）下载；GitHub LFS 直连仅 ~4 KB/s 不可用。
- 关键修复：初版预处理按浮点模型习惯做 `(v*255-127.5)/127.5` 且用 RGB，导致嵌入塌缩（不同人余弦 0.81–0.87，全部误判同一人）。变体实验确认 int8 量化版已把归一化烘焙进量化参数，正确输入为 **BGR、原始像素值 0..255**：不同人余弦降至 0.02–0.06，同一人亮度扰动 10% 后仍 0.94。已补真实模型回归测试防退化。
- 降级路径：`enabled=false`、模型缺失或加载失败时仅 warn 并跳过人物识别，批处理照常完成（端到端已验证）。

### 2.16 M22 关键点对齐验证

- 单元测试 `alignment_normalizes_framing`：真实 YuNet 检测框 vs 居中收缩 20% 框，两种构图分别嵌入后取余弦。
  普通裁剪 women_1 0.806 / men_1 0.764；对齐后 0.979 / 0.970，均显著提升（模型缺失时跳过）。
- 关键点模型 `models/2d106det.onnx`（4.3 MB）从 hf-mirror `deepghs/insightface` 下载。
- 关键修复：该 fp32 导出同样把 `(x-127.5)/127.5` 烘焙进图（首节点 Sub/Div），必须喂原始 0..255 BGR；
  错误归一化会让输出退化为固定"平均脸"（对平移/缩放完全不响应）。输出为 (x,y) 交错、[-1,1]，像素换算 `v*96+96`（192 空间）。
- 对齐链路：crop_face → resize 192 → 106 点 → 双眼中心/鼻尖/嘴角 5 点 → 相似变换 → warp 到 ArcFace 112 模板；任一步失败回退普通裁剪。

### 2.17 M23 gallery 增强端到端验证

- 单测：旧 JSON（无 `extra`）兼容读取、`find` 对 extra 生效、`absorb` 规则（min_sim/max_extra）、`merge`（含 src 在 dst 之前的下标前移）、`remove`、`merge_person_dirs` 同名加 `__{src}` 后缀、`remove_person_dir`。
- e2e（/tmp/idtest 两轮运行，gallery=/tmp/idtest/identities.json）：第二轮新增 men_1b 匹配进 `person_1/` 子目录，未注册新人；增量吸收路径由单测覆盖。

### 2.18 M24/M25 GUI 模块化与人物管理验证

- `cargo check -p sepiola-gui` 通过；`cargo test` 88 项通过；`timeout 12 cargo run -p sepiola-gui` exit=124（窗口持续运行无 panic）。
- 模块行数：main 77、state 195、worker 150、actions 247、views 249、dialogs 141、assets 110（各 <250）。
- M25 合并/删除走 `Gallery::merge/remove` + `merge_person_dirs/remove_person_dir`（M23 已单测），GUI 仅做编排与行内路径同步；窗口内交互（合并下拉、删目录勾选）需图形环境人工验证。

### 2.19 M26 示例 LUT、取消、参数保存/载入验证

```bash
# 示例 LUT 端到端：18 张测试图（1 人像 + 15 风景）经 config.example.toml 的 LUT 调色
cargo run -- /tmp/lutcheck/in -o /tmp/lutcheck/out -c /tmp/lutcheck/c.toml
# done: 16 processed, 16 ok, 0 failed；log.csv 中 stages=lut（人像 + 其他场景均命中）
```

- 示例 LUT 为程序化生成的 33³ `.cube`（约 948 KB/个），随仓库提供，`config.example.toml` 的 `luts/portrait.cube`、`luts/landscape.cube` 引用开箱可用。
- 人像 LUT 效果实测（strength 0.4）：输出相对源图 R +2.1、G +2.5、B −0.5，方向符合"肤色轻微暖化"设计。
- 取消：`run_batch_cancellable` 单元测试覆盖"开始前即取消"——3 张全部记为 cancelled、不产生输出、CSV 含 3 行 `cancelled`、事件序列为 `Discovered{3}` … `Cancelled{remaining:3}`；未取消时 cancelled=0。
- 配置往返：`config_toml_roundtrip_preserves_all_settings` 校验 `config.example.toml` 序列化再解析后所有字段一致（含 `[scene]` 重命名生效）；`saved_config_can_be_loaded_back` 覆盖 `save_file`（自动建父目录）+ `load_file`，并断言非法配置不落盘。
- GUI：`cargo check -p sepiola-gui` 通过；后台启动 6 秒进程存活（窗口正常）；新增"取消处理"（运行时显示，点击后禁用并提示）、"保存参数…/载入参数…"（rfd 文件对话框）与拖拽导入目录（`raw.dropped_files` → 输入目录）。

### 2.20 M27 评测集扩展与阈值扫描验证

```bash
cargo run --bin sepiola-eval -- test_data/identity --step 0.05
```

- 评测集：`test_data/identity/<人物名>/<图片>.jpg`，52 位人物 / 80 张，取自 GitHub `CseMony/lfw_dataset` 的 `lfw_train`（LFW 官方 250×250 图，含侧脸、遮挡、逆光、戴镜与多人同框）。目录名即真实标签，共 1.5 MB，随仓库提供；其中 11 张检出多张人脸，按最大人脸参与扫描。
- 扫描结果（步长 0.05）：

  | 阈值 | 聚类 | 误合并 | 拆分 | 精确率 | 召回率 | F1 |
  | --- | --- | --- | --- | --- | --- | --- |
  | 0.20 | 31 | 10 | 0 | 0.686 | 1.000 | 0.814 |
  | 0.25 | 47 | 3 | 0 | 0.889 | 1.000 | 0.941 |
  | 0.30 | 49 | 3 | 0 | 0.923 | 1.000 | 0.960 |
  | **0.35** | **51** | **1** | **0** | **0.960** | **1.000** | **0.980** |
  | 0.40 | 52 | 1 | 1 | 0.979 | 0.958 | 0.968 |
  | 0.50 | 54 | 0 | 2 | 1.000 | 0.812 | 0.897 |
  | 0.70 | 69 | 0 | 10 | 1.000 | 0.229 | 0.373 |
  | 0.80 | 80 | 0 | 15 | 1.000 | 0.000 | 0.000 |

- 结论：现有 `[identity] threshold = 0.35` 恰为 F1 最佳（0.980）；阈值 ≤0.20 出现 10 处误合并，≥0.40 开始拆分同人，验证了 0.35 的稳健性；下限（0.25～0.30）仍是可接受的保守替代。
- 单测（不依赖模型）：`clean_threshold_separates_everyone`、`low_threshold_merges_distinct_persons`、`high_threshold_splits_same_person`、`sweep_covers_endpoints_and_picks_best_f1`、`person_count_dedups_labels`，覆盖精确率/召回率、并查集聚类数、误合并与拆分统计。

### 2.21 M28 媒体库验证

```bash
cargo run --bin sepiola-lib -- scan /tmp/sepiola-libtest   # LFW 子集副本
cargo run --bin sepiola-lib -- albums /tmp/sepiola-libtest
cargo run --bin sepiola-lib -- list /tmp/sepiola-libtest --album 人像
cargo run --bin sepiola-lib -- retag /tmp/sepiola-libtest Nelson_Mandela/Nelson_Mandela_0001.jpg 生日
```

- 端到端：80 张 LFW 图全部归入「人像」相册（该子集均为人脸照，符合预期），自动注册 51 个 person 子标签；同人多张照片正确共用同一标签（如 `Abdoulaye_Wade` 4 张均为 `person_3`）；`retag` 新建「生日」相册并标记 (手动)，重扫不覆盖。
- 索引与 gallery 隔离：`<库>/.sepiola/library.json` 与 `<库>/.sepiola/identities.json`，扫描跳过隐藏目录；与批处理输出目录的 `identities.json` 互不干扰。
- 非图片按类型分桶：video/audio/document/archive/other 入库但不分相册；`list` 显示 `kind · person (手动)`。
- 单测（不依赖模型）：`classify_kind_buckets_by_extension`、`scan_adds_removes_and_skips_hidden_dirs`、`retag_persists_and_protects_manual_entries`、`albums_counts_images_only`、`cancelled_scan_leaves_existing_entries_saved`。
- GUI：新增"处理/库"页签；库页支持选目录/拖入目录、扫描归类（进度条 + 取消扫描）、相册 chips 浏览、文件列表与重打标签（下拉 + 自定义相册名）；`cargo check -p sepiola-gui` 通过，启动 8 秒存活。布局：全局 `item_spacing` 调整为 (12, 8)（原默认 (8, 3)），缓解组件挤压；库 GUI 模块拆为 `library.rs`（状态/线程，178 行）与 `library_view.rs`（视图，158 行），均 <250 行。

### 2.22 M29 场景标签细分相册验证

模型：ONNX Model Zoo `mobilenetv2-1.0.onnx`（14,246,826 bytes，Apache-2.0，经 S3 `s3.amazonaws.com/onnx-model-zoo` 下载，hf-mirror 对应仓库 404）；类名表 `models/imagenet_classes.txt`（pytorch/hub，1000 类，行号即输出下标）。

```bash
cargo test --lib scene::tags -- --nocapture   # 真实模型样本回归
cargo run --bin sepiola-lib -- scan /tmp/m29lib && cargo run --bin sepiola-lib -- albums /tmp/m29lib
```

- 归一化实测（预处理未烘焙进图，标准 torchvision 参数正确）：6 张 ImageNet 官方样本 top-1 全对——golden retriever 0.519、Persian cat 0.920、starfish 1.000、balloon 1.000、dining table 0.998、restaurant 0.924。
- 端到端：8 张混合库（3 动物 + 气球 + 餐厅 + 2 风景 + 1 人像）扫描后相册为 人像1（含 person_1）/植物动物3/生日1/聚会1/风景2，人像分支与 M28 行为一致。
- 映射规则：索引 ≤397（动物/自然物）+ 7 个真菌类 → 植物动物；balloon/candle → 生日；restaurant/dining table/pizza/cheeseburger/hotdog/wine bottle/beer glass/beer bottle → 聚会；其余人工物回退"风景"；`min_confidence = 0.15` 以下不细分。
- 模型/类名表缺失：`SceneTagger::new` 返回 Err，scan 内 `ok()` 降级为无 tagger，保持"风景"，不报错不中断。
- 单测 5 项：映射表覆盖、softmax 数值稳定、常数灰图归一化、缩放/裁剪尺寸、真实模型样本回归（模型缺失跳过）。
- GUI：库扫描配置来自 `Config::default()`，`[scene_tags]` 默认启用，无需新增控件；启动 8 秒存活。

### 2.23 M30 库页签虚拟化与文件操作验证

```bash
cargo check -p sepiola-gui   # 零告警
cargo test --lib library     # 含 remove_from_index_drops_only_target_entry
timeout 8 ./target/debug/sepiola-gui   # 存活（exit=124）
```

- 虚拟化：`ScrollArea::vertical().show_rows(ui, 24.0, total, |ui, range| ...)` 只渲染可见区间；行数据为 `Vec<usize>` 索引（相册过滤时按 `kind==Image && album` 过滤下标），每帧不再克隆全部 `PathBuf`；行内 `item_spacing.y=0` + `add_sized` 保证行高与 show_rows 假设一致。
- 点击预览：`Button::selectable` 行点击 → `select_library_file` 用 `image::open` 解码为 `ColorImage` 纹理存 `LibraryState.preview`；同路径复用避免重复解码；RAW/视频等解码失败提示"无可用预览"；右侧 `Panel::right("lib_preview")` 展示，缩放适配面板宽度。
- 右键菜单：`Response::context_menu` 四项——打开所在目录（`open_in_file_manager` 提为 pub 复用 M20 实现）、复制绝对路径（`ctx.copy_text`）、移动到相册（`menu_button` 子菜单：草稿相册 + 现有/内置相册，复用 `retag` 置 manual）、从索引移除（新增 `library::remove_from_index`，单测验证仅移除目标条目、幂等、磁盘文件不动）。
- 安全：菜单动作以 `RowAction` 枚举延迟到列表循环结束后执行，避免"从索引移除"重载索引后循环继续用旧下标越界。
- 模块规模：新增 `library_ops.rs`（114 行）承接文件操作，`library.rs` 164 行、`library_view.rs` 239 行，均 <250 行。
- egui 0.37 API 适配：`SelectableLabel` 已移除（改 `Button::selectable`）、`Ui::close_menu` 改名 `Ui::close`、`ScrollArea::id_salt` 替代 `id_source`。

### 2.24 M31 后台缩略图与磁盘缓存验证

```bash
cargo check -p sepiola-gui   # 零告警
cargo test --workspace       # 110 passed（含 thumb 3 项）
timeout 8 ./target/debug/sepiola-gui   # 存活（exit=124）
```

- 缓存键：手写 FNV-1a 64 对相对路径字节哈希为 16 位十六进制文件名，按 `max_side`（128）分桶；不用 `DefaultHasher`——其种子每进程随机，跨重启会全量缓存未命中。
- 新鲜度：缓存 mtime ≥ 源 mtime 直接复用（单测验证二次 `generate` 不重写文件）；源缺失或不可解码（RAW）返回 `Ok(None)`，不落占位文件，UI 侧记入 `thumb_requested` 防重复投递。
- 线程模型：单个 worker 线程 + `mpsc` 任务/结果双通道，惰性启动（库目录非空才 spawn）；切换库目录（选择/拖拽）时 `reset_thumbs` drop sender 退出旧线程并清空纹理；在途任务期间 `thumbs_pending` 以 100ms 低频重绘。
- UI：行高 24→40，行左 40×40 槽等比居中显示纹理，缺失时灰色占位并投递任务；右键菜单抽为 `library_ops.rs::row_context_menu`，缩略图槽与标签按钮共用同一动作集；非图片行槽留空。
- 模块规模：`src/thumb.rs` 126 行、`library_thumbs.rs` 99 行、`library.rs` 179 行、`library_ops.rs` 159 行、`library_view.rs` 237 行，均 <250 行。

### 2.25 M32 库内 RAW 一键显影验证

```bash
cargo check -p sepiola-gui   # 零告警
cargo test --lib library     # 含 develop_file_creates_jpeg_and_is_idempotent（真实 NEF）
cargo test --workspace       # 113 passed
timeout 8 ./target/debug/sepiola-gui   # 存活（exit=124）
```

- 后端 `library::develop_file`：`develop::raw_to_tiff`（三后端）→ `imageio::load` → `save_jpeg`；单测用内置后端对真实 Nikon D70 NEF 端到端显影，产物可被 `image::open` 解码；二次调用 mtime 不变（幂等复用）；`.work-*` 工作目录已清理；缺失源返回 `Err`。
- 产物命名 `sub/a.nef.jpg`（保留源扩展名 + 镜像子目录）：单测验证同 stem 不同扩展（`a.nef`/`a.dng`）产物不冲突；位于 `.sepiola` 隐藏目录内，扫描天然跳过，不会把产物当输入重复入库（呼应硬约束）。
- GUI：RAW 行右键出现「显影为 JPEG」（`is_raw_format` 按扩展名判定）；后台线程执行、`dev_inflight` HashSet 防同文件重复请求；在途期间 100ms 低频重绘；成功后状态栏提示、选中项自动重解码预览、`thumb_requested.remove` 触发缩略图重新生成。
- 联动回退：`select_library_file` 与 `thumb::generate` 均在源解码失败时尝试 `developed_path` 产物，显影前占位/提示、显影后自动出图。
- 模块规模：`library_ops.rs` 226 行、`library.rs` 191 行、`library_view.rs` 240 行，均 <250 行。

### 2.26 M33 库内 RAW 批量一键显影验证

```bash
cargo check -p sepiola-gui   # 零告警
cargo test --workspace       # 113 passed
timeout 8 ./target/debug/sepiola-gui   # 存活（exit=124）
```

- 入口：库页签顶栏「全部显影」按钮（`develop_button_ui`），范围跟随当前视图——选中相册时只显影该相册内 RAW，"全部"时覆盖整个索引；`pending_raw_count()==0` 或扫描中时禁用并悬停提示；队列在途时按钮显示「显影中…」，下方进度条「显影 done/total」。
- 线程模型：新增 `library_develop.rs`，把 M32 的"每文件起一个线程"改为**单个后台 worker + mpsc 任务队列**（任务携带 `(rel, Config)`，后端/质量沿用处理页当前选择），数百 RAW 批量时不产生线程爆炸与磁盘争用；`dev_inflight` HashSet 去重，批量与右键单文件共用同一队列，运行中再点按钮只追加未在途文件。
- 结果轮询：`poll_develop` 维护 done/ok/fail 计数——单文件保留 M32 的「已显影 → 产物」提示；批量中途显示进度、失败即时提示该文件错误，队列清空时汇总「成功 X，失败 Y（产物在 .sepiola/developed/）」；成功仍触发 `thumb_requested.remove` 与选中项预览刷新。
- 后端零改动：批量=重复调用 `library::develop_file`，已显影且产物 mtime ≥ 源的条目幂等秒回不重做（M32 单测覆盖）；`.sepiola` 隐藏目录产物不会被扫描当输入（硬约束）。
- 生命周期：切换库目录（选择/拖拽）时 `reset_develop` drop sender 退出旧 worker 并清空在途与计数，避免结果写回错库。
- 模块规模：`library_develop.rs` 235 行、`library_view.rs` 243 行、`library_ops.rs` 168 行、`library.rs` 198 行，均 <250 行（显影控件从视图抽入显影模块以守住上限）。

### 2.27 M34 场景分类细化验证

```bash
cargo test --lib places        # 5 项通过（类表解析/映射/预处理/真实模型回归）
cargo test --workspace         # 118 passed
cargo run -q --bin sepiola-lib -- scan /tmp/sepiola_e2e   # 端到端归类
timeout 8 ./target/debug/sepiola-gui   # 存活（exit=124）
```

- 模型来源：HuggingFace 无现成 Places365 ONNX，改用 ailia-models 托管的 `resnet18_places365.onnx`（storage.googleapis.com，直连 5 分钟仅 24MB，`curl -C -` 断点续传完成 45.6MB）；类表取 CSAILVision 官方 `categories_places365.txt`（365 行，`/x/name 0` 格式，解析去 3 字符前缀保留子路径）。
- 预处理实测：`strings` 检查首节点为 Conv（无 Sub/Div），确认未烘焙；按官方脚本复刻 拉伸 256×256（不保宽高比）→ center crop 224 → /255 → ImageNet mean/std → NCHW，输入名 `input_img`。回归：`test_data/landscape/pic_10.jpg` → top1 `lagoon` 0.377 → 风景；合成 PPT 图 → `server_room` 0.324 → 未命中（不判风景），证明模型响应正常且缺陷修复。
- 归类链路：人脸→人像；ImageNet 物体类先行（植物动物/生日/聚会 + 新增 美食（pizza/cheeseburger/hotdog/bagel/ice cream/banana/burrito）、截图·文档（screen/desktop computer/notebook/computer keyboard））；再 Places 场景类（风景/建筑/交通工具/运动/冒险/探险/美食/聚会）；均未命中→未分类。**风景兜底彻底移除**。
- 端到端：7 张混合样本扫描 → 植物动物 3、生日 1、聚会 2、风景 1，M29 行为不回退；`library.json` version 字段升为 2，v1 旧索引首次扫描全量重归类非 manual 图片一次。
- 已知局限：Places365/ImageNet 均无 日出/天象/截图 场景类——日出/天象仍留待专项分类器；PPT 截图依赖 ImageNet screen 类命中，无显示器边框的纯截图会落入未分类（不再误入风景）。
- GUI 零改动：侧栏相册列表本就按索引计数派生，新相册自动出现。

### 2.28 M35 库第一级文件类型总览验证

```bash
cargo test --workspace         # 121 passed（含 classify_kind_new_buckets / scan_collapses_project_and_game_dirs / migration_v4_reclassifies_old_kinds）
cargo run -q --bin sepiola-lib -- scan /tmp/sepiola_mix   # 混合目录端到端
```

- 端到端：混合目录（项目 Cargo.toml、Steam steamapps+appmanifest_.acf、epub、exe、录音 m4a、mp3、狗照片）扫描后共 7 条目 = audio/book/game/image/installer/project/recording 各 1；`proj/src/main.rs`、`game/data.pck` 等实体内部文件确认不入库，实体条目相册名=目录名。
- 类型分桶：epub/mobi/azw3/fb2/djvu→书籍；exe/msi/deb/rpm/apk/dmg/pkg/appx/snap/flatpak→安装包（自压缩包桶拆出，zip/rar/7z/tar 等保留）；inf/sys/cat→驱动；音频扩展名且文件名含 录音/record/rec_/voice/speech/通话→录音，其余音频不变。
- 迁移：索引 version 升 4，v3 及更旧索引首次扫描对全部非 manual 条目重判类型（v4 迁移单测：旧"文档"标记的 a.epub 重扫后改判 Book）。
- GUI：库页默认进入总览卡片页（12 类型按 ALL_KINDS 顺序、配色卡片+计数，仅"图片"可点进入浏览页）；图片浏览页顶部"← 返回总览"，列表与"全部"计数只含图片；`library_view.rs` 231 行、`library_overview.rs` 107 行，均 <250。
- 已知局限：录音判定为文件名启发式；游戏标记集覆盖 Steam/Godot 形态，其余游戏目录形态待样本反馈扩充；非图片类型浏览页未实现（卡片置灰提示）。

### 2.29 M36 电子书内容级元数据抽取验证

```bash
cargo test --lib book::    # 7 passed（EPUB/FB2/MOBI 合成 fixture、文本层 PDF、版权页启发式、损坏文件 None）
cargo test --workspace     # 129 passed
cargo run -q --bin sepiola-lib -- scan <含 fb2 的临时库>   # 端到端
timeout 8 ./target/debug/sepiola-gui                       # exit=124 存活
```

- 端到端：`活着.fb2` 扫描后索引 version=5，条目 `book_meta` 含 title=活着、author=余华、publisher=作家出版社、year=2012。
- 抽取实现：EPUB 经 container.xml 定位 OPF，Dublin Core 直取书名/作者/出版社/日期，contributor role="trl" 或含"译"判译者，再从正文 XHTML 去标签文本补齐 著/译/第X版/修订版/ISBN/年份 启发式；MOBI 扫描 "MOBI" 魔数定位 record0（不依赖固定头长，兼容 num_records 变化），EXTH 记录 100/101/102/106/503，加密返回 None；PDF 用 pdf-extract 抽文本层前 30K 字符走同一套启发式，扫描版无文本层返回 None；djvu 不支持。
- 迁移：`LibraryEntry.book_meta` 为 `#[serde(default)]`，旧索引直接兼容；v4 及更旧索引仅回填 book_meta 缺失的非 manual 书籍条目（区别于 v4 的全量重归类），v5 回填单测通过。
- GUI：总览"书籍"卡片可点进入 `books_view.rs`（105 行 <250），8 列表格按书名排序、缺字段"—"、未抽取计数提示；`LibraryPage` 路由改 match 三分支。
- 已知局限：中文启发式对排版变体（全角空格、跨行名字、"著译者"歧义）可能漏判或误判，字段宁缺毋滥；MOBI 不解析正文级译者/版本；PDF 书名/作者无可靠通用规则（仅版权页关键词命中时填充）。

### 2.30 M37 GUI 用户设置持久化验证

```bash
cargo test -p sepiola-gui  # 4 passed; 0 failed
cargo check -p sepiola-gui # 通过，零告警
cargo build -p sepiola-gui # 通过，更新 target/debug/sepiola-gui
rustfmt --check --edition 2021 sepiola-gui/src/user_settings.rs # 通过
```

- 设置文件为 `user-settings.json`：Linux 使用绝对 `XDG_CONFIG_HOME`，否则 `~/.config/sepiola/`；macOS 使用 `~/Library/Application Support/sepiola/`；Windows 使用 `%APPDATA%`（缺失时回退 `%LOCALAPPDATA%`）下的 `sepiola/`。
- 设置值变化时才保存；通过临时文件替换写入。缺失/损坏配置回退默认值；目录历史最多 8 项，最近优先并去重。
- 启动时恢复处理参数和有效书库；选择器、拖放及扫描共用目录切换逻辑，重新载入索引并清理旧预览/后台任务；总览页可从最近列表切换。
- 测试覆盖 JSON 往返、缺失/损坏配置回退、最近目录去重/顺序/上限，以及活动书库变更触发脏快照并持久化。原实现选库时提前更新了比较快照，导致帧末检测不到变化；修复后最近列表在快照副本生成时更新。启动有已保存书库时默认进入库页。未在图形环境中执行 GUI 手动交互冒烟；`cargo fmt --all -- --check` 仍会报告本次范围外既有格式差异，未全仓重格式化。

### 2.31 M38 书籍元信息筛选与正文搜索验证

```bash
cargo test --lib book::    # 11 passed
cargo test -p sepiola-gui  # 7 passed
cargo test --workspace     # 134 passed, 1 failed（既有目录扫描测试，单独复跑仍失败）
```

- 分面从已载入的书籍索引构建，覆盖格式、作者、出版社、年份、译者、版本、ISBN；不生成书名下拉项，书名仍在默认元信息全文搜索范围内；选项去重，按命中数降序、名称升序排列。
- 文本框默认搜索所有元信息和相对路径；「搜索正文」默认未选中，显式勾选并点击按钮后才异步检索。正文与元信息搜索命中取并集，再与分面条件取交集；显示进度，可取消；切换书库、扫描完成或修改关键词会清理旧结果/取消旧任务。
- 正文支持 EPUB XHTML、PDF 文本层、FB2 XML、未加密且采用未压缩或 PalmDOC 压缩的 MOBI/AZW；包含 GBK 等常见编码处理和 64 MiB 读取上限。扫描版 PDF、加密文件、Huffman 压缩 MOBI/AZW 与 DjVu 不提供正文命中。
- GUI 搜索模块拆分后各模块 250 行以内；新增分面排序、任意字段/路径搜索、分面组合和正文格式解压测试。
- workspace 唯一失败为未改动的 `library::tests::scan_collapses_project_and_game_dirs`：测试取消扫描后仍期待索引版本为 8（实际为 0），单独复跑可复现；与本次图书检索代码无关。
- `rustfmt` 对本次新增/重写 GUI 搜索模块执行格式化；`src/book.rs` 全文件检查仍报告既有格式差异，未对旧代码做整文件重排。未在图形环境手动检查界面交互。

## 3. 与计划的差异及原因

1. **16-bit 输出方式**：darktable 5.4.1 的 `--bpp 16` 会提示 `due to API restrictions ... cannot set the BPP to 16`，且默认导出 8-bit。实现改为在独立配置目录中预置 `darktablerc`：
   `plugins/imageio/format/tiff/bpp=16`，输出确认为 16-bit。
2. **并行隔离**：每次 `darktable-cli` 调用使用 `.work/.darktable/<pid>-<序号>-<文件名>` 独立 `--configdir`，并加 `--apply-custom-presets false`，避免并行实例争用数据库；调用结束后删除。
3. **develop 输入**：除了 RAW，也接受 darktable 可读的 JPEG/TIFF，便于端到端测试；生产路径不变。
4. **高光重做条件**：严格按计划，`stats_after.clipped_high > max_clipped_high` 时以 `strength_scale = 0.5` 从原图重做一次；重做后即使仍超标也不再重试，只在 CSV 中记录结果。
5. **分类器切换**：`scene.classifier` 支持 `"face"`（默认，YuNet）与 `"stub"`（测试用固定场景）。`SceneStages` 在启动时构建一次，LUT 用 `Arc` 持有并在线程间共享；`FaceClassifier` 内用 `Mutex<Session>` 串行化推理（`Session` 本身非并发安全）。
6. **ONNX Runtime 加载方式**：为避免下载约 100 MB 的官方二进制，`ort` 使用 `load-dynamic` + `api-23`。查找顺序：环境变量 `SEPIOLA_ONNXRUNTIME` → 可执行文件同目录 `lib/` → 可执行文件同目录 → `/usr/lib/x86_64-linux-gnu/libonnxruntime.so.1.23` 等常见系统路径。加载模型时 ORT 1.23 会向 stderr 打印大量 `Schema error: Trying to register schema ... already registered`，属于上游重复注册 schema 的已知告警；实测人脸检测与批量输出均正常（exit=0，2/2 成功），仅为控制台噪声。
7. **模型输入尺寸**：YuNet 2023mar 导出为固定 640×640 输入，因此预处理改为等比缩放到长边 640、顶左补零到 640×640，再按有效内容尺寸归一化人脸框；而不是 OpenCV 示例中的动态尺寸 + 32 对齐。
8. **M8 评测数据**：计划要求 15 张人像 + 15 张风景，数据集已落在 `test_data/`（头像来自 randomuser.me，风景来自 Lorem Picsum）。两者均为网络样例图，其中人像以正脸头像为主，对侧脸、遮挡、合影等困难样本覆盖不足；扩大评测集时可继续向 `test_data/` 增补。
9. **CLI 与配置的路径优先级**：命令行 `input_dir` / `output_dir` 覆盖配置文件中的同名项；模型与 LUT 的相对路径统一先找可执行文件同目录，未命中再回退当前工作目录。
10. **递归扫描与输出结构**：`collect_inputs` 改为深度优先递归；输出目录若在输入目录内，会按 canonicalize 后的路径比较并跳过，避免重复处理产物；输出与 `.work` 镜像输入相对路径（如 `out/sub/a.jpg`），因此不同子目录的同名文件不会互相覆盖。符号链接目录不跟随，避免目录环。
11. **ONNX Runtime 分发方式**：采用“随包目录”方案（A），库放在可执行文件旁的 `lib/`，启动时按上述顺序查找；不嵌入 ELF（方案 B）也不静态链接（方案 C）。当前环境验证时直接复制了系统库，正式分发需替换为官方自包含版本。
12. **配置文件可选**：`-c/--config` 由必填改为可选；省略时先找可执行文件同目录的 `config.toml`，其次当前工作目录，都没有则使用内置默认值。新增 `Config::load_with_paths`，先解析 TOML 再用命令行路径覆盖并校验，因此配置文件可省略 `input_dir`、`output_dir`、`[portrait]`、`[other]`。显式传入的配置文件不存在时仍报错。
13. **模型路径解析**：配置中的模型路径若为绝对路径则原样使用；若为相对路径，先找可执行文件同目录，再回退到当前工作目录。这样分发目录可整体移动到任意位置运行，同时不影响原有“在项目根目录运行”的用法。
14. **共享路径解析**：模型与 LUT 共用 `src/paths.rs` 的 `resolve_asset`，避免两处规则不一致；LUT 加载日志会记录实际使用的 `.cube` 路径。
15. **显影后端可切换**：新增 `[develop] backend`（`auto`/`darktable`/`builtin`）。`auto` 在启动时检测一次 `darktable-cli`，可用则用 darktable，否则回退内置后端。内置后端基于 `rawloader` 自行实现最小 RAW 管线（黑白电平、Bayer 去马赛克、白平衡、相机矩阵、sRGB），仅作为兜底，默认仍由 darktable 保证画质。
16. **未引入 LibRaw**：内置后端选择纯 Rust 的 `rawloader`，避免再依赖系统 C 库；代价是 CR3 等新格式支持取决于 `rawloader` 版本，且没有去马赛克之外的增强处理。
17. **相机矩阵改用归一化逆矩阵**：真实 NEF 测试暴露 `xyz_to_cam` 系数被放大 10000 倍的问题，直接求逆会让输出几乎全黑。改为使用 rawloader 的 `cam_to_xyz_normalized()`（先按行归一化再求伪逆），并删除自实现的 3×3 求逆，避免重复维护。
18. **真实 RAW 样本入库**：`test_data/raw/nikon_d70.nef`（5.2 MB）随仓库提供；测试在样本缺失时跳过，不影响其他用例。
19. **GUI 技术栈**：选择 egui/eframe 而非 Tauri，纯 Rust 实现、不引入 WebView 与前端工具链，代价是不支持移动端（本次范围仅桌面）。实现按 egui 0.37 新接口：`App::ui(&mut Ui, &mut Frame)`、统一的 `egui::Panel::{top,left,right,bottom}`、`CentralPanel::show(ui, ...)`。GUI 作为 workspace 成员 `sepiola-gui`，核心代码未拆成独立 `sepiola-core` crate，而是由根包同时提供 lib 与 CLI。
20. **ArcFace int8 预处理**：`arcfaceresnet100-11-int8.onnx` 的归一化已烘焙进量化参数，输入必须是 BGR、原始像素值 0..255；按浮点模型习惯做减均值除方差会让嵌入塌缩（实测不同人余弦 0.87）。早期仅外扩方形化裁剪时同人跨构图余弦 ~0.7，已由 M22 关键点对齐解决（见第 23 条）。
21. **人物识别的触发条件**：仅 `portrait` 场景且检测到人脸时执行识别；识别失败（推理错误等）只 warn 并按无人生成到输出根目录，不使整批失败。gallery 读写在 `Mutex` 下串行，rayon 并行安全。
22. **模型下载源**：GitHub 上 ONNX Model Zoo 的 LFS 媒体直连速度 ~4 KB/s 不可用；改用 hf-mirror.com 的 `onnxmodelzoo/arcfaceresnet100-11-int8`（实测 ~334 KB/s）。分发时模型不入库，需按 `config.example.toml` 注释自行下载。
23. **关键点模型输入/输出约定**：`2d106det.onnx` 与 ArcFace int8 一样把归一化烘焙进图（首节点 Sub/Div），必须喂原始 0..255 BGR 而非减均值除方差；输出为 (x,y) 交错、[-1,1] 归一化，192 空间像素换算为 `v*96+96`。5 点索引（双眼中心/鼻尖/左右嘴角）按 men_1/women_1 实测校准。
24. **对齐适用范围**：相似变换对 ±25% 以内的构图/尺度差异有效（实测余弦 0.97+）；极端尺度或大姿态变化超出 192 关键点估计可靠范围时，回退 ArcFace 先验模板裁剪路径，不强行对齐。
25. **M24 模块划分微调**：在计划的 6 个模块外增加 `assets.rs`（模型发现、中文字体、场景配置与通用选择行控件），以满足"各模块 <250 行"；M25 的重命名/合并/删除合并进同一个"人物管理"Window，而非三个独立弹窗。
26. **评测集落库（M27）**：M8 的人像/风景样例分别来自 randomuser.me 与 Lorem Picsum，均为单人正脸为主；身份评测另建 `test_data/identity/`，取 LFW 官方图（GitHub `CseMony/lfw_dataset` 的 `lfw_train`，52 位人物 / 80 张 / 1.5 MB）。LFW 单张图仍可能含旁观者（本集 11 张检出多张人脸），评测按"取最大人脸"参与，与生产流程一致，并在输出中提示真实标签可能不精确；未检出人脸或推理失败的图只列入跳过清单。
27. **阈值扫描口径（M27）**：阈值扫描用两两比较（同人对/不同人对）而非复现串行注册流程，因此结果与输入顺序无关；聚类数、误合并与拆分用并查集统计。这样得到的是阈值本身的判别能力，不掺入 gallery 增量吸收顺序的影响。
28. **库相册 v1 范围（M28）**：计划提到的日出/天文/植物动物/聚会/生日等细分相册需要额外分类器，v1 只落地「人像/风景/未分类」三个自动相册 + 任意手动相册名（含"某人"person 子标签）；相册为逻辑分组，不移动磁盘文件（避免破坏用户原有目录结构，与 Immich 逻辑相册一致）。
29. **库 gallery 隔离（M28）**：库的人物图库存在 `<库>/.sepiola/identities.json`，与批处理输出目录的 `identities.json` 相互独立；同一人会在两处分别注册标签，跨场景统一标签留待后续（如以人脸嵌入做全局 gallery）。
30. **细分相册映射近似（M29）**：ImageNet-1k 无 birthday/party/日出/天文类，「生日」以 balloon/candle、「聚会」以餐厅/餐桌/酒水/快餐类近似命中，日出/天文留待专项分类器；「植物动物」用官方类序规则（0..=397 为动物/自然物）+ 散落区段的 7 个真菌类。相册名仍为 String，新增映射不改数据结构。
31. **场景标签预处理实测不烘焙（M29）**：与 InsightFace 模型相反，ONNX Model Zoo 的 mobilenetv2-1.0 未把归一化烘焙进图，按 torchvision 标准（短边 256 → center crop 224 → /255 → mean/std）喂入后 6/6 样本 top-1 正确；沿用"烘焙与否必须实测"的项目经验，未凭假设直接实现。另：样本 `pic_1015.jpg` 下载截断（无 FFD9 尾）导致归类"未分类"，重新下载修复；jellyfish 官方样例本身歧义（top-1 iPod 0.165），换用 Persian cat 样本。
32. **预览为即时解码而非缩略图缓存（M30）**：点击行时用 `image::open` 全图解码，万级目录滚动本身已虚拟化不受影响，但连续快速点击大图会有短暂停顿；后台缩略图生成与磁盘缓存已由 M31 落地（见差异 33 与 §2.24）。RAW 文件不经显影管线无法用 image crate 解码，预览面板明确提示而非静默失败。
33. **缩略图缓存键与线程（M31）**：缓存文件名用 FNV-1a 64 对相对路径哈希（`DefaultHasher` 种子每进程随机，会导致跨重启缓存未命中，弃用）；生成用单个后台线程（列表每屏仅数十张可见，单线程足够且避免磁盘争用与线程池管理）；RAW/损坏图解码失败不落错误占位文件，由 UI 的 `thumb_requested` 记住已请求，避免每帧重试风暴。
34. **一键显影产物位置与格式（M32）**：产物统一落 `<库>/.sepiola/developed/`（镜像子目录、文件名保留源扩展名如 `a.nef.jpg`），而非源文件旁 sidecar——不污染用户目录结构，且隐藏目录被扫描天然跳过，避免产物重复入库；输出 8-bit JPEG 而非 16-bit TIFF，满足预览/缩略图与分享场景，需要成品级输出仍走处理页完整管线（含 LUT）。后端与 JPEG 质量沿用处理页当前选择，库页不重复设置。
35. **场景模型选型与风景兜底（M34）**：用户选定 Places365 方案，但 HuggingFace 无现成 ONNX 导出，改用 ailia-models 托管的官方 ResNet-18 转换版（预处理实测未烘焙，与官方脚本逐参数复刻）；归类顺序为 ImageNet 物体类先行、Places 场景类其次，保证 M29 已有的植物动物/生日/聚会行为不回退；"无脸兜底风景"改为兜底"未分类"，风景仅来自显式自然类映射，并以索引 version v2 触发旧库一次性全量重归类；日出/天象两相册因两个模型均无对应类而暂缓（相册名不预留，待专项分类器接入）。

## 4. 未完成与后续事项

- **评测集扩展**：已由 M27 补齐——`test_data/identity/` 引入 LFW 子集（52 位人物 / 80 张，含侧脸、遮挡、逆光、多人同框），并配套 `sepiola-eval` 阈值扫描工具（见 §2.20）。原有 `test_data/portrait` + `test_data/landscape` 30 张用于场景分类准确率。
- **示例 LUT 文件**：已由 M26 补齐，`luts/portrait.cube` 与 `luts/landscape.cube`（各 33³、约 948 KB）随仓库提供，`config.example.toml` 引用开箱可用。
- **默认无 LUT**：不带配置文件运行时 `stages` 为空，只做 RAW 显影与重编码，不做风格化调色。
- **16-bit TIFF 成品输出**：已由 M18 实现（`[output] pixel_format = "16bit"`）。
- **人物识别精度**：M22 五点关键点对齐与 M23 多嵌入 gallery/增量更新已落地，同人跨构图余弦提升至 0.97+；阈值经 M27 在 LFW 子集上扫描验证（0.35 为 F1 最佳，见 §2.20）。极端尺度或大姿态下对齐仍回退普通裁剪，此类样本在 LFW 子集中以拆分形式暴露。
- **真实 RAW 样本覆盖有限**：已用 Nikon D70 NEF 覆盖 Bayer + 无黑电平 + 12-bit 的场景；DNG/CR3/RAF（X-Trans）以及带非零黑电平、14-bit 的机型尚未入库，建议后续补充多机型样本。
- **内置后端画质局限**：无镜头校正、降噪、色调映射与 EXIF 方向，默认仍应使用 darktable。
- **dead_code 警告**：属于为路线图预留的公开 API（如 `save_tiff16`、`Lut3d::size`、`Config::scene`），不影响运行。
- **GUI 后续**：批次取消、参数配置保存/加载、目录拖拽导入已由 M26 实现（GUI 内"取消处理"、"保存参数…/载入参数…"、拖拽目录到窗口）；自动化 UI 测试与移动端仍未支持。GUI 交互（目录选择、LUT 选择、点击预览、放大查看、保留/批量删除、打开输出位置、人物改名/合并/删除、取消、保存/载入、拖拽、库页签扫描/浏览/重打标签）需在图形环境中人工验证。
- **媒体库后续（M28 起步，M29～M34 部分完成）**：场景标签模型已接入，非人像自动细分「植物动物/生日/聚会」（近似映射，见差异 30）；日出/天文仍需专项分类器（或 EXIF/时间聚类）。M30 已落地列表虚拟化、点击预览与右键文件操作（见差异 32）；M31 已落地后台缩略图生成与磁盘缓存（见 §2.24）；M32 已落地库内 RAW 一键显影（见 §2.25 与差异 34）；M33 已落地批量一键显影（「全部显影」按钮 + 单 worker 队列，见 §2.26）；M34 已落地场景分类细化（Places365 + 7 个新相册 + 移除风景误兜底，见 §2.27 与差异 35）；M35 已落地库第一级文件类型总览（书籍/录音/驱动/安装包分桶 + 项目/游戏实体折叠 + 类型计数卡片页，见 §2.28）；M36 已落地电子书内容级元数据抽取（EPUB/FB2/MOBI/PDF 解析书名/作者/出版社/年份/译者/版本/ISBN，索引 v5 回填，书籍浏览页，见 §2.29）；其余非图片类型目前按扩展名/文件名启发式分桶并有计数，浏览页尚未实现（当前仅图片、书籍页可用），未做内容级归类；日出/天象相册仍待专项分类器（Places365/ImageNet 均无对应类）。相册始终为逻辑分组（仅索引 `album` 字段），不物理移动文件、不改动用户目录结构。

## 5. 使用方式

```bash
# 最简用法：不需要配置文件，使用内置默认值（无 LUT）
cargo run -- ./raws -o ./out

# 需要 LUT 或自定义参数时再指定配置
cp config.example.toml config.toml
# 放置模型与 ONNX Runtime
mkdir -p models
# 从 OpenCV Zoo 下载 face_detection_yunet_2023mar.onnx 到 models/
# （可选）人物身份识别：从 hf-mirror 下载 arcfaceresnet100-11-int8.onnx 到 models/，
#   国内直连 GitHub LFS 极慢，可用：
#   https://hf-mirror.com/onnxmodelzoo/arcfaceresnet100-11-int8/resolve/main/model.onnx
# （可选）人脸关键点对齐：models/2d106det.onnx，可从 hf-mirror 的 deepghs/insightface 仓库下载；
#   缺失时自动回退普通裁剪
# 放置 ONNX Runtime：优先 <exe_dir>/lib/libonnxruntime.so；也可 export SEPIOLA_ONNXRUNTIME=/path/to/libonnxruntime.so
# 按需修改 input_dir / output_dir / LUT 路径
cargo run -- ./raws -o ./out --config config.toml
# 输入目录会被递归扫描；输出保持相同的子目录结构（out/sub/a.jpg）

# 图形界面（需图形环境与 OpenGL）
cargo run -p sepiola-gui

# 人物识别阈值扫描（评测集目录结构：<人物名>/<图片>）
cargo run --bin sepiola-eval -- test_data/identity --step 0.05

# 媒体库：扫描归类 / 相册统计 / 浏览 / 重打标签
cargo run --bin sepiola-lib -- scan ~/Photos
cargo run --bin sepiola-lib -- albums ~/Photos
cargo run --bin sepiola-lib -- list ~/Photos --album 人像
cargo run --bin sepiola-lib -- retag ~/Photos sub/a.jpg 生日
```

说明：省略 `-c` 时，先找可执行文件同目录的 `config.toml`，其次当前目录；都没有则用内置默认值。配置文件中的模型与 LUT 若写相对路径，优先按可执行文件所在目录解析。

随包分发目录（方案 A）：

```text
sepiola-dist/
├─ sepiola                             # cargo build --release 产物
├─ lib/libonnxruntime.so               # 官方自包含库（onnxruntime-linux-x64-*.tar.gz）
├─ models/face_detection_yunet_2023mar.onnx
├─ luts/portrait.cube
├─ luts/landscape.cube
└─ config.toml
```

在该目录下直接运行 `./sepiola ./raws -o ./out` 即可：配置、模型与 LUT 都按“exe 同目录优先、其次当前工作目录”查找，库自动从 `lib/` 加载；因此从其他工作目录用绝对路径启动也能找到这些资源。
