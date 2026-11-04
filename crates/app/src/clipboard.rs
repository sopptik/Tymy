//! Čtení schránky včetně obrázků (iced umí jen text).

use std::sync::Arc;

/// Obsah schránky při vložení.
#[derive(Debug, Clone)]
pub enum Clip {
    /// Obrázek převedený do PNG.
    Image {
        png: Arc<Vec<u8>>,
        width: u32,
        height: u32,
    },
    Text(String),
    Empty,
}

/// Přečte schránku: přednostně obrázek, jinak text. Blokující, volat mimo hlavní vlákno.
pub fn read() -> Clip {
    let mut cb = match arboard::Clipboard::new() {
        Ok(cb) => cb,
        Err(e) => {
            tracing::warn!(error = %e, "schránku nelze otevřít");
            return Clip::Empty;
        }
    };
    match cb.get_image() {
        Ok(img) => match encode_png(img.width as u32, img.height as u32, &img.bytes) {
            Ok(png) => {
                return Clip::Image {
                    png: Arc::new(png),
                    width: img.width as u32,
                    height: img.height as u32,
                };
            }
            Err(e) => tracing::warn!(error = %e, "obrázek ze schránky nelze převést"),
        },
        Err(arboard::Error::ContentNotAvailable) => {}
        Err(e) => tracing::debug!(error = %e, "obrázek ve schránce není"),
    }
    match cb.get_text() {
        Ok(t) if !t.is_empty() => Clip::Text(t),
        _ => Clip::Empty,
    }
}

/// RGBA → PNG.
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, png::EncodingError> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, width, height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header()?;
        writer.write_image_data(rgba)?;
    }
    Ok(out)
}

/// Rozměry pro zobrazení ve zprávě: max. 600 px na šířku, poměr stran zachován.
pub fn display_size(width: u32, height: u32) -> (u32, u32) {
    const MAX: u32 = 600;
    if width <= MAX || width == 0 {
        return (width, height);
    }
    (
        MAX,
        (height as u64 * MAX as u64 / width as u64).max(1) as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_valid_png() {
        let png = encode_png(2, 1, &[255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
    }

    #[test]
    fn scales_display_size() {
        assert_eq!(display_size(300, 200), (300, 200));
        assert_eq!(display_size(1200, 800), (600, 400));
    }
}
