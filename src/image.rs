//! Images: decoding PNG files, and scaling them to the size they're shown at.

use std::{
    io::Cursor,
    sync::atomic::{AtomicU64, Ordering},
};

/// Most pixels an image may have once decoded: 4096 × 4096.
const MAX_PIXELS: u64 = 4096 * 4096;

/// A decoded image, as premultiplied RGBA, four bytes a pixel.
pub struct Image {
    /// Distinguishes images in caches of their scaled copies.
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Shows the size, not the pixels.
impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Image({}×{})", self.width, self.height)
    }
}

impl Image {
    /// Decodes a PNG file.
    pub fn decode_png(bytes: &[u8]) -> Result<Image, String> {
        let failed = |e: png::DecodingError| format!("not a PNG image the browser can read: {e}");
        let mut decoder = png::Decoder::new(Cursor::new(bytes));
        // Palettes, low bit depths and 16-bit channels all become 8-bit colour.
        decoder.set_transformations(png::Transformations::normalize_to_color8());
        let mut reader = decoder.read_info().map_err(failed)?;
        let (width, height) = reader.info().size();
        if width as u64 * height as u64 > MAX_PIXELS {
            return Err(format!("the image is {width}×{height}, which is more than 4096×4096 pixels"));
        }
        let mut buf = vec![0; reader.output_buffer_size().ok_or("the image is too big")?];
        let info = reader.next_frame(&mut buf).map_err(failed)?;
        let data = &buf[..info.buffer_size()];

        let rgba: Vec<[u8; 4]> = match info.color_type {
            png::ColorType::Rgba => data.as_chunks::<4>().0.to_vec(),
            png::ColorType::Rgb => data.as_chunks::<3>().0.iter().map(|&[r, g, b]| [r, g, b, 255]).collect(),
            png::ColorType::GrayscaleAlpha => data.as_chunks::<2>().0.iter().map(|&[g, a]| [g, g, g, a]).collect(),
            png::ColorType::Grayscale => data.iter().map(|&g| [g, g, g, 255]).collect(),
            png::ColorType::Indexed => return Err("the image's palette wasn't expanded".into()),
        };
        let premultiply = |c: u8, a: u8| ((c as u16 * a as u16 + 127) / 255) as u8;
        let pixels = rgba.into_iter().flat_map(|[r, g, b, a]| [premultiply(r, a), premultiply(g, a), premultiply(b, a), a]).collect();

        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        Ok(Image { id: NEXT_ID.fetch_add(1, Ordering::Relaxed), width, height, pixels })
    }

    /// A copy `w` × `h` pixels in size, still premultiplied. Shrinking
    /// averages the pixels each new one covers, so detail blends rather than
    /// flickering in and out; enlarging blends neighbouring pixels.
    pub fn scaled(&self, w: u32, h: u32) -> Vec<u8> {
        let (sw, sh) = (self.width as usize, self.height as usize);
        let (w, h) = (w as usize, h as usize);
        let mut out = vec![0u8; w * h * 4];
        if sw == 0 || sh == 0 {
            return out;
        }
        let pixel = |x: usize, y: usize| &self.pixels[(y * sw + x) * 4..][..4];
        for y in 0..h {
            for x in 0..w {
                let mut sum = [0f32; 4];
                if w <= sw && h <= sh {
                    // The block of source pixels this one covers.
                    let (x0, y0) = (x * sw / w, y * sh / h);
                    let (x1, y1) = (((x + 1) * sw / w).max(x0 + 1), ((y + 1) * sh / h).max(y0 + 1));
                    for sy in y0..y1 {
                        for sx in x0..x1 {
                            for (s, &c) in sum.iter_mut().zip(pixel(sx, sy)) {
                                *s += c as f32;
                            }
                        }
                    }
                    let n = ((x1 - x0) * (y1 - y0)) as f32;
                    sum.iter_mut().for_each(|s| *s /= n);
                } else {
                    // Bilinear: the four source pixels around this one's centre.
                    let fx = ((x as f32 + 0.5) * sw as f32 / w as f32 - 0.5).clamp(0.0, (sw - 1) as f32);
                    let fy = ((y as f32 + 0.5) * sh as f32 / h as f32 - 0.5).clamp(0.0, (sh - 1) as f32);
                    let (x0, y0) = (fx as usize, fy as usize);
                    let (x1, y1) = ((x0 + 1).min(sw - 1), (y0 + 1).min(sh - 1));
                    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
                    for (sx, sy, weight) in [(x0, y0, (1.0 - tx) * (1.0 - ty)), (x1, y0, tx * (1.0 - ty)), (x0, y1, (1.0 - tx) * ty), (x1, y1, tx * ty)] {
                        for (s, &c) in sum.iter_mut().zip(pixel(sx, sy)) {
                            *s += c as f32 * weight;
                        }
                    }
                }
                for (o, s) in out[(y * w + x) * 4..][..4].iter_mut().zip(sum) {
                    *o = s.round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().unwrap().write_image_data(rgba).unwrap();
        bytes
    }

    #[test]
    fn decodes_and_premultiplies() {
        let image = Image::decode_png(&encode(2, 1, &[255, 0, 0, 255, 255, 255, 255, 0])).unwrap();
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.pixels, [255, 0, 0, 255, 0, 0, 0, 0], "a transparent pixel has no colour left");
        assert!(Image::decode_png(b"not a png").unwrap_err().contains("not a PNG"));
    }

    #[test]
    fn scaling_averages_or_blends() {
        // Black and white stripes, a pixel wide.
        let stripes: Vec<u8> = (0..4).flat_map(|i| if i % 2 == 0 { [0, 0, 0, 255] } else { [255, 255, 255, 255] }).collect();
        let image = Image::decode_png(&encode(4, 1, &stripes)).unwrap();
        assert_eq!(image.scaled(2, 1), [128, 128, 128, 255, 128, 128, 128, 255], "shrinking averages");
        let wide = image.scaled(8, 1);
        assert_eq!(&wide[..4], [0, 0, 0, 255], "the edges keep their colour");
        assert!(wide[4] > 0 && wide[4] < 255, "between pixels, they blend");
    }
}
