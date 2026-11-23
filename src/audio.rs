//! Audio tag extraction for the media library.

use std::path::Path;

use lofty::file::TaggedFileExt;
use lofty::tag::{ItemKey, Tag};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioMeta {
    pub title: Option<String>,
    pub work: Option<String>,
    pub composer: Option<String>,
    pub artist: Option<String>,
    pub conductor: Option<String>,
    pub performers: Option<String>,
    pub ensemble: Option<String>,
    pub movement: Option<String>,
    pub movement_number: Option<String>,
    pub album: Option<String>,
    pub recording_date: Option<String>,
    pub track_number: Option<String>,
    pub disc_number: Option<String>,
    pub label: Option<String>,
}

impl AudioMeta {
    pub fn extract(path: &Path) -> Option<Self> {
        let tagged = lofty::read_from_path(path).ok()?;
        Self::from_tags(tagged.tags())
    }

    fn from_tags(tags: &[Tag]) -> Option<Self> {
        let value = |key| {
            tags.iter()
                .find_map(|tag| tag.get_string(&key).map(str::trim))
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        let metadata = Self {
            title: value(ItemKey::TrackTitle),
            work: value(ItemKey::Work),
            composer: value(ItemKey::Composer),
            artist: value(ItemKey::TrackArtist),
            conductor: value(ItemKey::Conductor),
            performers: value(ItemKey::Performer).or_else(|| value(ItemKey::TrackArtists)),
            ensemble: value(ItemKey::AlbumArtist),
            movement: value(ItemKey::Movement),
            movement_number: value(ItemKey::MovementNumber),
            album: value(ItemKey::AlbumTitle),
            recording_date: value(ItemKey::RecordingDate).or_else(|| value(ItemKey::Year)),
            track_number: value(ItemKey::TrackNumber),
            disc_number: value(ItemKey::DiscNumber),
            label: value(ItemKey::Label),
        };
        let has_metadata = metadata.title.is_some()
            || metadata.work.is_some()
            || metadata.composer.is_some()
            || metadata.artist.is_some()
            || metadata.conductor.is_some()
            || metadata.performers.is_some()
            || metadata.ensemble.is_some()
            || metadata.movement.is_some()
            || metadata.movement_number.is_some()
            || metadata.album.is_some()
            || metadata.recording_date.is_some()
            || metadata.track_number.is_some()
            || metadata.disc_number.is_some()
            || metadata.label.is_some();
        has_metadata.then_some(metadata)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lofty::tag::TagType;

    #[test]
    fn normalizes_classical_and_recording_tags() {
        let mut tag = Tag::new(TagType::VorbisComments);
        tag.insert_text(ItemKey::Composer, "  Ludwig van Beethoven  ".to_owned());
        tag.insert_text(ItemKey::Work, "Symphony No. 5".to_owned());
        tag.insert_text(ItemKey::Conductor, "Herbert von Karajan".to_owned());
        tag.insert_text(ItemKey::Performer, "Berlin Philharmonic".to_owned());
        tag.insert_text(ItemKey::MovementNumber, "1".to_owned());
        tag.insert_text(ItemKey::RecordingDate, "1963".to_owned());

        let metadata = AudioMeta::from_tags(&[tag]).expect("metadata");
        assert_eq!(metadata.composer.as_deref(), Some("Ludwig van Beethoven"));
        assert_eq!(metadata.work.as_deref(), Some("Symphony No. 5"));
        assert_eq!(metadata.conductor.as_deref(), Some("Herbert von Karajan"));
        assert_eq!(metadata.performers.as_deref(), Some("Berlin Philharmonic"));
        assert_eq!(metadata.movement_number.as_deref(), Some("1"));
        assert_eq!(metadata.recording_date.as_deref(), Some("1963"));
    }

    #[test]
    fn empty_tags_produce_no_metadata() {
        assert!(AudioMeta::from_tags(&[]).is_none());
        assert!(AudioMeta::from_tags(&[Tag::new(TagType::VorbisComments)]).is_none());
    }
}