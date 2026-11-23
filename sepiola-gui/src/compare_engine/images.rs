use std::path::Path;

pub(super) fn difference_hash(path: &Path) -> Option<u64> {
    let image = sepiola::imageio::load_dynamic(path).ok()?.to_luma8();
    let thumbnail = image::imageops::resize(&image, 9, 8, image::imageops::FilterType::Triangle);
    let mut hash = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            if thumbnail.get_pixel(x, y)[0] > thumbnail.get_pixel(x + 1, y)[0] {
                hash |= 1u64 << (y * 8 + x);
            }
        }
    }
    Some(hash)
}

pub(super) fn similarity(left: u64, right: u64) -> f32 {
    similarity_from_distance((left ^ right).count_ones())
}

pub(super) fn similarity_from_distance(distance: u32) -> f32 {
    1.0 - distance as f32 / 64.0
}

pub(super) fn hash_buckets(hash: u64) -> Vec<(u8, u8)> {
    (0..11)
        .map(|segment| {
            let shift = segment * 6;
            let mask = if segment == 10 { 0x0f } else { 0x3f };
            (segment as u8, ((hash >> shift) & mask) as u8)
        })
        .collect()
}
