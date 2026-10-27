//! MobileCLIP2-S0 zero-shot scene-to-album classification using image/text embeddings.

use std::sync::Mutex;

use ort::session::Session;
use ort::value::Tensor;
use tokenizers::Tokenizer;

use crate::config::SceneTagSettings;
use crate::error::{Error, Result};
use crate::imageio::{ImageF32, CHANNELS};
use crate::library::{
    ALBUM_ADVENTURE, ALBUM_ARCHITECTURE, ALBUM_BIRTHDAY, ALBUM_EXPLORE, ALBUM_FOOD, ALBUM_PARTY,
    ALBUM_PLANTS_ANIMALS, ALBUM_SCENERY, ALBUM_SCREEN_DOC, ALBUM_SPORTS, ALBUM_VEHICLE,
};
use crate::paths::resolve_asset;
use crate::scene::face::ort_environment;
use crate::scene::tags::{center_crop, resize_short_side};

const SIDE: usize = 256;
const CONTEXT: usize = 77;
const EMBEDDING_SIZE: usize = 512;
const MEAN: [f32; 3] = [0.48145466, 0.4578275, 0.40821073];
const STD: [f32; 3] = [0.26862954, 0.26130258, 0.27577711];

const ALBUM_PROMPTS: &[(&str, &str)] = &[
    (
        ALBUM_SCENERY,
        "a photograph of a natural landscape with mountains, forests, lakes, rivers, beaches, ocean or sunset",
    ),
    (
        ALBUM_PLANTS_ANIMALS,
        "a photograph of animals, pets, wildlife, flowers, plants or trees",
    ),
    (
        ALBUM_BIRTHDAY,
        "a photograph of a birthday celebration with a birthday cake, candles, balloons or party decorations",
    ),
    (
        ALBUM_PARTY,
        "a photograph of a social gathering, party, banquet, bar or people dining together",
    ),
    (
        ALBUM_ARCHITECTURE,
        "a photograph of a building, architecture, city street, bridge or historic monument",
    ),
    (
        ALBUM_VEHICLE,
        "a photograph whose main subject is a car, bus, train, ship, airplane or other vehicle",
    ),
    (
        ALBUM_FOOD,
        "a close-up photograph of prepared food, a meal, fruit, dessert or a drink",
    ),
    (
        ALBUM_SPORTS,
        "a photograph of a sport, athletic competition, exercise or sports equipment",
    ),
    (
        ALBUM_ADVENTURE,
        "a photograph of an amusement park, outdoor recreation, camping or an adventurous activity",
    ),
    (
        ALBUM_EXPLORE,
        "a photograph of a dramatic natural wonder such as a canyon, cave, waterfall or wilderness",
    ),
    (
        ALBUM_SCREEN_DOC,
        "a photograph of a computer screen, document, presentation slide, chart or printed page",
    ),
];

#[derive(Clone, Debug, PartialEq)]
pub struct MobileClipMatch {
    pub album: &'static str,
    pub similarity: f32,
    pub margin: f32,
}

pub struct MobileClipTagger {
    vision: Mutex<Session>,
    album_embeddings: Vec<(&'static str, Vec<f32>)>,
    min_similarity: f32,
    min_margin: f32,
}

impl MobileClipTagger {
    pub fn new(settings: &SceneTagSettings) -> Result<Self> {
        let vision_path = resolve_asset(&settings.mobileclip_vision_model);
        let text_path = resolve_asset(&settings.mobileclip_text_model);
        let tokenizer_path = resolve_asset(&settings.mobileclip_tokenizer);
        for path in [&vision_path, &text_path, &tokenizer_path] {
            if !path.is_file() {
                return Err(Error::Model(format!(
                    "MobileCLIP asset not found: {}",
                    path.display()
                )));
            }
        }

        let tokenizer = Tokenizer::from_file(&tokenizer_path).map_err(|error| {
            Error::Model(format!("failed to load MobileCLIP tokenizer: {error}"))
        })?;
        let environment = ort_environment()?;
        let vision = Session::builder(environment)
            .and_then(|mut builder| builder.commit_from_file(&vision_path))
            .map_err(|error| {
                Error::Model(format!("failed to load {}: {error}", vision_path.display()))
            })?;
        let mut text = Session::builder(environment)
            .and_then(|mut builder| builder.commit_from_file(&text_path))
            .map_err(|error| {
                Error::Model(format!("failed to load {}: {error}", text_path.display()))
            })?;
        tracing::info!(
            vision = %vision_path.display(),
            text = %text_path.display(),
            "loading MobileCLIP2-S0"
        );

        let mut album_embeddings = Vec::with_capacity(ALBUM_PROMPTS.len());
        for &(album, prompt) in ALBUM_PROMPTS {
            album_embeddings.push((album, encode_text(&mut text, &tokenizer, prompt)?));
        }

        Ok(Self {
            vision: Mutex::new(vision),
            album_embeddings,
            min_similarity: settings.mobileclip_min_similarity,
            min_margin: settings.mobileclip_min_margin,
        })
    }

    pub fn tag(&self, image: &ImageF32) -> Result<Option<MobileClipMatch>> {
        let input = preprocess(image);
        let tensor = Tensor::from_array(([1usize, CHANNELS, SIDE, SIDE], input.into_boxed_slice()))
            .map_err(|error| {
                Error::Model(format!("failed to build MobileCLIP image tensor: {error}"))
            })?;
        let mut session = self
            .vision
            .lock()
            .map_err(|_| Error::Model("MobileCLIP vision mutex poisoned".to_string()))?;
        let outputs = session
            .run(ort::inputs!["pixel_values" => tensor])
            .map_err(|error| Error::Model(format!("MobileCLIP image inference failed: {error}")))?;
        let (_, embedding) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|error| Error::Model(format!("MobileCLIP image output: {error}")))?;
        let image_embedding = normalize(embedding)?;

        Ok(best_match(
            &image_embedding,
            &self.album_embeddings,
            self.min_similarity,
            self.min_margin,
        ))
    }
}

fn encode_text(session: &mut Session, tokenizer: &Tokenizer, prompt: &str) -> Result<Vec<f32>> {
    let encoding = tokenizer
        .encode(prompt, true)
        .map_err(|error| Error::Model(format!("MobileCLIP tokenization failed: {error}")))?;
    let mut ids: Vec<i64> = encoding
        .get_ids()
        .iter()
        .take(CONTEXT)
        .map(|&id| i64::from(id))
        .collect();
    ids.resize(CONTEXT, 0);
    let tensor =
        Tensor::from_array(([1usize, CONTEXT], ids.into_boxed_slice())).map_err(|error| {
            Error::Model(format!("failed to build MobileCLIP text tensor: {error}"))
        })?;
    let outputs = session
        .run(ort::inputs!["input_ids" => tensor])
        .map_err(|error| Error::Model(format!("MobileCLIP text inference failed: {error}")))?;
    let (_, embedding) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|error| Error::Model(format!("MobileCLIP text output: {error}")))?;
    normalize(embedding)
}

fn best_match(
    image: &[f32],
    candidates: &[(&'static str, Vec<f32>)],
    min_similarity: f32,
    min_margin: f32,
) -> Option<MobileClipMatch> {
    let mut scores: Vec<_> = candidates
        .iter()
        .map(|(album, embedding)| (*album, dot(image, embedding)))
        .collect();
    scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let &(album, similarity) = scores.first()?;
    let margin = similarity - scores.get(1).map_or(0.0, |score| score.1);
    if similarity < min_similarity || margin < min_margin {
        return None;
    }
    Some(MobileClipMatch {
        album,
        similarity,
        margin,
    })
}

fn dot(left: &[f32], right: &[f32]) -> f32 {
    left.iter().zip(right).map(|(a, b)| a * b).sum()
}

fn normalize(values: &[f32]) -> Result<Vec<f32>> {
    if values.len() != EMBEDDING_SIZE {
        return Err(Error::Model(format!(
            "unexpected MobileCLIP embedding size: {}",
            values.len()
        )));
    }
    let norm = dot(values, values).sqrt();
    if !norm.is_finite() || norm <= f32::EPSILON {
        return Err(Error::Model(
            "invalid MobileCLIP embedding norm".to_string(),
        ));
    }
    Ok(values.iter().map(|value| value / norm).collect())
}

fn preprocess(image: &ImageF32) -> Vec<f32> {
    let resized = resize_short_side(image, SIDE);
    let cropped = center_crop(&resized, SIDE);
    let plane = SIDE * SIDE;
    let mut data = vec![0.0f32; plane * CHANNELS];
    for y in 0..SIDE {
        for x in 0..SIDE {
            let source = (y * SIDE + x) * CHANNELS;
            let target = y * SIDE + x;
            for channel in 0..CHANNELS {
                let value = cropped.data[source + channel].clamp(0.0, 1.0);
                data[channel * plane + target] = (value - MEAN[channel]) / STD[channel];
            }
        }
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn best_match_rejects_low_similarity_and_close_runner_up() {
        let candidates = vec![
            (ALBUM_SCENERY, vec![0.8, 0.6]),
            (ALBUM_PARTY, vec![0.7, 0.7]),
        ];
        assert!(best_match(&[1.0, 0.0], &candidates, 0.9, 0.01).is_none());
        assert!(best_match(&[1.0, 0.0], &candidates, 0.0, 0.2).is_none());
    }

    #[test]
    fn best_match_returns_confident_album_and_margin() {
        let candidates = vec![
            (ALBUM_SCENERY, vec![0.9, 0.1]),
            (ALBUM_PARTY, vec![0.2, 0.8]),
        ];
        let result = best_match(&[1.0, 0.0], &candidates, 0.5, 0.2).unwrap();
        assert_eq!(result.album, ALBUM_SCENERY);
        assert!((result.similarity - 0.9).abs() < 1e-6);
        assert!((result.margin - 0.7).abs() < 1e-6);
    }
}
