//! Places365 场景分类（ResNet-18 ONNX），用于媒体库相册细分（M34）。
//!
//! 模型：ailia-models `resnet18_places365.onnx`（源自 CSAILVision Places365-CNNs）。
//! 预处理与官方推理脚本一致（实测首节点为 Conv，未烘焙归一化）：
//! 直接缩放到 256×256（忽略宽高比）→ center crop 224 → RGB /255 →
//! ImageNet mean/std → NCHW，输入名 `input_img`，输出 365 维 logits。

use std::sync::Mutex;

use ort::session::Session;
use ort::value::Tensor;

use crate::config::SceneTagSettings;
use crate::error::{Error, Result};
use crate::imageio::{ImageF32, CHANNELS};
use crate::library::{
    ALBUM_ADVENTURE, ALBUM_ARCHITECTURE, ALBUM_EXPLORE, ALBUM_FOOD, ALBUM_PARTY, ALBUM_SCENERY,
    ALBUM_SPORTS, ALBUM_VEHICLE,
};
use crate::scene::face::ort_environment;
use crate::scene::tags::{softmax_top1, SceneTag};

/// 官方评测缩放尺寸与裁剪尺寸。
const SIDE: usize = 256;
const CROP_SIDE: usize = 224;
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

/// Places365 场景标签器。
pub struct PlacesTagger {
    session: Mutex<Session>,
    classes: Vec<String>,
}

impl PlacesTagger {
    /// 创建标签器并加载 ONNX 模型与类表；模型缺失返回 Err（调用方静默降级）。
    pub fn new(settings: &SceneTagSettings) -> Result<Self> {
        let model = crate::paths::resolve_asset(&settings.places_model);
        if !model.is_file() {
            return Err(Error::Model(format!(
                "places365 model not found: {}",
                model.display()
            )));
        }
        let classes_path = crate::paths::resolve_asset(&settings.places_classes);
        let classes = load_classes(&classes_path)?;
        let environment = ort_environment()?;
        tracing::info!(model = %model.display(), classes = classes.len(), "loading places365 model");
        let session = Session::builder(environment)
            .and_then(|mut builder| builder.commit_from_file(&model))
            .map_err(|e| Error::Model(format!("failed to load {}: {e}", model.display())))?;
        Ok(Self {
            session: Mutex::new(session),
            classes,
        })
    }

    /// 推理 top-1 场景标签（复用 SceneTag：index 为 365 类下标）。
    pub fn tag(&self, img: &ImageF32) -> Result<SceneTag> {
        let input = preprocess(img);
        let tensor = Tensor::from_array((
            [1usize, CHANNELS, CROP_SIDE, CROP_SIDE],
            input.into_boxed_slice(),
        ))
        .map_err(|e| Error::Model(format!("failed to build places input tensor: {e}")))?;

        let mut session = self
            .session
            .lock()
            .map_err(|_| Error::Model("places tagger mutex poisoned".to_string()))?;
        let outputs = session
            .run(ort::inputs![tensor])
            .map_err(|e| Error::Model(format!("places inference failed: {e}")))?;
        let value = &outputs[0];
        let (_, logits) = value
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Model(format!("places output: {e}")))?;
        let (index, confidence) = softmax_top1(&logits);
        let class = self
            .classes
            .get(index)
            .cloned()
            .unwrap_or_else(|| format!("class_{index}"));
        Ok(SceneTag {
            index,
            class,
            confidence,
        })
    }
}

/// 读取类表：每行 `/x/name 0`，去掉 3 字符前缀 `/x/` 后为类名（保留子路径）。
fn load_classes(path: &std::path::Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::Model(format!("failed to read {}: {e}", path.display())))?;
    let classes: Vec<String> = text
        .lines()
        .filter_map(parse_places_class)
        .collect();
    if classes.len() < 2 {
        return Err(Error::Model(format!(
            "places classes too short: {}",
            path.display()
        )));
    }
    Ok(classes)
}

/// 解析单行类名；空行返回 None。
pub fn parse_places_class(line: &str) -> Option<String> {
    let token = line.split_whitespace().next()?;
    let bytes = token.as_bytes();
    if bytes.len() > 3 && bytes[0] == b'/' && bytes[2] == b'/' {
        Some(token[3..].to_string())
    } else {
        Some(token.to_string())
    }
}

/// Places365 类 → 细分相册名；None 表示不细分（由调用方兜底"未分类"）。
///
/// 风景=温和自然场景；探险=极端/荒野地形；冒险=游乐与户外活动；
/// 建筑=人造建筑与城市街景；日出/天象无对应类，留待专项分类器。
pub fn album_for_places_class(class: &str) -> Option<&'static str> {
    let album = match class {
        // 风景：温和自然与田园
        "beach"
        | "bamboo_forest"
        | "coast"
        | "creek"
        | "field/cultivated"
        | "field/wild"
        | "forest/broadleaf"
        | "forest_path"
        | "forest_road"
        | "rainforest"
        | "hot_spring"
        | "islet"
        | "lagoon"
        | "lake/natural"
        | "mountain"
        | "mountain_path"
        | "mountain_snowy"
        | "ocean"
        | "orchard"
        | "pasture"
        | "pond"
        | "river"
        | "snowfield"
        | "valley"
        | "vineyard"
        | "waterfall"
        | "wave"
        | "botanical_garden"
        | "formal_garden"
        | "japanese_garden"
        | "zen_garden"
        | "topiary_garden"
        | "vegetable_garden"
        | "corn_field"
        | "wheat_field"
        | "rice_paddy"
        | "hayfield"
        | "tree_farm"
        | "park"
        | "promenade"
        | "boardwalk"
        | "swimming_hole"
        | "harbor"
        | "pier" => ALBUM_SCENERY,
        // 探险：极端/荒野/遗迹地形
        "badlands"
        | "butte"
        | "canyon"
        | "crevasse"
        | "desert/sand"
        | "desert/vegetation"
        | "desert_road"
        | "archaelogical_excavation"
        | "excavation"
        | "grotto"
        | "iceberg"
        | "ice_floe"
        | "ice_shelf"
        | "igloo"
        | "oilrig"
        | "underwater/ocean_deep"
        | "volcano"
        | "catacomb"
        | "burial_chamber"
        | "trench"
        | "rock_arch"
        | "glacier"
        | "swamp"
        | "marsh"
        | "tundra" => ALBUM_EXPLORE,
        // 冒险：游乐与户外活动
        "amusement_park"
        | "amusement_arcade"
        | "water_park"
        | "campsite"
        | "raft"
        | "rope_bridge"
        | "ski_resort"
        | "raceway"
        | "playground"
        | "tree_house"
        | "hunting_lodge/outdoor"
        | "carrousel"
        | "ball_pit" => ALBUM_ADVENTURE,
        // 运动：场馆与球场
        "athletic_field/outdoor"
        | "baseball_field"
        | "basketball_court/indoor"
        | "boxing_ring"
        | "football_field"
        | "golf_course"
        | "gymnasium/indoor"
        | "arena/hockey"
        | "martial_arts_gym"
        | "racecourse"
        | "ski_slope"
        | "soccer_field"
        | "stadium/baseball"
        | "stadium/football"
        | "stadium/soccer"
        | "swimming_pool/indoor"
        | "swimming_pool/outdoor"
        | "volleyball_court/outdoor"
        | "ice_skating_rink/indoor"
        | "ice_skating_rink/outdoor"
        | "bowling_alley" => ALBUM_SPORTS,
        // 建筑/城市
        "apartment_building/outdoor"
        | "arch"
        | "aqueduct"
        | "balcony/exterior"
        | "bridge"
        | "building_facade"
        | "castle"
        | "chalet"
        | "church/indoor"
        | "church/outdoor"
        | "courthouse"
        | "cottage"
        | "dam"
        | "downtown"
        | "embassy"
        | "house"
        | "mansion"
        | "medina"
        | "mosque/outdoor"
        | "palace"
        | "pagoda"
        | "lighthouse"
        | "ruin"
        | "skyscraper"
        | "street"
        | "synagogue/outdoor"
        | "temple/asia"
        | "tower"
        | "viaduct"
        | "village"
        | "residential_neighborhood"
        | "alley"
        | "crosswalk"
        | "plaza"
        | "amphitheater"
        | "barn"
        | "boathouse"
        | "fire_station"
        | "courtyard"
        | "roof_garden"
        | "hotel/outdoor"
        | "inn/outdoor"
        | "motel"
        | "windmill"
        | "water_tower"
        | "construction_site"
        | "beach_house"
        | "slum"
        | "kasbah"
        | "oast_house"
        | "manufactured_home"
        | "schoolhouse"
        | "moat/water"
        | "arcade"
        | "bus_station/indoor"
        | "train_station/platform"
        | "subway_station/platform"
        | "parking_garage/indoor"
        | "parking_garage/outdoor" => ALBUM_ARCHITECTURE,
        // 交通工具
        "airfield"
        | "airplane_cabin"
        | "boat_deck"
        | "bus_interior"
        | "car_interior"
        | "cockpit"
        | "landing_deck"
        | "railroad_track"
        | "train_interior"
        | "runway"
        | "heliport"
        | "berth"
        | "gas_station" => ALBUM_VEHICLE,
        // 美食：食物特写与厨房餐档
        "bakery/shop"
        | "butchers_shop"
        | "delicatessen"
        | "dining_room"
        | "fastfood_restaurant"
        | "food_court"
        | "ice_cream_parlor"
        | "kitchen"
        | "pantry"
        | "pizzeria"
        | "restaurant_kitchen"
        | "sushi_bar"
        | "candy_store"
        | "galley" => ALBUM_FOOD,
        // 聚会：酒吧餐厅聚会场所
        "bar"
        | "beer_garden"
        | "beer_hall"
        | "banquet_hall"
        | "cafeteria"
        | "discotheque"
        | "pub/indoor"
        | "restaurant"
        | "dining_hall"
        | "wet_bar" => ALBUM_PARTY,
        _ => return None,
    };
    Some(album)
}

/// 缩放到 256×256（双线性，忽略宽高比）→ 224 中心裁剪 → 归一化 NCHW。
fn preprocess(img: &ImageF32) -> Vec<f32> {
    let resized = resize_square(img, SIDE);
    let cropped = center_crop(&resized, CROP_SIDE);
    let plane = CROP_SIDE * CROP_SIDE;
    let mut data = vec![0.0f32; plane * CHANNELS];
    for y in 0..CROP_SIDE {
        for x in 0..CROP_SIDE {
            let src = (y * CROP_SIDE + x) * CHANNELS;
            let dst = y * CROP_SIDE + x;
            for c in 0..CHANNELS {
                let value = cropped.data[src + c].clamp(0.0, 1.0);
                data[c * plane + dst] = (value - MEAN[c]) / STD[c];
            }
        }
    }
    data
}

/// 双线性拉伸到 `side`×`side`（与官方 cv2.resize 行为一致，不保宽高比）。
fn resize_square(img: &ImageF32, side: usize) -> ImageF32 {
    if img.width as usize == side && img.height as usize == side {
        return img.clone();
    }
    let mut out = vec![0.0f32; side * side * CHANNELS];
    let x_ratio = img.width as f32 / side as f32;
    let y_ratio = img.height as f32 / side as f32;
    for y in 0..side {
        let src_y = (y as f32 + 0.5) * y_ratio - 0.5;
        let y0 = (src_y.floor().max(0.0)) as usize;
        let y0 = y0.min(img.height as usize - 1);
        let y1 = (y0 + 1).min(img.height as usize - 1);
        let wy = (src_y - y0 as f32).clamp(0.0, 1.0);
        for x in 0..side {
            let src_x = (x as f32 + 0.5) * x_ratio - 0.5;
            let x0 = (src_x.floor().max(0.0)) as usize;
            let x0 = x0.min(img.width as usize - 1);
            let x1 = (x0 + 1).min(img.width as usize - 1);
            let wx = (src_x - x0 as f32).clamp(0.0, 1.0);
            let idx = |px: usize, py: usize| (py * img.width as usize + px) * CHANNELS;
            let dst = (y * side + x) * CHANNELS;
            for c in 0..CHANNELS {
                let top = img.data[idx(x0, y0) + c] * (1.0 - wx) + img.data[idx(x1, y0) + c] * wx;
                let bottom =
                    img.data[idx(x0, y1) + c] * (1.0 - wx) + img.data[idx(x1, y1) + c] * wx;
                out[dst + c] = top * (1.0 - wy) + bottom * wy;
            }
        }
    }
    ImageF32 {
        width: side as u32,
        height: side as u32,
        data: out,
    }
}

/// 居中裁剪 `side`×`side`（输入已为 SIDE×SIDE，即裁掉 16px 边）。
fn center_crop(img: &ImageF32, side: usize) -> ImageF32 {
    let w = img.width as usize;
    let h = img.height as usize;
    let offset_x = w.saturating_sub(side) / 2;
    let offset_y = h.saturating_sub(side) / 2;
    let mut out = vec![0.0f32; side * side * CHANNELS];
    for y in 0..side {
        for x in 0..side {
            let src_x = (x + offset_x).min(w - 1);
            let src_y = (y + offset_y).min(h - 1);
            let src = (src_y * w + src_x) * CHANNELS;
            let dst = (y * side + x) * CHANNELS;
            out[dst..dst + CHANNELS].copy_from_slice(&img.data[src..src + CHANNELS]);
        }
    }
    ImageF32 {
        width: side as u32,
        height: side as u32,
        data: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_places_class_strips_letter_prefix() {
        assert_eq!(parse_places_class("/a/airfield 0").as_deref(), Some("airfield"));
        assert_eq!(
            parse_places_class("/c/canal/natural 12").as_deref(),
            Some("canal/natural")
        );
        assert_eq!(parse_places_class(""), None);
    }

    #[test]
    fn places_album_mapping() {
        assert_eq!(album_for_places_class("beach"), Some(ALBUM_SCENERY));
        assert_eq!(album_for_places_class("lake/natural"), Some(ALBUM_SCENERY));
        assert_eq!(album_for_places_class("park"), Some(ALBUM_SCENERY));
        assert_eq!(album_for_places_class("canyon"), Some(ALBUM_EXPLORE));
        assert_eq!(album_for_places_class("amusement_park"), Some(ALBUM_ADVENTURE));
        assert_eq!(
            album_for_places_class("basketball_court/indoor"),
            Some(ALBUM_SPORTS)
        );
        assert_eq!(album_for_places_class("castle"), Some(ALBUM_ARCHITECTURE));
        assert_eq!(album_for_places_class("airplane_cabin"), Some(ALBUM_VEHICLE));
        assert_eq!(album_for_places_class("pizzeria"), Some(ALBUM_FOOD));
        assert_eq!(album_for_places_class("pub/indoor"), Some(ALBUM_PARTY));
        // 室内/办公等未映射类不再兜底风景。
        assert_eq!(album_for_places_class("office"), None);
        assert_eq!(album_for_places_class("bedroom"), None);
        assert_eq!(album_for_places_class("classroom"), None);
    }

    #[test]
    fn preprocess_normalizes_constant_gray() {
        let img = ImageF32 {
            width: 320,
            height: 200,
            data: vec![0.5f32; 320 * 200 * CHANNELS],
        };
        let input = preprocess(&img);
        assert_eq!(input.len(), CHANNELS * CROP_SIDE * CROP_SIDE);
        let plane = CROP_SIDE * CROP_SIDE;
        for c in 0..CHANNELS {
            let expected = (0.5 - MEAN[c]) / STD[c];
            assert!(
                (input[c * plane] - expected).abs() < 1e-5,
                "channel {c}: {} vs {expected}",
                input[c * plane]
            );
        }
    }

    #[test]
    fn resize_square_shapes() {
        let img = ImageF32 {
            width: 640,
            height: 480,
            data: vec![0.25f32; 640 * 480 * CHANNELS],
        };
        let resized = resize_square(&img, SIDE);
        assert_eq!((resized.width, resized.height), (SIDE as u32, SIDE as u32));
        let crop = center_crop(&resized, CROP_SIDE);
        assert_eq!((crop.width, crop.height), (CROP_SIDE as u32, CROP_SIDE as u32));
    }

    /// 真实模型回归：风景样本应命中风景相册，办公/截图类场景不得判为风景
    /// （模型缺失时跳过）。
    #[test]
    fn real_model_places_samples() {
        let settings = SceneTagSettings::default();
        let Ok(tagger) = PlacesTagger::new(&settings) else {
            return; // 模型未就位时跳过
        };
        let landscape = std::path::Path::new("test_data/landscape/pic_10.jpg");
        let Ok(img) = crate::imageio::load(landscape) else {
            return;
        };
        let tag = tagger.tag(&img).expect("places tag");
        let album = album_for_places_class(&tag.class);
        println!("landscape → top1 {} {:.3} → {album:?}", tag.class, tag.confidence);
        assert_eq!(album, Some(ALBUM_SCENERY), "landscape sample: {}", tag.class);

        // 合成"幻灯片截图"：白底 + 深色标题条与文本块，不应归入风景。
        let slide = synthetic_slide();
        let tag = tagger.tag(&slide).expect("places tag slide");
        let album = album_for_places_class(&tag.class);
        println!("slide → top1 {} {:.3} → {album:?}", tag.class, tag.confidence);
        assert_ne!(album, Some(ALBUM_SCENERY), "slide must not be scenery: {}", tag.class);
    }

    /// 生成 1280×720 白底 PPT 风格图（标题条 + 若干文本行块）。
    fn synthetic_slide() -> ImageF32 {
        let (w, h) = (1280usize, 720usize);
        let mut data = vec![0.96f32; w * h * CHANNELS];
        let mut rect = |x0: usize, y0: usize, x1: usize, y1: usize, value: f32| {
            for y in y0..y1.min(h) {
                for x in x0..x1.min(w) {
                    let i = (y * w + x) * CHANNELS;
                    for c in 0..CHANNELS {
                        data[i + c] = value;
                    }
                }
            }
        };
        rect(0, 0, w, 120, 0.15); // 顶部标题条
        for row in 0..6 {
            let y = 180 + row * 70;
            rect(120, y, 150, y + 24, 0.2); // 项目符号
            rect(180, y, 1000 - row * 90, y + 24, 0.25); // 文本行
        }
        ImageF32 {
            width: w as u32,
            height: h as u32,
            data,
        }
    }
}
