# Sepiola：RAW 照片批量自动调光调色（Rust）

> 名字来自 *Sepiola*（短尾乌贼）：能随环境改变明暗与颜色，也呼应摄影里的 sepia 色调。
> 这份文档放在项目根目录。每次让 Qwen 写代码时，把「第 0～4 节」连同当前任务一起贴给它。

## 0. 给代码生成模型的规则（每次都要贴）

1. 一次只实现**一个文件里的一个功能**，不要自行扩展需求，不要改动其他文件。
2. **不要凭记忆写第三方 crate 的 API。** 我会提供对应版本的文档或示例，只能照着写；没有提供就明确说「不确定」，不要编造函数名。
3. 所有公开函数写签名、文档注释、至少一个单元测试。
4. 禁止 `unwrap()` / `expect()`（测试代码除外），错误统一用 `crate::error::Error`。
5. 不引入我没有列出的依赖。需要新依赖时，先停下来问我。
6. 输出格式：先列「修改的文件」，再给完整文件内容，不要只给片段。
7. 我会用 `cargo check` / `cargo test` 验证，编译报错我会原样贴回来，请据此修改。

## 1. 目标与范围

输入：一个文件夹的 RAW 照片（不修改原文件）。
输出：调好光色的 JPEG（可选 16-bit TIFF），以及原图/结果对比图和处理日志。

命令行用法（目标形态）：

```
sepiola ./raws -o ./out --config config.toml
```

流程：

```
RAW ──darktable-cli──▶ 16-bit TIFF ──▶ 场景分析 SceneInfo（人像？人脸位置）
                                          │
                              按场景选择一组 Stage（可配置）
                                          ▼
              Stage 1 ──▶ Stage 2 ──▶ … ──▶ Stage N      （依次作用于同一张图）
                                          ▼
                        曝光/高光溢出检查（超标则降低强度重做一次）
                                          ▼
                              输出 JPEG + 对比图 + 日志
```

**本阶段不做**：界面；在 Rust 里解码 RAW；训练模型；降噪、锐化、放大。
后续功能见第 9 节路线图，架构上已为它们预留位置（Stage 抽象）。

## 2. 目录结构

```
sepiola/
├─ Cargo.toml                 # 包名 sepiola，二进制名 sepiola
├─ ARCHITECTURE.md
├─ config.example.toml
├─ luts/                      # .cube 文件
├─ models/                    # onnx 模型（不进 git）
├─ tests/fixtures/            # 小尺寸测试图
└─ src/
   ├─ main.rs                 # 入口，仅调用 cli::run()
   ├─ cli.rs                  # clap 参数解析
   ├─ config.rs               # 配置结构与读取
   ├─ error.rs                # 统一错误类型
   ├─ pipeline.rs             # 单张/批量流程编排（rayon）
   ├─ imageio.rs              # 读写图像，ImageF32 定义
   ├─ develop.rs              # 调用 darktable-cli：RAW → TIFF
   ├─ scene/
   │  ├─ mod.rs               # SceneInfo、FaceBox、trait SceneClassifier
   │  ├─ stub.rs              # 假实现（返回固定结果）
   │  └─ face.rs              # 人脸检测实现（后期）
   ├─ grade/                  # 所有「调光调色」逻辑
   │  ├─ mod.rs               # trait Stage、Context、build_stages()
   │  ├─ lut3d.rs             # .cube 解析 + 三线性插值（纯函数）
   │  ├─ lut_stage.rs         # LutStage：套 LUT 并按强度混合
   │  ├─ blend.rs             # 原图与结果按比例混合
   │  ├─ stats.rs             # 直方图、高光/暗部裁切比例
   │  ├─ white_balance.rs     # （路线图 v0.2）
   │  └─ skin_protect.rs      # （路线图 v0.3）
   └─ report.rs               # 对比图与 CSV 日志
```

## 3. 核心类型与接口（先定死，再写实现）

```rust
// error.rs
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("io: {0}")] Io(#[from] std::io::Error),
    #[error("image: {0}")] Image(String),
    #[error("develop failed: {0}")] Develop(String),
    #[error("lut: {0}")] Lut(String),
    #[error("config: {0}")] Config(String),
    #[error("model: {0}")] Model(String),
}
pub type Result<T> = std::result::Result<T, Error>;

// imageio.rs
/// RGB，行优先，每通道 f32，范围 0.0..=1.0，sRGB 编码（非线性）。
#[derive(Clone)]
pub struct ImageF32 { pub width: u32, pub height: u32, pub data: Vec<f32> } // len = w*h*3
pub fn load(path: &std::path::Path) -> Result<ImageF32>;
pub fn save_jpeg(img: &ImageF32, path: &std::path::Path, quality: u8) -> Result<()>;
pub fn save_tiff16(img: &ImageF32, path: &std::path::Path) -> Result<()>;

// scene/mod.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneKind { Portrait, Other }

/// 归一化坐标，0.0..=1.0，相对整张图。
#[derive(Clone, Copy, Debug)]
pub struct FaceBox { pub x: f32, pub y: f32, pub w: f32, pub h: f32 }

#[derive(Clone, Debug)]
pub struct SceneInfo {
    pub kind: SceneKind,
    pub faces: Vec<FaceBox>,        // 没有人脸则为空
}
pub trait SceneClassifier: Send + Sync {
    fn classify(&self, img: &ImageF32) -> Result<SceneInfo>;
}

// grade/mod.rs  —— 所有调整都实现这个 trait
pub struct Context<'a> {
    pub scene: &'a SceneInfo,
    pub strength_scale: f32,        // 全局强度系数，默认 1.0；安全检查失败时会减半
}
pub trait Stage: Send + Sync {
    fn name(&self) -> &'static str;
    fn apply(&self, img: &mut ImageF32, ctx: &Context) -> Result<()>;
}
/// 按某个场景的配置，创建要依次执行的 Stage 列表。
pub fn build_stages(cfg: &crate::config::SceneConfig) -> Result<Vec<Box<dyn Stage>>>;

// grade/lut3d.rs
pub struct Lut3d { /* size: usize, table: Vec<[f32; 3]> */ }
impl Lut3d {
    pub fn from_cube_file(path: &std::path::Path) -> Result<Self>;
    pub fn apply(&self, img: &mut ImageF32);          // 三线性插值，原地修改
}

// grade/lut_stage.rs
pub struct LutStage { /* lut: std::sync::Arc<Lut3d>, strength: f32 */ }
// apply(): graded = clone + lut.apply；blend(original, graded, strength * ctx.strength_scale)

// grade/blend.rs
/// out = original * (1 - strength) + graded * strength；strength 取 0.0..=1.0
pub fn blend(original: &ImageF32, graded: &mut ImageF32, strength: f32);

// grade/stats.rs
pub struct ToneStats { pub mean_luma: f32, pub clipped_high: f32, pub clipped_low: f32 }
pub fn analyze(img: &ImageF32) -> ToneStats;           // clipped_* 为像素占比 0..1

// develop.rs
pub fn raw_to_tiff(raw: &std::path::Path, out_dir: &std::path::Path) -> Result<std::path::PathBuf>;
```

## 4. 配置（config.example.toml）

每个场景有自己的 Stage 列表，Stage 的参数放在同名子表里。以后新增 Stage，只需在列表里加名字、加一个子表。

```toml
input_dir  = "/path/to/raws"
output_dir = "/path/to/out"
jpeg_quality = 92
workers = 4                      # 并行张数

[portrait]
stages = ["lut"]                 # 路线图：["white_balance", "lut", "skin_protect"]
[portrait.lut]
file = "luts/portrait.cube"
strength = 0.4                   # 人像保守

[other]
stages = ["lut"]
[other.lut]
file = "luts/landscape.cube"
strength = 0.7

[safety]
max_clipped_high = 0.02          # 处理后高光溢出超过 2%：strength_scale 减半，重做一次
```

## 5. 实现顺序（每步独立可验证，按顺序交给 Qwen）

| 步骤 | 文件 | 验收标准 |
|---|---|---|
| M0 | `Cargo.toml`、`error.rs`、`main.rs`、`cli.rs` | `cargo run -- --help` 正常输出 |
| M1 | `imageio.rs` | 读一张 TIFF/JPEG 原样存回，肉眼无差异；单测：读写往返误差 < 1/255 |
| M2 | `grade/lut3d.rs` | 单位 LUT（identity）应用后图像不变；已知 LUT 与手算结果一致 |
| M3 | `grade/blend.rs`、`grade/stats.rs` | strength=0 返回原图，=1 返回结果；纯白图 clipped_high≈1 |
| M4 | `grade/mod.rs`、`grade/lut_stage.rs`、`config.rs` | 用配置创建 LutStage 列表，对测试图执行，结果与直接调用 lut3d + blend 一致 |
| M5 | `develop.rs` | 能把一张 RAW 转成 TIFF；失败时返回带 stderr 的 `Error::Develop` |
| M6 | `scene/mod.rs`、`scene/stub.rs`、`pipeline.rs` | 用 stub 分类器跑通单张完整流程，含高光溢出重试逻辑 |
| M7 | `report.rs`、pipeline 批量化 | rayon 并行处理文件夹，输出对比图与 CSV，单张失败不影响整批 |
| M8 | `scene/face.rs` | 用真实人脸检测替换 stub；15 张人像与 15 张风景测试，准确率达标 |

M0～M4 是纯 Rust、不依赖外部库，最适合练手，也最容易让 Qwen 一次做对，先做这几步。

## 6. 外部依赖（版本用 `cargo add` 锁定，不要让模型猜）

| 用途 | crate | 备注 |
|---|---|---|
| 命令行 | `clap`（derive） | |
| 配置 | `serde`、`toml` | |
| 错误 | `thiserror` | |
| 图像读写 | `image` | 注意 16-bit TIFF 的读取类型 |
| 并行 | `rayon` | |
| 日志 | `tracing`、`tracing-subscriber` | |
| 人脸检测（M8） | `ort`，或 `opencv` | 版本差异大，必须先贴官方示例给模型 |

外部程序：`darktable-cli`。调用前先 `darktable-cli --help` 确认参数；并行运行多个实例时可能互相冲突（共用库文件），需要查文档确认如何隔离配置目录，**不要让模型猜参数**。

## 7. 设计约束

- 所有图像处理函数是**纯函数**，不读文件、不打印，方便单测。
- 对原始 RAW 文件只读，输出一律写到 `output_dir`。
- 单张失败不影响整批：记录错误、继续处理下一张，最后汇总。
- LUT 在启动时读取一次，用 `Arc` 在线程间共享。
- 大图处理时按行并行（`par_chunks_mut`），避免复制整张图。
- **每个调整都必须是一个 Stage**，主流程只负责按顺序调用 Stage，不写具体调色逻辑。
- 高光溢出检查：所有 Stage 执行完后 `clipped_high` 超过阈值，则 `strength_scale` 减半，从原图重做一次，并在日志里标记。

## 8. 每次任务的提示词模板

```
你是 Rust 工程师。项目 Sepiola 的架构见上文（第 0～4 节）。
当前任务：实现 <文件路径> 中的 <函数/功能>。
要求：
- 严格遵守第 0 节规则与第 3 节接口，不得修改接口。
- 依赖版本：<粘贴 Cargo.toml 的相关行>
- 参考资料：<粘贴官方文档或示例，没有则写"无">
- 需要通过的测试：<粘贴验收标准>
先复述你的实现思路（不超过 5 行），再给出完整文件。
```

出现编译错误时：

```
cargo check 报错如下，请只修改必要的地方并给出完整文件：
<完整报错>
```

## 9. 路线图（后续功能，均以新增 Stage 的方式加入）

| 版本 | 功能 | 实现方式 | 备注 |
|---|---|---|---|
| v0.1 | 核心流程：RAW 转换、场景判断、LUT 调色、批处理、对比图 | 本文档第 5 节 M0～M8 | |
| v0.2 | 自动白平衡 | `grade/white_balance.rs`，先做灰世界或高光白点等传统算法 | 不需要模型，可先做 |
| v0.3 | 肤色保护 | `grade/skin_protect.rs`，利用 `SceneInfo.faces` 限制人脸区域的色偏和饱和度变化 | 这也是 `SceneInfo` 要带人脸框的原因 |
| v0.4 | 自动生成 LUT | 用 Python 工具跑 3D LUT 模型，输出 `.cube`，Sepiola 只负责应用 | 模型部分留在 Python，Rust 不必直接推理 |
| v0.5 | 学习个人风格 | 用你自己修过的「原图/成品」配对，拟合出个人 LUT | |
| v0.6 | 桌面界面 | Tauri 或 egui，调用同一套核心库 | 届时把核心拆成 `sepiola-core` 库 + 命令行 + 界面三个 crate |

做 v0.2 以后，`config.example.toml` 里每个场景的 `stages` 列表按顺序加名字即可，主流程不用改。

## 10. 常见坑（提前告诉模型）

- LUT 通常作用在 **sRGB 编码** 的值上，不要在线性空间应用，否则颜色会偏。
- `.cube` 文件：注意 `LUT_3D_SIZE`、`DOMAIN_MIN/MAX`，数据顺序是 **R 变化最快**。
- 16-bit 与 f32 转换：除以 65535，而不是 65536 或 256。
- 混合时不要把已经裁切的值再放大；输出前统一 `clamp(0.0, 1.0)`。
- 人像判断用「最大人脸占整图面积比例」，而不是"有没有脸"。
- Stage 里需要保留原图时，在 Stage 内部 `clone()` 一份，不要修改 Context 里共享的数据。
