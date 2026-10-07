use image::GrayImage;
use rayon::prelude::*;

/// Per-row luma summary of a grayscale strip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowStat {
    pub y: u32,
    pub min: u8,
    pub max: u8,
    pub mean: f32,
    /// Mean absolute horizontal gradient of the row.
    ///
    /// The sum of `|dx|` and its per-pixel mean rank rows identically because
    /// every row of one image shares the same width, but the mean keeps the
    /// term on the same scale as the luma range so the flatness fallback can
    /// actually weigh both signals instead of being decided by `width` alone.
    pub gradient: f64,
}

/// Summarizes every row of `image`.
///
/// The planner only ever needs per-row aggregates, so materializing a luma
/// plane here would double the memory traffic of an already large strip for no
/// benefit. Rows are read as contiguous slices, never through per-pixel
/// indexing. The caller owns the conversion to luma and the trade-off behind it,
/// because only the caller knows what the source format was.
///
/// Rows are also independent: the gradient term restarts at each row, so a row
/// never looks at its neighbour. That is what lets a strip one image tall
/// measure across every core instead of one — see [`row_stat`].
#[must_use]
pub fn row_profile(image: &GrayImage) -> Vec<RowStat> {
    let width = image.width() as usize;
    if width == 0 {
        return Vec::new();
    }
    image
        .as_raw()
        .par_chunks(width)
        .enumerate()
        .map(|(y, row)| row_stat(row, width, y as u32))
        .collect()
}

/// One row's aggregates, computed without reference to any other row.
fn row_stat(row: &[u8], width: usize, y: u32) -> RowStat {
    let mut min = u8::MAX;
    let mut max = u8::MIN;
    let mut sum = 0u32;
    let mut gradient = 0u32;
    let mut previous = row[0];
    for &value in row {
        min = min.min(value);
        max = max.max(value);
        sum += u32::from(value);
        gradient += u32::from(value.abs_diff(previous));
        previous = value;
    }
    RowStat {
        y,
        min,
        max,
        mean: sum as f32 / width as f32,
        gradient: if width > 1 {
            gradient as f64 / (width - 1) as f64
        } else {
            0.0
        },
    }
}
