//! Input tokens of an inline image from its real dimensions: the width and height from a PNG, JPEG,
//! GIF or WebP header, and the image-token formula of the provider that will read it.

/// A provider's image-token formula.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ImageFormula {
    /// 512-pixel tiles after fitting in 2048 × 2048 and the short side in 768; `detail: low` is a
    /// flat 85.
    OpenAiTiles,
    /// `w × h / 750` after fitting the long side in 1568 pixels.
    Anthropic,
    /// 258 per image up to 384 × 384, else 258 per 768-pixel tile.
    Gemini,
}

impl ImageFormula {
    /// The formula of a provider type, when one is known.
    #[must_use]
    pub fn of_provider(kind: &str) -> Option<ImageFormula> {
        match kind {
            "openai" | "azure_openai" => Some(ImageFormula::OpenAiTiles),
            "anthropic" => Some(ImageFormula::Anthropic),
            "gemini" => Some(ImageFormula::Gemini),
            _ => None,
        }
    }

    /// The tokens of a `w × h` image; `low` is OpenAI's `detail: low`.
    #[must_use]
    pub fn tokens(self, w: u32, h: u32, low: bool) -> u64 {
        let (w, h) = (f64::from(w.max(1)), f64::from(h.max(1)));
        match self {
            ImageFormula::OpenAiTiles => {
                if low {
                    return 85;
                }
                let fit = (2048.0 / w.max(h)).min(1.0);
                let (w, h) = (w * fit, h * fit);
                let short = (768.0 / w.min(h)).min(1.0);
                let (w, h) = ((w * short).floor(), (h * short).floor());
                let tiles = (w / 512.0).ceil() * (h / 512.0).ceil();
                85 + 170 * (tiles as u64)
            }
            ImageFormula::Anthropic => {
                let fit = (1568.0 / w.max(h)).min(1.0);
                ((w * fit).floor() * (h * fit).floor() / 750.0).ceil() as u64
            }
            ImageFormula::Gemini => {
                if w <= 384.0 && h <= 384.0 {
                    258
                } else {
                    258 * ((w / 768.0).ceil() * (h / 768.0).ceil()) as u64
                }
            }
        }
    }
}

fn be16(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes(b.get(i..i + 2)?.try_into().ok()?)))
}

fn le16(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?)))
}

fn le24(b: &[u8], i: usize) -> Option<u32> {
    let s = b.get(i..i + 3)?;
    Some(s.iter().rev().fold(0u32, |acc, x| (acc << 8) | u32::from(*x)))
}

fn png(b: &[u8]) -> Option<(u32, u32)> {
    if b.get(0..8)? != b"\x89PNG\r\n\x1a\n" || b.get(12..16)? != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(b.get(16..20)?.try_into().ok()?);
    let h = u32::from_be_bytes(b.get(20..24)?.try_into().ok()?);
    Some((w, h))
}

fn gif(b: &[u8]) -> Option<(u32, u32)> {
    let sig = b.get(0..6)?;
    if sig != b"GIF87a" && sig != b"GIF89a" {
        return None;
    }
    Some((le16(b, 6)?, le16(b, 8)?))
}

fn jpeg(b: &[u8]) -> Option<(u32, u32)> {
    if b.get(0..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut i = 2;
    loop {
        while *b.get(i)? != 0xFF {
            i += 1;
        }
        while *b.get(i)? == 0xFF {
            i += 1;
        }
        let marker = *b.get(i)?;
        i += 1;
        if matches!(marker, 0xD8 | 0x01 | 0xD0..=0xD7) {
            continue;
        }
        let len = usize::try_from(be16(b, i)?).ok().filter(|l| *l >= 2)?;
        let sof = matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if sof {
            return Some((be16(b, i + 5)?, be16(b, i + 3)?));
        }
        i = i.checked_add(len)?;
    }
}

fn webp(b: &[u8]) -> Option<(u32, u32)> {
    if b.get(0..4)? != b"RIFF" || b.get(8..12)? != b"WEBP" {
        return None;
    }
    match b.get(12..16)? {
        b"VP8 " => Some((le16(b, 26)? & 0x3FFF, le16(b, 28)? & 0x3FFF)),
        b"VP8L" => {
            let v = u32::from_le_bytes(b.get(21..25)?.try_into().ok()?);
            Some(((v & 0x3FFF) + 1, ((v >> 14) & 0x3FFF) + 1))
        }
        b"VP8X" => Some((le24(b, 24)? + 1, le24(b, 27)? + 1)),
        _ => None,
    }
}

/// The width and height in an image's header; `None` for another format or a header cut short.
#[must_use]
pub fn dimensions(b: &[u8]) -> Option<(u32, u32)> {
    png(b)
        .or_else(|| jpeg(b))
        .or_else(|| gif(b))
        .or_else(|| webp(b))
        .filter(|(w, h)| *w > 0 && *h > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_of(w: u32, h: u32) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&[8, 6, 0, 0, 0]);
        b
    }

    fn jpeg_of(w: u16, h: u16) -> Vec<u8> {
        let mut b = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x4A, 0x46];
        b.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&[3, 1, 0x22, 0]);
        b
    }

    #[test]
    fn headers() {
        assert_eq!(dimensions(&png_of(512, 300)), Some((512, 300)));
        assert_eq!(dimensions(&jpeg_of(2048, 1024)), Some((2048, 1024)));
        assert_eq!(dimensions(b"GIF89a\x10\x00\x20\x00"), Some((16, 32)));
        let mut vp8x = b"RIFF\0\0\0\0WEBPVP8X\0\0\0\0\0\0\0\0".to_vec();
        vp8x.extend_from_slice(&[0xFF, 0x01, 0x00, 0x3F, 0x00, 0x00]);
        assert_eq!(dimensions(&vp8x), Some((512, 64)));
        assert_eq!(dimensions(&png_of(512, 300)[..20]), None);
        assert_eq!(dimensions(b"not an image"), None);
    }

    #[test]
    fn formulas() {
        let t = ImageFormula::OpenAiTiles;
        assert_eq!(t.tokens(512, 512, false), 85 + 170);
        assert_eq!(t.tokens(1024, 1024, false), 765);
        assert_eq!(t.tokens(2048, 2048, false), 765);
        assert_eq!(t.tokens(2048, 4096, false), 1105);
        assert_eq!(t.tokens(4000, 4000, true), 85);
        assert_eq!(ImageFormula::Anthropic.tokens(1000, 1000, false), 1334);
        assert_eq!(ImageFormula::Gemini.tokens(300, 300, false), 258);
        assert_eq!(ImageFormula::Gemini.tokens(1000, 1000, false), 258 * 4);
        assert_eq!(ImageFormula::of_provider("bedrock"), None);
    }
}
