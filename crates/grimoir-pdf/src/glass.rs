//! The "ink on glass" filter used by the reader's transparent mode.
//!
//! The filter estimates the paper color, makes near-paper pixels transparent,
//! and keeps ink/text opaque. Saturated pixels are treated as artwork and are
//! preserved, so color pages do not disappear.

use rayon::prelude::*;

use crate::RawPage;

/// Returns a new RGBA buffer with the paper keyed out.
///
/// `paper_opacity` is the opacity of the paper itself (1.0 keeps the page
/// opaque; 0.0 removes the paper completely). `ink_strength` controls how
/// aggressively near-paper shades are classified as paper.
pub fn apply_ink_on_glass(page: &RawPage, paper_opacity: f32, ink_strength: f32) -> Vec<u8> {
    let mut output = page.rgba.clone();
    let paper = estimate_paper_color(page);
    let paper_alpha = (paper_opacity.clamp(0.0, 1.0) * 255.0).round();

    let strength = ink_strength.clamp(0.0, 1.0);
    let low = 0.004 + 0.020 * (1.0 - strength);
    let high = 0.080 + 0.150 * (1.0 - strength);

    // Work in parallel by scanline. A full A4 page at 1500×2200 has ~3.3
    // million pixels; doing this serially on the GTK thread is the main cost
    // of turning a page in glass mode.
    let row_bytes = page.width.max(1) as usize * 4;
    output.par_chunks_mut(row_bytes).for_each(|row| {
        for pixel in row.chunks_exact_mut(4) {
            let r = pixel[0] as i32;
            let g = pixel[1] as i32;
            let b = pixel[2] as i32;

            let max = r.max(g).max(b);
            let min = r.min(g).min(b);

            // Preserve saturated artwork (manga color pages, covers, diagrams).
            if max - min > 48 && max > 64 {
                pixel[3] = 255;
                continue;
            }

            let dr = r - paper.0 as i32;
            let dg = g - paper.1 as i32;
            let db = b - paper.2 as i32;
            let distance_squared = (dr * dr + dg * dg + db * db) as f32;
            let normalized = distance_squared / 195_075.0; // 3 * 255^2

            let ink = smoothstep(low, high, normalized);
            let alpha = paper_alpha + (255.0 - paper_alpha) * ink;
            pixel[3] = alpha.round().clamp(0.0, 255.0) as u8;
        }
    });

    output
}

/// Estimates the page's paper color.
///
/// Most pages are light paper with dark ink, so the brightest decile is a good
/// paper sample. Some manga pages invert this: a black background with white
/// lettering. When the median is dark, sample the darkest decile instead, so
/// the black field becomes the transparent "paper" and the white lettering
/// stays opaque.
fn estimate_paper_color(page: &RawPage) -> (u8, u8, u8) {
    let pixel_count = page.rgba.len() / 4;
    if pixel_count == 0 {
        return (255, 255, 255);
    }

    // Sample roughly 4096 pixels across the whole page, not only the border:
    // scanned pages often have black borders or cropped edges.
    let step = (pixel_count / 4096).max(1);
    let mut samples: Vec<(u32, u8, u8, u8)> = Vec::with_capacity(pixel_count / step + 1);

    let mut index = 0;
    while index < pixel_count {
        let offset = index * 4;
        let r = page.rgba[offset];
        let g = page.rgba[offset + 1];
        let b = page.rgba[offset + 2];
        let luma = (r as u32 * 30 + g as u32 * 59 + b as u32 * 11) / 100;
        samples.push((luma, r, g, b));
        index += step;
    }

    samples.sort_unstable_by_key(|sample| sample.0);
    let median = samples[samples.len() / 2].0;
    let inverted = median < 96;
    let percentile = if inverted { 0.10 } else { 0.90 };
    let index = ((samples.len() as f32 * percentile) as usize).min(samples.len() - 1);
    let (luma, r, g, b) = samples[index];

    // A page with no obvious paper (full-bleed dark art) is safer keyed
    // against white than against an arbitrary mid-tone.
    if luma < 100 && !inverted {
        (255, 255, 255)
    } else {
        (r, g, b)
    }
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if edge0 >= edge1 {
        return if x >= edge1 { 1.0 } else { 0.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(pixels: &[[u8; 4]]) -> RawPage {
        let mut rgba = Vec::with_capacity(pixels.len() * 4);
        for pixel in pixels {
            rgba.extend_from_slice(pixel);
        }
        RawPage {
            width: pixels.len() as u32,
            height: 1,
            rgba,
        }
    }

    #[test]
    fn paper_becomes_transparent_and_ink_stays_opaque() {
        let white = [255, 255, 255, 255];
        let black = [0, 0, 0, 255];
        let page = page(&[white, black]);
        let output = apply_ink_on_glass(&page, 0.0, 0.8);
        assert!(output[3] < 20, "paper alpha was {}", output[3]);
        assert_eq!(output[7], 255);
    }

    #[test]
    fn inverted_page_keeps_light_text_opaque() {
        let black = [0, 0, 0, 255];
        let white = [255, 255, 255, 255];
        let page = page(&[black, black, black, white]);
        let output = apply_ink_on_glass(&page, 0.0, 0.8);
        assert!(output[3] < 20, "black paper alpha was {}", output[3]);
        assert_eq!(output[15], 255, "white lettering alpha was {}", output[15]);
    }

    #[test]
    fn saturated_art_is_preserved() {
        let red = [220, 40, 40, 255];
        let page = page(&[red]);
        let output = apply_ink_on_glass(&page, 0.0, 1.0);
        assert_eq!(output[3], 255);
    }

    #[test]
    fn paper_opacity_keeps_some_alpha() {
        let page = page(&[[255, 255, 255, 255]]);
        let output = apply_ink_on_glass(&page, 0.82, 0.8);
        assert!(output[3] > 200);
    }
}
