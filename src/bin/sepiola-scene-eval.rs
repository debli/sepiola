//! Evaluate scene-to-album classification on a labelled image folder.
//!
//! Dataset layout: `<root>/<expected album>/<image>`.
//! ```bash
//! cargo run --bin sepiola-scene-eval -- test_data/scene-labelled
//! ```

use std::path::{Path, PathBuf};

use clap::Parser;
use sepiola::config::Config;
use sepiola::library::ALBUM_UNCLASSIFIED;
use sepiola::scene::mobileclip::MobileClipTagger;
use sepiola::scene::places::{album_for_places_class, PlacesTagger};
use sepiola::scene::tags::{album_for_class, SceneTagger};

#[derive(Parser)]
#[command(
    name = "sepiola-scene-eval",
    about = "Evaluate scene-to-album classification on labelled images"
)]
struct Cli {
    /// Root folder with one expected album per immediate subfolder
    root: PathBuf,

    /// Optional config containing scene model paths and confidence thresholds
    #[arg(short, long)]
    config: Option<PathBuf>,
}

#[derive(Default)]
struct Score {
    total: usize,
    correct: usize,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("sepiola-scene-eval: {error}");
        std::process::exit(1);
    }
}

fn run() -> sepiola::error::Result<()> {
    let cli = Cli::parse();
    let config = match &cli.config {
        Some(path) => Config::load_file(path)?,
        None => Config::default(),
    };
    let mobileclip = MobileClipTagger::new(&config.scene_tags)?;
    let tagger = SceneTagger::new(&config.scene_tags)?;
    let places = PlacesTagger::new(&config.scene_tags)?;
    let mut samples = Vec::new();

    for entry in std::fs::read_dir(&cli.root)? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_dir() || is_hidden(&path) {
            continue;
        }
        let expected = entry.file_name().to_string_lossy().into_owned();
        collect_images(&path, &expected, &mut samples)?;
    }
    if samples.is_empty() {
        return Err(sepiola::error::Error::Config(format!(
            "no labelled images found under {}",
            cli.root.display()
        )));
    }

    let mut imagenet_score = Score::default();
    let mut places_score = Score::default();
    let mut cascade_score = Score::default();
    let mut mobileclip_score = Score::default();
    let mut skipped = 0usize;

    for (path, expected) in &samples {
        let image = match sepiola::imageio::load(path) {
            Ok(image) => image,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let image_tag = tagger.tag(&image)?;
        let places_tag = places.tag(&image)?;
        let mobileclip_tag = mobileclip.tag(&image)?;
        let imagenet_album = (image_tag.confidence >= config.scene_tags.min_confidence)
            .then(|| album_for_class(image_tag.index, &image_tag.class))
            .flatten();
        let places_album = (places_tag.confidence >= config.scene_tags.places_min_confidence)
            .then(|| album_for_places_class(&places_tag.class))
            .flatten();
        let cascade_album = imagenet_album
            .or(places_album)
            .unwrap_or(ALBUM_UNCLASSIFIED);
        let imagenet_album = imagenet_album.unwrap_or(ALBUM_UNCLASSIFIED);
        let places_album = places_album.unwrap_or(ALBUM_UNCLASSIFIED);
        let mobileclip_album = mobileclip_tag
            .as_ref()
            .map_or(ALBUM_UNCLASSIFIED, |tag| tag.album);

        record(&mut imagenet_score, imagenet_album, expected);
        record(&mut places_score, places_album, expected);
        record(&mut cascade_score, cascade_album, expected);
        record(&mut mobileclip_score, mobileclip_album, expected);
        println!(
            "{}\texpected={}\tMobileCLIP={} ({:.3}; margin={:.3})\tImageNet={} ({:.3}; {})\tPlaces365={} ({:.3}; {})\tlegacy={}",
            path.display(),
            expected,
            mobileclip_album,
            mobileclip_tag.as_ref().map_or(0.0, |tag| tag.similarity),
            mobileclip_tag.as_ref().map_or(0.0, |tag| tag.margin),
            imagenet_album,
            image_tag.confidence,
            image_tag.class,
            places_album,
            places_tag.confidence,
            places_tag.class,
            cascade_album,
        );
    }

    println!(
        "\nLabelled images: {}; skipped unreadable: {skipped}",
        samples.len()
    );
    print_score("ImageNet", &imagenet_score);
    print_score("Places365", &places_score);
    print_score("Legacy cascade", &cascade_score);
    print_score("MobileCLIP", &mobileclip_score);
    Ok(())
}

fn collect_images(
    dir: &Path,
    expected: &str,
    output: &mut Vec<(PathBuf, String)>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if is_hidden(&path) {
            continue;
        }
        if entry.file_type()?.is_dir() {
            collect_images(&path, expected, output)?;
        } else if entry.file_type()?.is_file() {
            output.push((path, expected.to_string()));
        }
    }
    Ok(())
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with('.'))
}

fn record(score: &mut Score, predicted: &str, expected: &str) {
    score.total += 1;
    score.correct += usize::from(predicted == expected);
}

fn print_score(name: &str, score: &Score) {
    let accuracy = if score.total == 0 {
        0.0
    } else {
        score.correct as f32 / score.total as f32
    };
    println!(
        "{name}: {}/{} correct ({:.1}%)",
        score.correct,
        score.total,
        accuracy * 100.0
    );
}
