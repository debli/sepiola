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
