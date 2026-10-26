//! 调试工具：打印 2d106det 在测试人像上的 106 关键点，用于校准 5 点映射。
use sepiola::identity::{crop_face, resize_exact};
use sepiola::scene::face::FaceClassifier;
use sepiola::scene::landmark::LandmarkModel;
use sepiola::scene::SceneClassifier;
use sepiola::{config::SceneSettings, imageio};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("usage: dump_landmarks <image>");
    let img = imageio::load(std::path::Path::new(&path))?;
    let settings = SceneSettings::default();
    let classifier = FaceClassifier::new(&settings)?;
    let info = classifier.classify(&img)?;
    let face = info
        .faces
        .iter()
        .max_by(|a, b| a.area().partial_cmp(&b.area()).unwrap())
        .expect("no face");
    let crop = crop_face(&img, face);
    let crop = resize_exact(&crop, 192);
    let model = LandmarkModel::load("models/2d106det.onnx")?;
    let points = model.landmarks(&crop)?;
    let xs: Vec<f32> = points.iter().map(|p| p[0]).collect();
    let ys: Vec<f32> = points.iter().map(|p| p[1]).collect();
    eprintln!(
        "x range {:.1}..{:.1}  y range {:.1}..{:.1}",
        xs.iter().cloned().fold(1e9,f32::min), xs.iter().cloned().fold(-1e9,f32::max),
        ys.iter().cloned().fold(1e9,f32::min), ys.iter().cloned().fold(-1e9,f32::max)
    );
    for (i, p) in points.iter().enumerate() {
        println!("{i:3}: {:.1} {:.1}", p[0], p[1]);
    }
    Ok(())
}
