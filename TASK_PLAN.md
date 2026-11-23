# Sepiola 任务计划

基于 ARCHITECTURE.md 第 5 节「实现顺序」制定，每步独立可验证。

---

## M0：项目骨架搭建

**文件**：`Cargo.toml`、`error.rs`、`main.rs`、`cli.rs`

- 创建 Rust 项目，引入依赖：
  - `clap`（derive）—— 命令行
  - `thiserror` —— 错误
  - `serde`、`toml` —— 配置
  - `image` —— 图像读写
  - `rayon` —— 并行
  - `tracing`、`tracing-subscriber` —— 日志
- 定义 `Error` 枚举（6 种变体）和 `Result<T>` 类型别名
- 用 `clap` 实现命令行参数：`input_dir`、`output_dir`、`--config`
- `main.rs` 仅调用 `cli::run()`

**验收**：`cargo run -- --help` 正常输出

---

## M1：图像读写

**文件**：`imageio.rs`

- 定义 `ImageF32` 结构体（RGB、行优先、f32、sRGB 非线性）
- 实现 `load()`：读 TIFF/JPEG，转 f32 并 normalize 到 0.0..=1.0
- 实现 `save_jpeg()`、`save_tiff16()`
- 16-bit 与 f32 转换：除以 65535

**验收**：读写往返误差 < 1/255

---

## M2：3D LUT 解析与应用

**文件**：`grade/lut3d.rs`

- 解析 `.cube` 文件（`LUT_3D_SIZE`、`DOMAIN_MIN/MAX`，R 变化最快）
- `Lut3d::from_cube_file()`：读取并构建 LUT 表
- `Lut3d::apply()`：三线性插值，原地修改，作用在 sRGB 编码值上

**验收**：identity LUT 应用后图像不变；已知 LUT 与手算结果一致

---

## M3：混合与统计工具

**文件**：`grade/blend.rs`、`grade/stats.rs`

- `blend()`：`out = original * (1-s) + graded * s`，strength 取 0.0..=1.0
- `ToneStats` 结构体（`mean_luma`、`clipped_high`、`clipped_low`）
- `analyze()`：计算平均亮度、高光/暗部裁切比例（像素占比 0..1）
- 输出前统一 `clamp(0.0, 1.0)`

**验收**：strength=0 返回原图，=1 返回结果；纯白图 clipped_high≈1

---

## M4：Stage trait 与配置系统

**文件**：`grade/mod.rs`、`grade/lut_stage.rs`、`config.rs`

- `Stage` trait（`name()` + `apply()`）
- `Context` 结构体（`scene`、`strength_scale`）
- `build_stages()` 函数
- `LutStage` 实现：内部 clone + LUT 应用 + blend，LUT 用 `Arc` 共享
- 配置结构与 TOML 读取（按场景拆分 `stages` 列表 + 子表）
- 生成 `config.example.toml`

**验收**：用配置创建 LutStage 列表并执行，结果与直接调用 lut3d + blend 一致

---

## M5：RAW 显影

**文件**：`develop.rs`

- 调用外部 `darktable-cli` 将 RAW 转为 16-bit TIFF
- 处理失败时返回 `Error::Develop` 并附带 stderr
- 调用前 `darktable-cli --help` 确认参数；并行时注意隔离配置目录

**验收**：能将一张 RAW 转成 TIFF；失败时错误信息完整

---

## M6：场景分析（stub）+ 单张流水线

**文件**：`scene/mod.rs`、`scene/stub.rs`、`pipeline.rs`

- `SceneInfo`、`FaceBox`（归一化坐标）、`SceneKind`（Portrait / Other）类型
- `SceneClassifier` trait（Send + Sync）
- `stub.rs`：假实现，返回固定分类结果
- `pipeline.rs`：编排单张完整流程
  - develop → classify → build_stages → apply
  - 高光溢出检查：`clipped_high` 超阈值 → `strength_scale` 减半 → 从原图重做
  - 输出 JPEG + 日志标记
- 对原始 RAW 只读，输出到 `output_dir`

**验收**：用 stub 分类器跑通单张完整流程，含高光溢出重试逻辑

---

## M7：批量处理与报告

**文件**：`report.rs`，扩展 `pipeline.rs`

- 生成原图/结果对比图
- 输出 CSV 处理日志（文件名、场景、Stage 列表、溢出状态等）
- `rayon` 并行处理整个文件夹（`par_chunks_mut` / `par_iter`）
- 单张失败不影响整批，记录错误继续处理，最后汇总
- LUT 在启动时读取一次，`Arc` 在线程间共享

**验收**：rayon 并行处理文件夹，输出对比图与 CSV

---

## M8：真实人脸检测

**文件**：`scene/face.rs`

- 用 `ort`（ONNX Runtime）或 `opencv` 实现人脸检测
- 替换 stub 分类器
- 人像判断逻辑：最大人脸占整图面积比例（而非"有没有脸"）
- 模型文件放 `models/` 目录，不进 git

**验收**：15 张人像 + 15 张风景，准确率达标

---

## M9：递归目录扫描

**文件**：`pipeline.rs`

- `collect_inputs` 递归遍历输入目录下的所有子目录
- 输出目录若位于输入目录内，自动跳过，避免把上一轮产物当成输入
- 输出镜像输入结构：`out/sub/a.jpg`、`out/sub/a_compare.jpg`；`.work` 也按子目录隔离，避免同名文件互相覆盖
- 不跟随符号链接目录，避免目录环
- 顶层目录行为保持不变

**验收**：嵌套目录中的图片全部被处理，同名文件不互相覆盖；将输出目录放在输入目录内重复运行不会重复处理产物

---

## M10：ONNX Runtime 随包分发（方案 A）

**文件**：`scene/face.rs`

- 动态库查找顺序：`SEPIOLA_ONNXRUNTIME` → 可执行文件同目录 `lib/` → 可执行文件同目录 → 系统常见路径
- 分发目录约定：`sepiola`、`lib/libonnxruntime.so`、`models/`、`config.toml`、`luts/`
- 使用官方自包含的 `onnxruntime-linux-x64-*.tar.gz` 中的库，不拷贝系统那份带 `libXNNPACK` 等外部依赖的库
- 启动日志记录实际加载的库路径
- 不做嵌入二进制（方案 B）与静态链接（方案 C）

**验收**：把库放到可执行文件同目录 `lib/` 下、不设环境变量仍能完成推理，日志显示加载的是该路径；找不到库时错误信息列出查找途径

---

## M11：配置文件可选

**文件**：`src/cli.rs`、`src/config.rs`

- `-c/--config` 改为可选参数
- 未提供时：当前目录存在 `config.toml` 则读取（保持旧行为），否则使用内置默认值
- 显式指定的配置文件不存在时仍然报错
- `Config` 实现 `Default`；`input_dir`、`output_dir`、`portrait`、`other` 允许在 TOML 中省略（路径由命令行提供）
- 默认值：JPEG 92、workers 4、无 LUT、`classifier = "face"`、`max_clipped_high = 0.02`

**验收**：不带 `-c` 且无 `config.toml` 时能正常处理图片；带 `-c missing.toml` 时明确报错；`cargo test` 覆盖默认值与可选参数

---

## M12：模型路径优先 exe 同目录

**文件**：`src/scene/face.rs`

- 配置中的绝对路径原样使用
- 相对路径先尝试 `<exe_dir>/<model>`，存在则使用
- 不存在时回退到相对当前工作目录的路径，保持原有行为
- 启动日志记录实际加载的模型路径；错误信息显示解析后的路径

**验收**：把二进制与 `models/` 一起复制到新目录，从其他工作目录启动仍能加载模型并正确分类；绝对路径与相对回退行为有单元测试

---

## M13：LUT 路径同规则解析

**文件**：`src/paths.rs`、`src/grade/mod.rs`、`src/scene/face.rs`

- 抽取共享的 `paths::resolve_asset`：绝对路径原样使用；相对路径先找 `<exe_dir>/<path>`，未命中回退当前工作目录
- 模型与 `.cube` LUT 都走该函数，避免两套规则漂移
- 启动日志记录实际加载的 LUT 路径

**验收**：分发目录中的 `luts/` 被优先加载（即使当前目录也有同名 LUT）；exe 旁没有 LUT 时回退到工作目录；`cargo test` 覆盖三种路径情况

---

## M14：config.toml 同规则查找

**文件**：`src/cli.rs`

- 未指定 `-c` 时：先找 `<exe_dir>/config.toml`，其次 `./config.toml`，都没有则用内置默认值
- 显式 `-c` 仍然优先，且文件不存在时报错
- 启动日志记录实际读取的配置文件路径
- 查找逻辑抽成 `find_config(exe_dir, cwd)`，便于单元测试

**验收**：分发目录带 `config.toml`、从其他工作目录启动时能自动读取；删除后回退到工作目录的 `config.toml`；两处都没有时使用默认值；单元测试覆盖上述三种情况

---

## M15：显影后端可切换（darktable / builtin）

**文件**：`src/config.rs`、`src/develop.rs`、`src/builtin_develop.rs`、`src/pipeline.rs`、`Cargo.toml`、`config.example.toml`

- 新增 `[develop] backend = "auto"`，取值 `auto` | `darktable` | `builtin`；非法值报配置错误
- `auto` 启动时检测一次 `darktable-cli`：可用走 darktable，缺失则告警并回退内置后端
- 内置后端：`rawloader` 解码 + 黑/白电平 + 双线性去马赛克（Bayer/RGB DNG）+ 白平衡 + 相机矩阵 → sRGB，写出 16-bit TIFF；非 RAW 输入直接转 TIFF
- RAW 扩展名判断统一到 `develop::is_raw_format`，扫描与显影共用
- 文档注明内置后端画质局限：无镜头校正、降噪、色调映射与 EXIF 方向

**验收**：`builtin` 后端跑通全流程且输出确为 16-bit TIFF；无 darktable 的 PATH 下 `auto` 回退内置并成功；无法解码的文件只失败该张；新增单元测试覆盖后端选择、去马赛克、色彩转换与非 RAW 转换

---

## M16：内置后端真实 RAW 样本测试

**文件**：`src/builtin_develop.rs`、`test_data/raw/nikon_d70.nef`

- 将 raw.pixls.us 的 Nikon D70 NEF（5,488,179 字节）纳入项目测试数据
- 端到端测试：真实 NEF → 解码 → 去马赛克 → 白平衡 → 相机矩阵 → 16-bit TIFF，断言输出尺寸为 3038×2014，亮度均值与标准差处于合理区间（非全黑/全白、非平坦画面）
- 修复相机矩阵量级问题：部分厂商的 `xyz_to_cam` 为 ×10000 的整数系数，改用 rawloader 的 `cam_to_xyz_normalized()`
- 增加矩阵缩放无关性回归测试，防止同类问题再次出现
- 样本缺失时跳过该测试，便于精简检出后仍能运行其余用例

**验收**：`cargo test` 全部通过；真实 RAW 输出非全黑/全白且具备足够的动态变化

---

## M17：桌面图形界面（egui/eframe）

**文件**：`src/lib.rs`、`src/main.rs`、`src/pipeline.rs`、`Cargo.toml`、`sepiola-gui/`

- 新增 `src/lib.rs` 导出核心模块，CLI 改为调用 `sepiola::cli::run()`，使 GUI 与 CLI 复用同一份实现
- 根 `Cargo.toml` 增加 workspace，新增 `sepiola-gui` 成员（`eframe` + `rfd` + `image`）
- `pipeline` 增加 `run_batch_with(cfg, on_event)`：扫描完成与单张完成时推送 `BatchEvent`，原 `run_batch` 行为不变
- GUI 最小功能：输入/输出目录选择、显影后端、场景分类器、并行线程、JPEG 质量、高光阈值、人像/其他 LUT 与强度
- 运行时在后台线程执行批处理，界面展示进度条与逐张结果；点击结果行显示 `<name>_compare.jpg` 前后对比
- 启动时从常见系统路径加载中文字体作为回退（Noto CJK / 文泉驿 / 苹方 / 微软雅黑）

**验收**：`cargo test` 全部通过；`cargo check -p sepiola-gui` 通过；GUI 从选择目录到出图、查看对比图全流程可用

---

## M18：导出选项（像素格式 / 压缩率 / 元数据）

**文件**：`src/config.rs`、`src/imageio.rs`、`src/pipeline.rs`、`config.example.toml`、`sepiola-gui/src/main.rs`

- 新增 `[output]` 配置节 `OutputSettings`：
  - `pixel_format`：`"8bit"`（默认，JPEG）或 `"16bit"`（TIFF，走已有 `imageio::save_tiff16`）；扩展名随格式变化（`.jpg` / `.tif`）
  - `keep_metadata`：默认 `true`；`false` 时删除输出 JPEG 的 APP1/APP2/COM 段（`jpeg_strip_metadata`）；`true` 且源为 JPEG 时把源 APP1/APP2/COM 复制到输出（`jpeg_copy_metadata`）；RAW 源由 darktable 自带 EXIF，仅做删除侧
  - 压缩率沿用顶层 `jpeg_quality`（默认 92），GUI 已有滑条
- `validate()` 检查 `pixel_format ∈ {8bit,16bit}`
- `output_paths` 需按格式选择扩展名；对比图始终 JPEG
- GUI“处理参数”区新增像素格式下拉与元数据复选框

**验收**：单测覆盖 8/16bit 输出扩展名与元数据复制/剥离；`cargo test` 通过

---

## M19：结果查看与保留/删除（GUI）

**文件**：`sepiola-gui/src/main.rs`

- 结果行新增“保留”复选框，默认勾选；点击行在右侧预览基础上增加“放大”按钮：`egui::Window` 模态显示对比图，带缩放滑条（按可用宽度自适应，可放大到原始尺寸）
- 新增“删除未保留”按钮：对未勾选的行删除其输出文件与对比图（`fs::remove_file`，容错），行标记“已删除”；提供全选/全不选
- 删除后行状态列显示“已删除”，避免重复操作

**验收**：`cargo check -p sepiola-gui` 通过；交互在图形环境人工验证

---

## M20：打开输出位置（GUI）

**文件**：`sepiola-gui/src/main.rs`

- “开始处理”附近新增“打开输出位置”按钮：`std::process::Command` 按平台调用 `xdg-open`（Linux）/ `open`（macOS）/ `explorer`（Windows），目标为输出目录（不存在则先创建）；失败时写入状态消息

**验收**：`cargo check -p sepiola-gui` 通过；图形环境点击后文件管理器打开输出目录

---

## M21：人物身份识别与按人归档

**文件**：`src/config.rs`、`src/scene/face.rs`（runtime_library 改 pub(crate)）、`src/identity.rs`（新）、`src/pipeline.rs`、`src/report.rs`、`sepiola-gui/src/main.rs`、`models/arcfaceresnet100-11-int8.onnx`

- 新增 `[identity]` 配置：`enabled`（默认 true，模型缺失时自动禁用并告警）、`model`（默认 `models/arcfaceresnet100-11-int8.onnx`）、`threshold`（默认 0.35，余弦相似度）、`gallery`（默认 `identities.json`，`paths::resolve_asset` 解析）
- `src/identity.rs`：
  - `FaceIdentifier`：ORT 会话（`Mutex<Session>`），输入 1×3×112×112 RGB、`(v*255-127.5)/127.5`，取首个输出 512 维并 L2 归一化
  - `Gallery`：`Vec<Entry{label, embedding}>` JSON 持久化（serde Serialize）；`identify(emb)`：最优余弦 ≥ threshold 复用标签，否则新增 `person_N` 并写回
  - 线程安全：`Mutex` 保护 gallery，批量结束后 `save`
- pipeline：场景为 Portrait 且检出人脸时，取最大人脸框从显影原图裁剪（外扩 ~20% 边距、方形化）→ 嵌入 → 识别；输出路径插入人物子目录 `out/<label>/<rel_stem>.<ext>`；`SingleResult`/`CsvRow` 增加 `person: Option<String>`；`BatchEvent::Processed` 的 output 使用实际路径（成功时取 `result.output`）
- GUI：结果表新增“人物”列；每行人物标签可编辑，保存时更新 gallery JSON 并把 `out/<旧名>` 目录 `fs::rename` 为 `out/<新名>`；设置区新增身份开关、模型路径选择、阈值
- 模型下载：`https://media.githubusercontent.com/media/onnx/models/main/validated/vision/body_analysis/arcface/model/arcfaceresnet100-11-int8.onnx`（65MB，GitHub LFS，进行中的后台任务）

**验收**：同一人多张图归入同一子目录、不同人分开；重启后 gallery 生效（旧标签复用）；模型缺失时批处理正常回退；`cargo test` 通过

---

## M22：人脸关键点对齐（参考 digiKam/InsightFace）

**现状问题**：`crop_face` 只做方形外扩，无姿态对齐；同一人不同构图/角度余弦仅 ~0.7。已确认 YuNet 导出仅 4 通道 bbox、**无关键点输出**（`decode_stride`）。

**方案**：
- 引入可选关键点模型 `models/2d106det.onnx`（InsightFace 2D106，约 4.3MB；下载源优先 hf-mirror 的 deepghs/insightface 仓库）
- 新增 `src/scene/landmark.rs`：人脸框 → 方形裁剪 → 112×112 → 输出 106 点 → 取双眼中心/鼻尖/嘴角 5 点（索引映射须用 men_1/men_1_copy 实测校准）→ **相似变换**对齐到 ArcFace 标准 5 点模板 → 112×112 对齐图喂给 ArcFace
- 配置：`[identity] landmark_model = "models/2d106det.onnx"`；模型缺失/推理失败 → 回退现有 `crop_face`（warn，不中断）
- **降级预案**：若关键点模型下载或 5 点映射验证失败，M22 改为"多裁剪增强"（同一人脸 2~3 种外扩比例各生成嵌入，交给 M23 多嵌入 gallery 吸收差异），并在文档记录

**验收**：同人跨构图相似度显著提升（目标 ≥0.8）；无模型时行为与 M21 一致；`cargo test` 通过

---

## M23：Gallery 增强（参考 Immich）

**方案**（`src/identity.rs`）：
- `GalleryEntry` 增加 `extra: Vec<Vec<f32>>`（serde default，旧 JSON 的单一 `embedding` 字段天然兼容）；`find` 对 primary+extra 取最大余弦
- **增量更新**（Immich 式样本多样化）：匹配成功且新嵌入与 primary 余弦 < `update_min_sim`（默认 0.5）且 `extra.len() < max_extra`（默认 4）时，追加嵌入并 save
- **合并**：`Gallery::merge(src, dst)`——嵌入并入 dst、删除 src；输出目录合并（同名文件加 `__{src}` 后缀避免覆盖）
- **删除**：`Gallery::remove(label)` + 可选删除对应输出子目录
- 配置：`[identity] update_gallery = true, max_extra = 4, update_min_sim = 0.5`

**验收**：旧格式 gallery 可读；单测覆盖增量更新/合并/删除/兼容；e2e：误并两人 → 合并操作 → 重跑复用标签

---

## M24：GUI main.rs 模块化重构

**现状**：`sepiola-gui/src/main.rs` 约 700 行，全部挤在一个文件。

**拆分**（纯重构，行为不变）：
- `main.rs`：入口 + App 组装（<80 行）
- `state.rs`：App 状态、`Row`、`build_config`、`discover_model`
- `worker.rs`：后台批处理线程与 `BatchEvent` 通道
- `actions.rs`：delete_unkept / open_output_location / rename / merge / remove 人物操作
- `views.rs`：顶部控制面板 + 结果表格 + 状态栏
- `dialogs.rs`：放大查看 Window、人物编辑/合并/删除 Window

**验收**：`cargo check -p sepiola-gui` 与 `cargo test` 通过；GUI 冒烟启动正常；各模块 <250 行

---

## M25：GUI 人物管理（依赖 M23、M24）

- 人物列操作扩展：**合并到…**（下拉选择其他 person）、**删除人物**（勾选是否同时删输出目录）
- 对齐模型路径设置项（`landmark_model`，发现逻辑同 ArcFace：exe 目录→工作目录）
- 调用 M23 的 `merge/remove`，同步更新行内路径与 gallery 文件

**验收**：GUI 内完成合并/删除后重跑批处理，标签与目录状态一致

---

## M26：后续事项补齐（示例 LUT、批次取消、参数保存/载入、拖拽导入）

**背景**：PLAN_COMPLETION.md 第 4 节列出的待办中，"示例 LUT 缺失""批次取消未实现""参数配置无法保存/载入""目录拖拽导入未支持"四项可在本轮补齐。

**方案**：
- `luts/portrait.cube`、`luts/landscape.cube`（33³、sRGB 编码域）：人像走柔和 S 曲线 + 暗部抬起 + 肤色轻微暖化；风景走对比/饱和提升 + 冷阴影暖高光。随仓库提供，`config.example.toml` 的引用开箱可用
- `pipeline::run_batch_cancellable(cfg, on_event, cancel: &AtomicBool)`：置位后未开始的图片记为 `cancelled`（写入 CSV 状态列），已开始的跑完；新增 `BatchEvent::Cancelled { remaining }` 与 `BatchSummary.cancelled`，CLI 汇总行在有取消时追加 cancelled 数
- `Config` 派生 `Serialize`，新增 `to_toml` / `save_file` / `load_file`；GUI 增加"保存参数…""载入参数…"按钮，载入后回填全部界面字段（模型路径仍按 exe/工作目录发现）
- GUI 接受窗口拖入的目录或图片：设为输入目录，输出目录为空时自动填 `<输入>/out`

**验收**：`cargo test` 通过（含取消跳过、配置 TOML 往返与保存/载入）；示例 LUT 生效（实测人像输出 RGB 偏暖）；GUI 可保存/载入参数并能拖入目录

---

## M27：人物识别评测集扩展与阈值扫描工具

**背景**：PLAN_COMPLETION.md 第 4 节指出"当前 `test_data/` 为 15 + 15 张样例，对侧脸、遮挡、合影、逆光等困难样本覆盖不足"，且阈值（0.35）缺少数据支撑。

**方案**：
- 引入 LFW 子集（52 位人物 / 80 张，来自 GitHub `CseMony/lfw_dataset` 的 `lfw_train`）落到 `test_data/identity/<人物名>/<图片>.jpg`，目录名即真实标签；LFW 天然包含侧脸、遮挡、逆光、戴镜与多人同框等困难样本
- 新增 `src/eval.rs`：加载评测集（逐图取最大人脸，与生产流程一致，检测+对齐+嵌入），两两比较后给出每个阈值下的精确率/召回率/F1、聚类数、误合并簇数与被拆分人物数（并查集统计）
- 新增 `src/bin/sepiola-eval.rs`：`sepiola-eval <评测集目录> [-c config.toml] [--min/--max/--step]`，输出扫描表、F1 最佳阈值，并标记当前配置阈值所在行
- `FaceIdentifier::embedding_for` 公开只读嵌入接口（不写 gallery），供评测复用同一条对齐链路

**验收**：`cargo test` 通过（含 5 项 eval 指标单测）；在 LFW 子集上实跑扫描表，最佳阈值 F1 ≥ 0.95 且能定位误合并/拆分区间

---

## M28：媒体库（库目录、按类型归类、图片相册、浏览/重打标签/移动）

**背景**：用户提出引入"库"的概念——指定一个目录作为库来源，自动按文件类型归类（第一版先做图片，后续扩展到文档/图书/视频/安装包/音频等），图片在逻辑上拆分到分类相册（人像、风景，后续日出/天文/植物动物/聚会/生日/某人等）；RAW 显影只是库的一项功能，还需要浏览、重新打标签、移动到不同相册。同时 GUI 布局偏紧凑，需要拉开组件间距。

**方案**：
- 新增 `src/library.rs`：
  - `FileKind`（Image/Video/Audio/Document/Archive/Other）按扩展名归类，v1 只对 Image 做深度处理，其余类型仅入库分桶，扩展时加映射即可
  - `LibraryEntry { rel_path, kind, album, person, manual }`，索引持久化在 `<库根>/.sepiola/library.json`（扫描时跳过 `.sepiola` 与隐藏目录）；人物 gallery 独立存放于 `<库根>/.sepiola/identities.json`，与批处理输出目录的 gallery 互不干扰
  - `scan`：增量对比索引（新增/删除/保留），新图片跑人脸检测→相册（人像/风景）+ `FaceIdentifier` 人物标签；模型缺失时回退"未分类"相册并报告；发 `LibraryEvent`（Discovered/Processed/Done）供 GUI 显示进度
  - `retag(root, rel, album)` 重新打标签（置 `manual`，自动归类不再覆盖）；相册是逻辑分组，不移动磁盘文件
  - 相册枚举 v1：`人像`（含"某人"person 子标签）、`风景`、`未分类`；日出/天文/植物动物/聚会/生日等待后续分类器接入，接口保持 String 相册名
- 新增 `src/bin/sepiola-lib.rs` CLI：`scan <目录> [-c config]`、`albums <目录>`、`list <目录> [--album 名称]`、`retag <目录> <相对路径> <相册>`
- GUI：
  - 顶栏加"处理 / 库"两个页签，中央区按页签切换；批处理视图与库视图互不干扰
  - 新增 `sepiola-gui/src/library.rs`（状态 + 后台扫描线程 + 视图，<250 行）：选库目录、扫描（带进度与取消复用 cancel 模式）、相册列表（含计数）、选中相册的图片列表、下拉框重打标签、显示 person 子标签
  - 布局间距：`style_mut` 全局 `item_spacing` 加大（8→12 纵向），分隔线后补空段，左右面板内边距增加

**验收**：`cargo test` 通过（含扩展名归类、增量扫描 diff、retag 持久化与 manual 保护、索引往返）；`sepiola-lib` 在 `test_data/identity` 上实跑 scan/albums/list/retag；`cargo check -p sepiola-gui` 通过且 GUI 冒烟存活；界面组件不再挤压

---

## M29：场景标签模型（细分相册）

**背景**：M28 的相册 v1 只有"人像/风景/未分类"。用户要求先做细分相册的场景标签模型，把非人像图片按内容细分到「植物动物 / 生日 / 聚会」等相册（日出/天文 ImageNet 无直接对应类，留作后续专项分类器）。

**方案**：
- 模型：ImageNet-1k 图像分类 ONNX（MobileNetV2 级别，体积小、CPU 推理快），标签映射（id→class name）随模型落地 `models/`
- 新增 `src/scene/tags.rs` `SceneTagger`：
  - 复用 `face.rs` 的 ort 模式（共享 `ort_environment()`、`Mutex<Session>`）
  - 预处理：短边缩放到 256 → center crop 224 → RGB /255 → ImageNet mean/std 归一化 → NCHW；是否烘焙预处理以样本图 top-1 实测为准（InsightFace 烘焙、Xenova 导出通常不烘焙，不可假设）
  - 输出 softmax top-k；标签→相册映射表（植物动物/生日/聚会/风景等，多类聚合到少数相册名），未命中回退"风景"（无脸图）或"未分类"（推理失败）
- `Config` 新增 `[scene_tags]` 段：`model`、`labels`、`enabled`、`min_confidence`
- `library.rs` `auto_album` 非人像分支接入 tagger：人像逻辑不变（人脸优先），无脸图片由 tagger 决定相册；tagger 模型缺失时保持 M28 行为（"风景"）
- GUI：库文件列表展示自动相册即现有列，无需新增控件；扫描配置透传 `[scene_tags]` 模型路径
- 验证：用 ImageNet 官方样本图（如 golden retriever、birthday cake、jellyfish 等）实测 top-1 正确性；`sepiola-lib scan` 端到端确认细分相册落地

**验收**：`cargo test` 通过（含映射表与预处理单测）；样本图 top-1 分类正确率验证归一化无误；`sepiola-lib scan` 在非人像图片上产出「植物动物/生日/聚会/风景」等相册；模型缺失时回退不报错

---

## M30：库页签浏览性能与文件操作（虚拟化 / 点击预览 / 右键菜单）

**背景**：用户反馈库页签三个缺陷——巨大文件目录滚动卡顿（全量渲染 Label）、点击行无法浏览图片（仅选中无预览）、缺少右键菜单进行文件操作。

**方案**：
- 虚拟化：`ScrollArea::show_rows` 固定行高 24px，只渲染可见区间；行数据仅存索引 `Vec<usize>`，避免每帧克隆全部路径
- 点击预览：行可点选，右侧新增预览面板（`Panel::right`），选中图片即时解码为纹理（同路径复用不重复解码；RAW/视频等解码失败给出提示）
- 右键菜单（`Response::context_menu`）：打开所在目录（复用 `open_in_file_manager`）、复制绝对路径（`ctx.copy_text`）、移动到相册（子菜单：草稿相册 + 现有/内置相册，复用 `retag`）、从索引移除（新增 `library::remove_from_index`，仅改索引不动磁盘文件）
- 菜单动作延迟到列表循环结束后执行，避免迭代中重载索引导致越界
- GUI 模块拆分：文件操作独立为 `library_ops.rs`，各模块保持 <250 行

**验收**：`cargo check -p sepiola-gui` 零告警；`cargo test` 通过（含 `remove_from_index` 单测）；GUI 冒烟存活；万级目录滚动仅渲染可见行

---

## M31：后台缩略图生成与磁盘缓存（库页签）

**背景**：M30 的文件列表仅有文字行，浏览图片库需要可视化；差异记录中已把后台缩略图留作后续（参考 Immich thumb 管线）。

**方案**：
- 后端 `src/thumb.rs`：缓存路径 `<库根>/.sepiola/thumbs/{max_side}/{FNV-1a64(相对路径)}.jpg`；手写 FNV-1a 64 保证跨进程稳定（`DefaultHasher` 种子每进程随机不可用）；`generate` 缓存 mtime 不早于源文件则复用，否则解码 → `imageops::thumbnail` → JPEG(80)；源缺失/不可解码（RAW 等）返回 `Ok(None)`，不落错误缓存
- GUI `library_thumbs.rs`：惰性启动单个后台线程消费相对路径任务 → 生成 → 读回像素 → channel 发回；UI 线程 `poll_thumbs` 加载纹理并重绘；每个 rel 只投递一次（失败也占位防重试风暴）
- 列表行高 24→40，行左侧 40×40 缩略图槽（等比居中、缺失时灰色占位并投递任务）；右键菜单抽为 `library_ops.rs::row_context_menu`，缩略图槽与标签按钮共用
- 切换库目录（选择/拖拽）时 `reset_thumbs` 清空纹理与任务（drop sender 使旧 worker 退出）；有在途任务时 100ms 低频重绘

**验收**：`cargo check -p sepiola-gui` 零告警；`cargo test --workspace` 通过（含 thumb 3 项单测：哈希稳定分桶、缓存新鲜复用不重写、不可解码返回 None）；GUI 冒烟存活；各 GUI 模块 <250 行

---

## M32：库内 RAW 一键显影

**背景**：库页签中 RAW 无法预览/缩略图（image crate 不能直接解码 RAW），媒体库后续事项中"库内 RAW 一键显影"落地。

**方案**：
- 后端 `library::developed_path`：产物 `<库根>/.sepiola/developed/<镜像相对路径>.jpg`（保留源扩展名，如 `sub/a.nef.jpg`，避免同 stem 不同扩展互覆盖；镜像子目录避免同名冲突；`.sepiola` 隐藏目录扫描天然跳过，不会把产物当输入）
- 后端 `library::develop_file`：复用 `develop::raw_to_tiff`（auto/darktable/builtin 三后端）→ `imageio::load` → `save_jpeg`（质量取 `config.jpeg_quality`）；产物 mtime ≥ 源则幂等复用；中间 TIFF 放独立 `.work-{pid}-{seq}` 工作目录防并行互踩，完成后清理
- 缩略图回退：`thumb::generate` 解码源失败时尝试已显影产物，RAW 显影后缩略图自动可生成
- GUI：RAW 行右键新增"显影为 JPEG"（`RowAction::Develop`）；后台线程执行 + `dev_rx` 轮询，`dev_inflight` 防同文件重复请求；成功后状态栏提示、自动刷新选中项预览、`thumb_requested.remove` 允许缩略图重新请求
- 预览回退：`select_library_file` 源解码失败时改开产物；预览面板提示语更新为引导右键显影
- 显影后端/质量沿用处理页当前选择（`self.backend`/`self.jpeg_quality`）

**验收**：`cargo check -p sepiola-gui` 零告警；`cargo test --workspace` 通过（含真实 NEF 内置后端显影产物可解码、幂等复用、工作目录清理、缺失源报错等单测）；GUI 冒烟存活

---

## M33：库内 RAW 批量一键显影

**背景**：M32 只支持逐行右键显影单个 RAW；媒体库后续事项"批量一键显影"落地。

**方案**：
- GUI `library_view.rs`：顶栏新增"全部显影"按钮，作用范围跟随当前视图（选中相册或全部），无待显影 RAW 时禁用并提示；队列在途时显示"显影 done/total"进度条
- 新增 GUI `library_develop.rs`：显影改为**单个后台 worker + 任务队列**（替代 M32 每文件起线程的做法，避免数百 RAW 时线程爆炸与磁盘争用）；`enqueue_develop` 以 `dev_inflight` 去重；`develop_all` 一次性入队当前视图全部 RAW；`poll_develop` 维护 done/ok/fail 计数，单文件保留"已显影 → 产物"提示，批量显示进度与"成功 X，失败 Y"汇总
- 后端不变：批量 = 重复调用 `library::develop_file`（幂等，已显影且产物新鲜的直接复用不重做）；显影后端/质量仍沿用处理页选择
- 切换库目录（选择/拖拽）时 `reset_develop` drop sender 退出旧 worker 并清空在途状态

**验收**：`cargo check -p sepiola-gui` 零告警；`cargo test --workspace` 通过；GUI 冒烟存活；库模块各 <250 行

---

## M34：场景分类细化（Places365 场景模型 + 相册扩展，修复风景误兜底）

**背景**：M29 只映射植物动物/生日/聚会三类，其余无脸图一律兜底"风景"，导致 PPT 截图等被误归风景；用户要求细化场景分类并修复该缺陷。

**方案**：
- 引入 Places365 场景专用模型：ailia-models `resnet18_places365.onnx`（45MB，源自 CSAILVision Places365-CNNs，storage.googleapis.com 直连慢需断点续传）+ 官方 `categories_places365.txt` 类表；实测首节点为 Conv（预处理未烘焙），按官方脚本复刻：拉伸 256×256（不保宽高比）→ center crop 224 → RGB/255 → ImageNet mean/std → NCHW，输入名 `input_img`
- 新增 `src/scene/places.rs`：PlacesTagger + 365→相册显式映射（风景/建筑/交通工具/运动/冒险/探险/美食/聚会），未命中返回 None；风景只来自显式自然类
- 归类顺序改为：人脸→人像；ImageNet 物体类（植物动物/生日/聚会/美食/截图·文档，新增 screen/desktop computer/notebook/computer keyboard→"截图/文档"、pizza 等→"美食"）；Places 场景类；**兜底"未分类"（彻底移除风景兜底）**
- 新相册常量：建筑/交通工具/美食/运动/冒险/探险/截图·文档；GUI 侧栏自动从索引计数派生，零改动
- 配置扩展：`[scene_tags]` 新增 places_model/places_classes/places_min_confidence（默认 0.25），模型缺失静默跳过 Places 细分
- 索引迁移：LibraryIndex 增加 version 字段（serde default 0），v1 旧索引首次扫描对全部非 manual 图片重归类一次后升为 2
- 已知局限：Places365 与 ImageNet 均无 日出/天象/截图 类，日出/天象仍留待专项分类器；PPT 截图靠 ImageNet screen 类或落入未分类

**验收**：真实模型回归（风景照→lagoon→风景、合成 PPT→不判风景）；端到端扫描 7 张混合样本相册分布正确；`cargo test --workspace` 118 项通过；GUI 冒烟存活

---

## M35：库第一级文件类型总览（类型分桶扩展 + 项目/游戏实体识别）

**背景**：库页此前直接展示文件列表，图片只是其中一个类型；用户要求第一级按文件类型（文档/书籍/音频/视频/录音/图片/驱动/安装包/项目/游戏）统计个数，点击类型图标才进入对应浏览页（先实现图片），且项目/游戏要识别为单个实体、内部文件不列出。

**方案**：
- `FileKind` 扩展：新增 Recording/Book/Driver/Installer/Project/Game（epub/mobi/azw3/fb2/djvu→书籍；exe/msi/deb/rpm/apk/dmg 等→安装包，从压缩包桶拆出；inf/sys/cat→驱动；音频扩展名+文件名含 录音/record/voice/通话 等关键词→录音）；`ALL_KINDS` 定义总览展示顺序，`label()` 提供中文名
- 实体识别：`walk` 遇到目录先查标记文件——游戏（steamapps 目录、appmanifest_*.acf、*.pck）优先于项目（.git/.hg/.svn/Cargo.toml/package.json/go.mod/pom.xml/build.gradle/CMakeLists.txt/pyproject.toml/*.sln/*.csproj）；命中即折叠为单个 Project/Game 条目（相册名=目录名），不再递归，内部文件不入库
- `LibraryIndex::kind_counts()` 提供类型→条目数；索引 version 升 4：v3 及更旧索引首次扫描对全部非 manual 条目重判类型（旧"文档/压缩包"改判书籍/安装包等）
- GUI 新增 `library_overview.rs`：库页默认显示总览（目录选择+扫描控制+类型计数卡片网格，按类型配色）；仅"图片"卡片可点，进入原浏览页（`library_view` 顶部改为"← 返回总览"+显影按钮，列表只列图片）；其余类型卡片置灰提示待后续里程碑
- 已知局限：录音靠文件名启发式（无语义分析）；游戏标记集保守（Steam/Godot 形态），更多游戏形态待样本反馈扩充；非图片类型仅有计数，浏览页未实现

**验收**：`cargo test --workspace` 121 项通过（含新类型归类、实体折叠、v4 迁移 3 个新用例）；CLI 端到端混合目录 7 条目=audio/book/game/image/installer/project/recording 各 1，实体内部文件不入库；GUI 各模块 ≤250 行

---

## M36：电子书内容级元数据抽取（EPUB/FB2/MOBI/PDF + 书籍浏览页）

**背景**：M35 后书籍类型仅有计数与文件列表，元数据依赖文件名不可靠；用户要求直接读取电子书内容判定作者、出版年份、出版社、译者、版本等，而非仅 metadata/文件名推断。范围经确认：EPUB+FB2+MOBI/AZW3+PDF 全部格式，并一并实现书籍浏览页。

**方案**：
- 新建 `src/book.rs`：`BookMeta{title,author,publisher,year,translator,edition,isbn}`（全 Option）；`extract(path)` 按扩展名分派，全字段缺失返回 `None`
  - EPUB：zip 读 `META-INF/container.xml` → OPF Dublin Core（dc:title/creator/publisher/date/contributor，role="trl" 或含"译"判译者）；再取前若干部 XHTML 去标签正文做启发式补齐（"X 著/X 译/译者：/第X版/修订版/ISBN/19xx-20xx年"）
  - FB2：quick-xml 事件解析 title-info（book-title/author/translator/publisher/pubdate/ISBN）
  - MOBI/AZW/AZW3：手写 PalmDOC/EXTH 解析——扫描 "MOBI" 魔数定位 record0，读 EXTH 记录 100 作者/101 出版社/102 出版日期/106 ISBN/503 书名，书名兜底 DB name；加密文件返回 None；正文级字段不解析
  - PDF：`pdf-extract` 抽文本层前 30K 字符走同一套启发式；扫描版无文本层返回 None；djvu 暂不支持
- `LibraryEntry` 新增 `#[serde(default)] book_meta: Option<BookMeta>`；scan 对 Book 类型抽取入库；索引 version 升 5：v4 及更旧索引仅回填缺元数据的非 manual 书籍条目（不做全量重归类）
- GUI：`LibraryPage` 新增 `Books`，总览"书籍"卡片可点进入 `books_view.rs` 表格页（书名/作者/出版社/年份/译者/版本/ISBN/文件 8 列，按书名排序，缺字段显示"—"，顶部返回按钮）
- 依赖新增：`zip`（deflate）、`quick-xml`、`pdf-extract`

**验收**：book 7 项单测（合成 EPUB/FB2/MOBI fixture、手工构造文本层 PDF、版权页启发式、损坏文件返回 None）+ v5 回填迁移单测通过；`cargo test --workspace` 129 项通过、零告警；CLI 端到端 FB2 扫描后 `library.json` 含 book_meta；GUI 冒烟存活、各模块 ≤250 行

---

## M37：GUI 用户设置持久化与最近书库

**背景**：GUI 关闭后会丢失最后一次使用的输入/输出目录、处理参数和书库目录；多书库用户每次启动都需重新选择目录。

**方案**：
- 新增 GUI 用户设置模块，以 JSON 保存到操作系统用户配置目录（Linux 遵循 `XDG_CONFIG_HOME`，缺省 `~/.config/sepiola/`；macOS/Windows 使用各自用户配置目录）；文件缺失或损坏时安全回退默认值
- 持久化 GUI 当前处理设置（输入/输出目录、显影后端、分类器、线程数、质量/阈值、模型/LUT、像素格式、元数据开关、人物识别设置）及当前书库目录
- 保存最近使用的 8 个书库目录，最近使用的排在前面并去重；目录选择/拖入时更新历史，库总览页提供最近书库下拉选择，切换后重新加载索引并清理旧库预览/后台任务
- 用户设置只在值变化时写盘，避免每帧 I/O；启动时恢复最后使用的设置和书库
- 单元测试覆盖 JSON 往返、历史目录去重/上限/顺序、缺失或损坏配置回退

**验收**：GUI 用户设置在进程重启后恢复；最近书库列表可切换且索引同步刷新；`cargo test -p sepiola-gui` 与 `cargo check -p sepiola-gui` 通过；手动检查配置文件位于用户配置目录且不会写入库目录。

---

## M38：书籍元信息筛选与正文搜索

**背景**：M36 的书籍页目前仅展示表格，无法按常见格式或内容级元信息快速筛选，也不能搜索书籍正文。

**方案**：
- 以当前书籍索引构建格式、作者、出版社、年份、译者、版本、ISBN 的去重分面；书名不生成下拉分面，避免全库一书一项；每个分面选项显示命中数，按数量降序排列（同数按名称排序），各分面可组合筛选
- 增加文本搜索，默认跨所有书籍元信息及文件名做不区分大小写匹配；筛选条件与文本搜索同时生效
- 增加默认关闭的「搜索正文」选项；仅用户勾选并触发搜索后，后台线程检索当前书库书籍，正文命中与元信息命中取并集；格式分面仍与搜索结果取交集；切换书库或取消时停止旧任务
- 正文读取支持 EPUB XHTML、PDF 文本层、FB2 XML 文本及 MOBI/AZW 的未压缩和 PalmDOC 压缩文本；扫描版 PDF、加密或不支持的 MOBI 压缩格式无正文命中
- 增加分面计数/排序、组合筛选、跨字段搜索及正文格式/解压测试

**验收**：`cargo test --lib book::`、`cargo test -p sepiola-gui` 通过；下拉只提供非书名元信息分面并按命中数倒排，书名仍可由默认文字搜索命中；多个元信息筛选可组合；正文搜索默认关闭、显式启动且不阻塞 UI，匹配结果与元信息搜索合并；切换书库后不显示旧库筛选或正文结果。

---

## M39：补录遗漏功能与递归扫描取消修复

**背景**：工作区已落地部分能力，但此前没有完整写入任务计划；此外递归枚举阶段的取消请求无法及时停止。

**已落地内容**：

- HEIC/HEIF 支持：使用 `libheif-rs` 解码，并接入图像读取、库分类与缩略图路径。
- 图片场景识别补充 MobileCLIP 能力，并保留现有 ImageNet/Places365 分类链路。
- 补录书籍处理拆分、调色风格模板与预览图的实现记录，避免把这些已有功能误记为待实现。
- 递归扫描遍历目录和目录项时检查取消标志；取消时不保存不完整的扫描索引、不把未遍历文件误判为删除，并向 GUI 返回明确取消状态。

**验收**：扫描取消在遍历阶段可中止且保留旧索引完整性；HEIC/HEIF 与场景模型读取路径可用；既有书籍拆分、风格模板和预览图实现保持不变。

---

## M40：双目录比较、同步与精确重复清理

**背景**：需要从主菜单比较任意两个目录，并可按文件、图片、书籍或音乐查看差异、选择方向同步或保留一侧清理重复文件。

**方案**：

- 新增主菜单「比较」页，可选择或拖入 A/B 目录；拒绝相同、嵌套或不存在的目录。
- 后台递归比较并可取消；文件按 SHA-256、名称、修改时间比较，支持相对路径配对及内容相同但路径不同的改名识别。
- 图片以 dHash 提供感知相似度候选；相似候选只展示，不得用于清理。HEIC/HEIF 复用项目解码路径。
- 书籍比较 SHA-256、文件名/书名、修改时间、作者、出版社、版次和译者；音乐比较标签可读性、作者、录音日期和指挥（`lofty`）。
- 提供 A→B、B→A 分类同步；执行前确认，明确覆盖同相对路径文件。
- 提供保留 A/保留 B 的精确重复清理；只对当前分类计算 SHA-256 完全相同项，二次确认，删除前重新校验保留侧仍有副本。
- 新增 `sha2` 与 `lofty` 依赖；覆盖字段差异和感知相似度边界的单测。

**验收**：四类差异字段与同步范围符合定义；后台任务可取消；精确副本删除经过二次确认和执行前 hash 复核；图片感知相似项不会被删除；GUI 与比较引擎测试通过。

---

## 分期建议

| 阶段 | 包含 | 依赖 |
| --- | --- | --- |
| **第一阶段** | M0、M1、M2、M3、M4 | 纯 Rust，无外部程序 |
| **第二阶段** | M5、M6 | 需安装 darktable-cli |
| **第三阶段** | M7 | 依赖 M6 |
| **第四阶段** | M8、M9、M10、M11、M12、M13、M14、M15、M16 | 需引入模型文件与 ONNX Runtime；M16 需真实 RAW 样本 |
| **第五阶段** | M17 | 桌面 GUI，需图形环境与 eframe 依赖 |
| **第六阶段** | M18、M19、M20、M21 | 导出选项、结果保留/删除、打开输出位置、人物身份识别（需 ArcFace 模型与 ONNX Runtime） |
| **第七阶段** | M22、M23、M24、M25 | 参考 digiKam/Immich：关键点对齐、gallery 多嵌入与合并/删除、GUI 模块化与人物管理 |
| **第八阶段** | M26 | 示例 LUT、批次取消、参数保存/载入、拖拽导入 |
| **第九阶段** | M27 | 人物识别评测集（LFW 子集随仓库提供）与阈值扫描工具 |
| **第十阶段** | M28 | 媒体库（类型归类、相册、浏览/重打标签）与 GUI 页签、间距调整 |
| **第十一阶段** | M29 | 场景标签模型（ImageNet ONNX）细分相册：植物动物/生日/聚会等 |
| **第十二阶段** | M30 | 库页签文件列表虚拟化、点击预览、右键文件操作菜单 |
| **第十三阶段** | M31 | 后台缩略图生成与磁盘缓存（库页签文件列表） |
| **第十四阶段** | M32 | 库内 RAW 一键显影（右键触发，产物入 `.sepiola/developed/`，预览/缩略图联动） |
| **第十五阶段** | M33 | 库内 RAW 批量一键显影（"全部显影"按钮，单 worker 队列，进度与成功/失败统计） |
| **第十六阶段** | M34 | 场景分类细化（Places365 场景模型、新增 7 个相册、移除风景误兜底、索引 v2 迁移） |
| **第十七阶段** | M35 | 库第一级文件类型总览（书籍/录音/驱动/安装包分桶、项目/游戏实体折叠、索引 v4 迁移、总览卡片页） |
| **第十八阶段** | M36 | 电子书内容级元数据抽取（EPUB/FB2/MOBI/PDF 解析书名/作者/出版社/年份/译者/版本/ISBN、索引 v5 回填、书籍浏览页） |
| **第十九阶段** | M37 | GUI 用户设置持久化（处理参数、当前书库、最近 8 个书库目录与快速切换） |
| **第二十阶段** | M38 | 图书元信息分面筛选、跨字段文字搜索与用户显式启用的后台正文搜索 |
| **第二十一阶段** | M39 | 补录 HEIC/HEIF、MobileCLIP、书籍处理/调色预览，以及递归扫描取消修复 |
| **第二十二阶段** | M40 | 四类双目录比较、双向同步与 SHA-256 精确重复清理 |
| **第二十三阶段** | M41 | 音频标签归一化、索引迁移与音乐元数据浏览/筛选 | 依赖 Lofty 与现有媒体库索引 |
| **第二十四阶段** | M42 | AcoustID 增量数据下载、离线导入与已知录音指纹匹配 | 需按需下载数据并评估存储；不提供跨演奏作品识别 |
| **第二十五阶段** | M43 | 古典作品/乐章归组与有限曲库的无模型候选识别 | 依赖 M41；需人工标注语料评估 |
| **第二十六阶段** | M44 | 电子书阅读器（EPUB/MOBI/AZW3/TXT/PDF，章节导航与进度记忆） | 依赖 M36 书籍索引；纯 Rust 解析，不调用外部程序 |
| **第二十七阶段** | M45 | 阅读体验增强（跨章连续滚动、选区工具条、字体/主题/版面设置、翻译与 AI 解释） | 依赖 M44；egui 原生实现，外部能力走可配置接口 |
| **后续** | v0.2 白平衡、v0.3 肤色保护... | 均以新增 Stage 方式加入 |

---

## 路线图

| 版本 | 功能 | 实现方式 |
|------|------|----------|
| v0.1 | 核心流程：RAW 转换、场景判断、LUT 调色、批处理、对比图 | M0～M8 |
| v0.2 | 自动白平衡 | `grade/white_balance.rs`，灰世界/高光白点等传统算法 |
| v0.3 | 肤色保护 | `grade/skin_protect.rs`，限制人脸区域色偏和饱和度变化 |
| v0.4 | 自动生成 LUT | Python 工具跑 3D LUT 模型，输出 `.cube`，Rust 只负责应用 |
| v0.5 | 学习个人风格 | 用「原图/成品」配对拟合个人 LUT |
| v0.6 | 桌面界面 | egui 最小实现已由 M17 提前落地（workspace 成员 `sepiola-gui`）；后续可迁移 Tauri 或拆分 `sepiola-core` |
## 音频库后续计划

## M41：音频元数据入库与浏览

**背景**：M35 已将音频与录音分桶，M40 仅在目录比较时临时读取少量音乐标签；媒体库尚不能浏览、搜索或筛选音乐元数据。

**方案**：

- 扩展 `src/library.rs` 音频条目元数据，至少记录作曲家/作者、作品名、指挥、演奏者、乐团、乐章、录音日期、专辑、曲目/碟号及厂牌；缺失或无法解析的字段保持为空，不覆盖源文件标签。
- 用 Lofty 读取常见音频标签；对不同标签标准的同义字段做归一化，同时保留原始值供后续修正。索引增加向前兼容字段，旧库扫描时按需补提取音频元数据。
- GUI 库总览的「音频/录音」类型进入音频表格页，展示元数据并支持关键词搜索、作曲家/指挥/演奏者筛选和作品/乐章排序；只修改库索引，不改音频文件。
- 本阶段只建元数据浏览基础，不承诺凭标签自动认定不同录音属于同一作品；该行为由 M43 负责。

**验收**：标签测试覆盖 ID3、Vorbis comments 与常见 MP4 标签；缺失/损坏标签可容错；旧索引可读取；音频页搜索、乐章顺序和空字段展示有测试。

## M42：AcoustID 更新数据下载与本地录音指纹库

**背景**：AcoustID 提供按日发布的 JSONL.GZ 数据更新，可用于本地匹配已知录音；其 Chromaprint 指纹标识录音，不等于跨演奏版本的作品识别。

**方案**：

- 使用 `sepiola-acoustid download` 按日期范围和数据流选择下载官方增量文件；支持 dry-run、续传、已完整文件跳过与失败重试，默认经 `http://localhost:7890` 代理并输出到 `data/acoustid/`。Chromaprint 纯 Rust 实现负责指纹生成，Symphonia 纯 Rust 解码音频，不调用 `fpcalc` 或 Python。避免默认拉取全部历史数据，按需选择日期与更新类型。
- 新增导入/重建命令，按官方更新流顺序构建本地录音及指纹索引；导入需可重复执行、支持断点恢复，并在事务中应用单日更新，避免中断留下半更新数据库。
- 新增本地录音识别接口：为用户音频计算 Chromaprint，与本地索引匹配并返回录音/曲目元数据及匹配分数；无匹配时明确返回未知，不将其伪装成作品识别结果。
- 记录数据来源与许可：AcoustID 数据通常为 CC BY-SA 3.0，AcoustID–MusicBrainz 映射为 CC0；在线 Web Service 有非商业及请求速率限制。官方数据库说明见 `https://acoustid.org/database`。

**验收**：小型固定数据集可从空库重建且重复导入结果不变；中断后可续跑；已知录音指纹可命中其曲目、未知录音不误报；完整数据下载与数据库重建需单独评估磁盘空间和耗时。

## M43：古典作品与乐章归组及无模型候选识别

**背景**：用户希望同一作品的不同录音能够归组，并按乐章顺序排列；AcoustID 录音指纹不能直接关联不同演奏版本，因此需单独设计作品层匹配。

**方案**：

- 为作曲家、作品编号/作品名、乐章序号/名称、编制等建立规范化作品与乐章字段；优先使用音频标签、MusicBrainz/AcoustID 关联元数据和人工确认结果。
- 以规范化作曲家 + 明确作品编号为高置信度键；对仅有标题、乐章名或不完整标签的项目提供候选列表，不自动强行合并。乐章按显式序号排序，无法解析序号时再按标题与文件名稳定排序。
- 无机器学习模型时，允许针对有限参考曲库试验传统音频特征（如 chroma、节奏与结构相似度）生成跨演奏版本候选；候选须附证据/置信度，低置信度交由用户确认。该方案不承诺对任意古典录音普遍识别。
- GUI 展示作品分组和乐章顺序，允许拆分、合并及修正作品字段；人工修改持久化且不会被后续自动扫描覆盖。

**验收**：用包含 Beethoven、Mozart 等已知作品及多版本录音的人工标注小型语料评估；作品编号冲突不会静默合并，乐章顺序可复现；记录候选准确率、漏召回和人工修正结果。

---

## M44：电子书阅读器（EPUB/MOBI/AZW3/TXT/PDF）

**背景**：M36 只抽取书名/作者等元数据，书籍页无法打开正文。需要在库内直接阅读，覆盖 EPUB、MOBI、AZW3、TXT、PDF，解析层尽量使用纯 Rust 库，不依赖 calibre、pdftotext 等外部程序。

**方案**：

- 新增核心模块 `src/reader/`：
  - `mod.rs`：统一入口 `BookDocument::open(path)`，输出 `chapters: Vec<Chapter { title, text }>` 与书名；按扩展名分派，未知格式返回明确错误。
  - `epub.rs`：解析 `META-INF/container.xml` → OPF → manifest/spine，按书脊顺序读取 XHTML；TOC 优先 EPUB3 nav，其次 EPUB2 NCX；章节标题取 TOC 文本，缺失时回退首个标题或文件名。
  - `mobi.rs`：MOBI/AZW/AZW3 使用纯 Rust `mobi` crate 解 PalmDOC/HUFF-CDIC 压缩正文，再按标签结构化文本；加密或损坏文件返回错误。
  - `text.rs`：TXT 编码识别（BOM / UTF-8 校验 / GBK 回退）；按「第X章/节/回/卷」与 `Chapter N` 行切分章节，无标题时按段落边界分块。
  - `pdf.rs`：沿用 `pdf-extract` 文本层（纯 Rust），按换页符或段落边界分块；扫描版无文本层时提示不支持。
  - `markup.rs`：HTML/XHTML → 纯文本，保留段落与标题换行、解码实体，供 EPUB/MOBI 共用。
- 阅读进度：`.sepiola/reading.json` 记录每本书（相对路径）的所在章节与全局字号；不修改书籍文件。
- TXT 纳入书籍类型：`BOOK_EXTENSIONS` 增加 `txt`，从文档类型移除；索引版本升到 v9 触发一次旧库重归类。
- GUI 新增 `LibraryPage::Reader`：书籍表格点击书名或「阅读」按钮后台解析并进入阅读页；提供返回、上一章/下一章、章节目录、字号调整与滚动阅读；解析在后台线程，不阻塞界面；离开阅读页时保存进度。

**验收**：五种格式均可打开并显示正文与章节；EPUB 按书脊/TOC 顺序、MOBI/AZW3 可解压 HUFF-CDIC 与 PalmDOC；TXT 可识别 UTF-8 与 GBK；PDF 文本层可读；进度在重启后恢复；`cargo test --lib reader::`、`cargo test -p sepiola-gui` 通过。

---

## M45：阅读体验增强（连续滚动、选区工具条、阅读设置）

**背景**：M44 的阅读页只能按章翻页，缺少连续滚动、选中文字后的快捷操作以及字体/背景设置。成熟的 Rust 电子书库（`ebook`、`epub`、`mobi` 等）只负责解析，不提供阅读器 UI，因此继续在 egui 上原生补齐，不更换解析层。

**方案**：

- 连续滚动：阅读区维护 `stream_start..stream_end` 章节窗口，跨章拼接渲染；滚动接近底部时自动追加下一章。切换章节或目录跳转时递增 `stream_generation` 重置滚动位置。当前章节按章节块顶部（而非正文顶部）判定，回补上一章后仍能正确识别并继续前进，不会卡在上一章。
- 选中文字：正文使用只读但可选中的 `TextEdit`（`&str` 缓冲）；从 `TextEditOutput.cursor_range` 提取选区文字与选区末端坐标，浮动工具条跟随选区显示（上方空间不足时翻到下方，超出屏幕自动收边）。
- 目录：右侧抽屉，用浮层按动画宽度滑出/收起（不改变正文宽度，避免动画期间重排正文），选中章节后自动关闭。
- 工具条操作：
  - 复制：写入系统剪贴板并清除选区。
  - 翻译 / AI 解释：调用 `[reader.ai]` 配置的 OpenAI 兼容接口（`{base_url}/chat/completions`），后台线程请求，结果窗口展示并可复制；未配置时按钮禁用并提示。
  - 分享：把选中文字写入临时文件并交给系统默认程序打开，失败时回退复制到剪贴板。
  - 词典：按 `[reader.dictionary] url` 模板跳转，占位符 `{word}` 替换为 URL 编码后的选中文字（缺省追加到末尾），支持 `https://` 网站与 `eudic://` 等自定义 scheme，交给系统默认处理程序打开；未配置时按钮禁用并提示。
  - 关闭 / Esc：清除选区。
- 阅读设置：字号（12–32 px）、字体族（比例/等宽）、主题（跟随/纸白/米黄/夜间）；设置与每本书章节进度一起写入 `.sepiola/reading.json`，重启后恢复。
- 设置弹窗：顶栏只保留「目录」「阅读设置」，设置项收进 `egui::Window` 对话框（主题、字体、字号、行间距、页边距、背景/文字色、配色重置与词典地址）；弹窗边框与控件配色跟随阅读主题，改动即时生效并自动保存。首页「设置」页只保留 AI 翻译/解释，改动同样即时落盘到 `user-settings.json`。
- 系统字体：用 `fc-list` 枚举系统字体（不可用时扫描常见字体目录），设置弹窗内可搜索并按名称选择；选中后重建 `FontDefinitions` 并注册 `reader-custom` 字族（缺字形回退到内置字体链），选择随阅读进度持久化（`ReadingState.custom_font`）。
- 版面调节：行间距（1.0–2.6 倍，写入 `TextFormat.line_height`，预排版同步计算）与正文左右页边距（0–120 px，通过 `Frame::inner_margin` 收窄正文）；两者变化时清空章高缓存重新测量。
- 控件配色：主题不再用 `gamma_multiply` 生成按钮底色（深色文字会得到近黑按钮），改为在背景与文字色之间线性混合，并显式设置按钮/下拉框的前景描边、链接色、文本光标与输入框底色；只自定义背景色时按明暗自动选择文字色。
- 配置：新增 `[reader.ai]`（`base_url`、`api_key`、`model`），`base_url` 与 `model` 均非空时启用；新增 `[reader.dictionary]`（`url`，如 `eudic://dict/{word}`），`url` 非空时启用词典按钮。
- 设置页：顶栏「设置」页签可编辑词典地址模板与 AI 服务（接口地址/API Key/模型），改动立即写入 `~/.config/sepiola/user-settings.json` 并同步到已打开的阅读器；用户设置为空时回退 `config.toml`。
- 新增 GUI 模块 `reader_theme.rs`（主题配色与设置控件）、`reader_stream.rs`（连续滚动与选区捕获）、`reader_selection.rs`（工具条、翻译/AI 解释、分享）。
- 独立阅读窗口：阅读页不再占用库页路由（移除 `LibraryPage::Reader`），改用 `Context::show_viewport_immediate` 打开独立原生窗口（900×720，标题带书名）；库页停留在原页签，关闭窗口或点「关闭阅读」时保存进度并销毁窗口。
- 正文样式：`Chapter` 新增 `spans: Vec<StyleSpan>`（UTF-8 字节区间 + `TextStyle { bold, italic, mono, heading }`），EPUB 解析时记录 h1–h6、strong/b、em/i、code/pre 区间；渲染用统一的 `styled_job` 生成 `LayoutJob`（标题按级别放大，粗斜体走启动时注册的系统粗体/斜体字族，缺字体时退化为普通字形），同一函数也用于章高预排版，保证滑动窗口测量与实渲染一致。TXT/PDF/MOBI 无样式区间。
- 脚注与交叉索引：`Chapter` 新增 `links: Vec<LinkSpan>`（`LinkTarget::Chapter` / `LinkTarget::Footnote`）。`markup::parse_document` 提取 `<a href>` 与 `noteref` 标记，并把脚注容器（`epub:type="footnote"`、`role="doc-footnote"` 等）的正文从章节中移出、按 id 保存；EPUB 组装章节时把 href 解析成章内跳转或脚注文本，正文中的链接以下划线高亮，点击链接跳章、点击脚注标记弹出可复制的浮窗。
- 外观设置：字体族扩展为黑体/宋体/楷体/等宽（启动时注册系统衬线、楷体与衬线粗体字族，缺失时回退），字号 12–32 可调；主题预设之外可自定义背景色与文字色（`ReadingState.background/text_color`，空值跟随主题），背景色另提供常用调色板（纯白/纸白/米黄/豆绿/淡蓝/灰阶/黑）一键选色，改动即时生效并随阅读进度持久化。

**验收**：阅读在独立窗口打开，关闭窗口或点「关闭阅读」回到原库页并保留进度；EPUB 的标题分级、粗体、斜体与等宽代码在正文中可见，脚注标记与章节内链可点击（脚注弹窗可复制、内链跳转到目标章节），且滑动窗口预排版高度与实渲染仍一致；滚动到底部自动追加下一章且位置连续；选中文字弹出跟随选区的工具条（复制/分享/词典可用，未配置 API 时翻译与 AI 解释给出提示）；目录为右侧滑出抽屉并在选择后收起；字号、字体、主题、背景色与文字颜色切换立即生效并在重启后保持；阅读设置通过独立对话框调整，切换任意主题/自定义配色后按钮文字仍清晰可读；可选择系统字体并立即套用到正文，行间距与页边距调整后正文与章高缓存同步更新；`cargo test --lib` 与 `cargo test -p sepiola-gui` 通过。

**性能（长书籍无限滚动）**：正文按章渲染只读 `TextEdit`，已实现滑动窗口：只挂载可视区上下各约一屏的章节，顶部章节离可视区超过两屏时丢弃并用实测章高补偿 `vertical_scroll_offset`；滚到窗口顶部时回补上一章（未知章高先按正文同字体、同换行宽度预排版，与实测块高误差 < 1px，滚动偏移一次到位，不会在向上滚动时产生位移；首帧尚未实测正文宽度时推迟到下一帧回补）；远在可视区下方的章节同步裁掉。`chapter_heights` 在字号、字体族或正文宽度变化时失效重测。由此挂载章节数与书长解耦，内存与每帧成本只与屏幕附近内容相关。剩余风险：单个超长章节（EPUB 单个 XHTML 可达数十万字）仍是一个整体 galley，需要解析期再切分。

---

## M46：AI 文本分析（Copilot CLI 接入、可见文本彩标）

**背景**：`[reader.ai]` 目前只支持 OpenAI 兼容 HTTP，工具条可对选中文字翻译/AI 解释；缺少 CLI 后端，也无法对整屏正文做语言分析。目标：AI 设置可选择 Copilot CLI 等外部命令；阅读时分析当前屏幕可见文本的常用表达、词组与语法点，正文彩色标注并可打开详情。

**方案**：

1. AI 后端抽象
   - `ReaderAiSettings` 增加 `provider`（`api` / `copilot` / `command`）、`cli_path`（默认 `copilot`）、`cli_args`（默认 `-p`）。提示词作为**最后一个 argv** 追加，避免引号/空格转义；需要额外开关（如 `--allow-all-tools`）时写进 `cli_args`。
   - `enabled()` 按 provider 判定：api 需 `base_url` + `model`；copilot/command 需 `cli_path`。
   - `assist::ask` 拆为 `ask_api`（保留现状）与 `ask_cli`：`Command::new(cli_path).args(split_args).arg(prompt)`，stdout 去尾空白作为结果，非零退出附带 stderr；60 秒超时（后台线程持有 `Child` 句柄，超时 kill）。沿用现有 `Receiver<Result<String, String>>` 异步模式，不阻塞 UI。
   - 设置页 AI 区增加「后端」选择：copilot 显示 CLI 路径、参数（示例 `-p`）与「检测」按钮（执行 `--version` 或 `--help` 探测）；api 保留地址/Key/模型。配置写入 `user-settings.json`，改动即时保存。

2. 可见文本采集
   - `reader_stream` 每帧记录已挂载章节与滚动视口的交集：由 `TextEditOutput.galley`、`galley_pos`、`text_clip_rect` 与滚动视口求交，按 Row 累计 glyph 数把可见行换算成章节内 UTF-8 字节区间，存入 `ReaderState.visible_ranges: Vec<(usize, Range<usize>)>`。
   - 顶栏新增「分析本页」按钮（邻近「目录」「阅读设置」）：拼接可见区间（章节间空行，合计上限约 2500 字），保留 `(chapter, 偏移)` 映射用于回标。

3. 分析与标注
   - 新增 `src/reader/analyze.rs`：提示词要求仅输出 JSON `{"summary": "...", "items": [{"text": "...", "kind": "phrase|expression|grammar", "note": "...", "detail": "..."}]}`；解析容忍 ```json 围栏、缺字段与非法项；按 `text` 在可见区间内定位（最长优先、跳过重叠）生成 `Annotation { chapter, start, end, kind, note, detail }`。
   - 配色：词组=蓝、常用表达=绿、语法=橙；`TextFormat.background` 用低透明度同色底 + 同色下划线，与链接下划线区分。`styled_job` 与 `measure_chapter_height` 同时接收本章标注并切分区间，保证预排版与实渲染一致。
   - 交互：点击正文标注（优先于链接命中）弹出「分析详情」窗口（原文、类型、说明、扩展讲解，可复制）；「分析本页」另开结果面板按类型分组列出条目，提供「清除标注」。切换章节或关闭阅读窗口时清空，不持久化。
   - 阅读设置弹窗增加「显示分析标注」开关与三色图例。

4. 测试与验收
   - 单测：CLI 参数拼接与超时/错误映射（用 `sh -c` 或临时脚本代替 copilot）；JSON 解析（围栏、坏字段、重复词组、跨行文本）；可见区间→标注映射（CJK、重复词、区间边界）；章高预排版用例加入标注区间。
   - 验收：配置 copilot 后选中文字可翻译/AI 解释；「分析本页」能对屏幕可见文本给出词组/表达/语法条目并三色标注，点击条目或正文标注可打开详情；切换主题/字体后标注仍可读；未配置后端时按钮禁用并提示；`cargo test --lib` 与 `cargo test -p sepiola-gui` 通过。

**风险与开放问题**：Copilot CLI 的非交互参数随版本变化（`-p` / `--prompt`），因此参数模板可配置；若 CLI 需要交互确认，需在 `cli_args` 增加相应开关，检测失败时引导用户查看 `copilot --help`；长文本分析受上下文与费用限制，先只分析可见文本并截断。

---

## M47：记忆卡片笔记本（遗忘曲线复习、AI 出题与造句批改）

**背景**：阅读器能看书，但读到的词汇、表达与知识点无法沉淀复习。目标：库内提供独立「笔记本」页签，用记忆卡片记录待记忆内容，按遗忘曲线安排复习并记录每次复习时间，同时复用 M46 的 AI 后端生成习题、批改造句。

**方案**：

1. 数据模型与存储（`src/notebook/`）
   - `mod.rs`：`Notebook { next_id, cards: Vec<NoteCard> }`；`NoteCard { id, front, back, tags, created_at, updated_at, due_at, interval_days, ease, repetitions, lapses, last_reviewed_at, history: Vec<ReviewLog> }`；`ReviewLog { at, rating, prev_interval, next_interval }`。正面为待记忆内容（词/短语/问答），背面为答案或释义；时间统一存 UTC Unix 秒，界面按本地时区显示。
   - 存储：库根 `.sepiola/notebook.json`（与 `reading.json` 同目录，每个库独立）；写入先写 `.tmp` 再 rename；读取/解析失败时把原文件备份为 `notebook.corrupt-<时间戳>.json` 并回退空表，避免静默覆盖；删除卡片只移出列表，id 不复用。
   - 容量：每卡 `history` 保留最近 200 条；单库 1 万张以内全量读写（更大规模转 SQLite 列入风险）。
2. 复习调度（`src/notebook/schedule.rs`）
   - 新卡首评：Again → 10 分钟，Hard/Good → 1 天，Easy → 3 天。
   - 旧卡：Again → 10 分钟、`repetitions = 0`、`lapses + 1`、`ease = max(1.3, ease - 0.2)`；Hard → `interval * 1.2`、`ease - 0.15`；Good → `interval * ease`；Easy → `interval * ease * 1.3`、`ease + 0.15`。间隔上限 365 天，`ease` 上限 3.0；不使用随机扰动，保证可复现。
   - `preview(card) -> [Rating; 4]` 返回四档的「下次间隔」文案，复习界面直接展示，预览与实际落盘共用同一函数。
   - 队列：`due_at <= now` 的卡按到期时间升序；新卡按 `created_at` 升序、每日上限 20（常量）；未到期卡不进队列。
3. 笔记本页签（`sepiola-gui/src/notebook_view.rs`，新增 `Tab::Notebook`）
   - 左侧列表：按正/背面与标签搜索，按标签过滤，按到期/创建时间排序，显示到期与新卡数量；右侧为详情与编辑（正/背面多行编辑、标签、删除、复习历史表）。
   - 复习模式：全屏卡片，点击或空格翻面；四个评分按钮带下次间隔预览；评分后写入复习时间与调度结果，自动前进并刷新角标；支持跳过与结束复习；空队列时显示下一次到期时间。
   - 统计：今日待复习、今日已复习、连续复习天数（按本地日期）、近 30 天复习次数柱状图；均由 `ReviewLog` 现算，不额外持久化。
   - 提醒：顶栏「笔记本」显示到期角标；启动及每分钟检查（`request_repaint_after`）时若 `due > 0`，在库页顶部显示可关闭横幅「N 张卡片待复习 → 去复习」，关闭仅对本次会话有效。
4. AI 出题与造句批改（`src/notebook/ai.rs`，复用 M46 后端）
   - 后端：复用 M46 的 `provider`（`api`/`copilot`/`command`）配置与设置页「AI 服务」区，不新增配置项；把 `reader::assist` 的 `ask_api`/`ask_cli` 调用抽为阅读器与笔记本共用的入口，沿用 60 秒超时与 `Receiver` 异步结果。
   - 出题：按卡片正/背面与标签生成 3–5 道题，提示词要求仅输出 JSON `{"exercises": [{"kind": "cloze|choice|translation|correction", "question": "...", "options": ["..."], "answer": "...", "explanation": "..."}]}`；解析容忍 ```json 围栏、前后缀文本与缺字段；选择题本地按 `answer` 判分，答案与讲解可折叠查看。
   - 造句：以卡片正面为目标词/短语、背面为语义提示，用户输入句子后请求 AI，输出 `{"ok": bool, "corrected": "...", "issues": [{"span": "...", "kind": "grammar|usage|word-choice|naturalness", "explanation": "..."}], "comment": "..."}`；界面展示原文、修改稿、问题列表与总评，可复制；结果与时间写入该卡最近一次 `practice` 记录，失败时保留草稿并提示。
   - 交互约束：AI 请求全部后台执行，不阻塞 UI；未配置后端时「出题」「检查造句」禁用并提示去设置页；解析失败或超时可重试一次并展示原始返回。
5. 测试与验收
   - 单测：存储往返、损坏文件回退与备份、id 不复用；四档评分、间隔上限、ease 下限、预览与实际一致、跨时区显示；队列排序与新卡上限；AI JSON 解析（围栏、缺字段、非法项、重复词）；造句结果解析与错误映射（用 `sh -c` 或临时脚本模拟 CLI）。
   - 验收：新增/编辑/删除卡片并重启后仍在；评分后到期时间与历史正确，角标与横幅随复习实时更新；搜索、过滤与统计可用；配置 AI 后可对单卡出题并本地判分，造句可得到修改稿与问题列表；未配置 AI 时相关按钮禁用；`cargo test --lib notebook::`、`cargo test -p sepiola-gui` 通过。

**风险与开放问题**：AI 输出为自由文本，需 JSON 容错加一次重试，仍失败时展示原文供复制；复习时间依赖系统时钟，记录 UTC，检测到回拨只提示、不自动改期；系统级通知（`notify-rust`）会新增依赖且各平台权限不一致，先用应用内角标/横幅，暂不引入；卡片量过大或需多设备同步时再迁移到 SQLite/服务端。
