use std::fs::{self, File};
use std::io::{self, Cursor, Read, Write};
use std::path::{Path, PathBuf};

use base64::Engine as _;

const AVATAR_FILE: &str = "profile-avatar.jpg";
const MAX_AVATAR_BYTES: usize = 16 * 1024;
const MAX_INPUT_BYTES: usize = 10 * 1024 * 1024;
const MAX_INPUT_PIXELS: u64 = 20_000_000;
const MAX_AVATAR_DIMENSION: u32 = 96;

pub(crate) fn avatar_path() -> Result<PathBuf, String> {
    Ok(crate::settings::data_directory()?.join(AVATAR_FILE))
}

pub(crate) fn load_avatar() -> Result<Option<Vec<u8>>, String> {
    let path = avatar_path()?;
    match load_avatar_from(&path) {
        Ok(avatar) => Ok(avatar),
        Err(error) => Err(format!(
            "Não foi possível carregar a foto do perfil: {error}"
        )),
    }
}

fn load_avatar_from(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    validate_avatar(&bytes)?;
    Ok(Some(bytes))
}

pub(crate) fn normalize_avatar(input: &[u8]) -> Result<Vec<u8>, String> {
    if input.is_empty() {
        return Err("O arquivo de imagem está vazio.".to_owned());
    }
    if input.len() > MAX_INPUT_BYTES {
        return Err("A imagem original excede o limite de 10 MiB.".to_owned());
    }

    let (format, width, height) = image_dimensions(input)?;
    if !matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg) {
        return Err("Escolha uma imagem PNG ou JPEG.".to_owned());
    }
    if u64::from(width) * u64::from(height) > MAX_INPUT_PIXELS {
        return Err("A imagem tem resolução excessiva para uma foto de perfil.".to_owned());
    }
    let decoded = image::load_from_memory(input)
        .map_err(|error| format!("Não foi possível decodificar a imagem: {error}"))?
        .to_rgb8();
    let source = image::DynamicImage::ImageRgb8(decoded);

    for dimension in [96, 80, 64, 48, 32] {
        let thumbnail = source.thumbnail(dimension, dimension).to_rgb8();
        let mut square =
            image::RgbImage::from_pixel(dimension, dimension, image::Rgb([64, 64, 64]));
        let x = (dimension - thumbnail.width()) / 2;
        let y = (dimension - thumbnail.height()) / 2;
        image::imageops::overlay(&mut square, &thumbnail, i64::from(x), i64::from(y));
        let normalized = image::DynamicImage::ImageRgb8(square);

        for quality in [82, 72, 62, 52, 42] {
            let mut output = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, quality)
                .encode_image(&normalized)
                .map_err(|error| format!("Não foi possível preparar a miniatura: {error}"))?;
            if output.len() <= MAX_AVATAR_BYTES {
                return Ok(output);
            }
        }
    }

    Err("Não foi possível reduzir a foto ao limite de 16 KiB.".to_owned())
}

pub(crate) fn normalize_avatar_file(path: &Path) -> Result<Vec<u8>, String> {
    let file =
        File::open(path).map_err(|error| format!("Não foi possível ler a imagem: {error}"))?;
    let mut bytes = Vec::new();
    file.take((MAX_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Não foi possível ler a imagem: {error}"))?;
    normalize_avatar(&bytes)
}

pub(crate) fn decode_avatar_payload(encoded: &str) -> Result<Vec<u8>, String> {
    let maximum_encoded_size = MAX_AVATAR_BYTES.div_ceil(3) * 4;
    if encoded.len() > maximum_encoded_size {
        return Err("A miniatura recebida excede o limite permitido.".to_owned());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| "A miniatura recebida não está codificada corretamente.".to_owned())?;
    validate_avatar(&bytes)?;
    Ok(bytes)
}

pub(crate) fn avatar_color_image(bytes: &[u8]) -> Result<eframe::egui::ColorImage, String> {
    validate_avatar(bytes)?;
    let image = image::load_from_memory(bytes)
        .map_err(|error| format!("Não foi possível decodificar a foto do perfil: {error}"))?
        .to_rgba8();
    let (width, height) = image.dimensions();
    Ok(eframe::egui::ColorImage::from_rgba_unmultiplied(
        [width as usize, height as usize],
        image.as_raw(),
    ))
}

pub(crate) fn store_avatar(bytes: &[u8]) -> Result<(), String> {
    let path = avatar_path()?;
    store_avatar_to(&path, bytes).map_err(|error| {
        format!(
            "Não foi possível salvar a foto do perfil em '{}': {error}",
            path.display()
        )
    })
}

fn store_avatar_to(path: &Path, bytes: &[u8]) -> io::Result<()> {
    validate_avatar(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "caminho sem pasta"))?;
    fs::create_dir_all(parent)?;
    let temporary_path = path.with_extension("jpg.tmp");
    let mut file = File::create(&temporary_path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    if let Err(error) = crate::settings::atomic_replace(&temporary_path, path) {
        let _ = fs::remove_file(&temporary_path);
        return Err(error);
    }
    Ok(())
}

pub(crate) fn remove_avatar() -> Result<(), String> {
    let path = avatar_path()?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "Não foi possível remover a foto do perfil em '{}': {error}",
            path.display()
        )),
    }
}

fn validate_avatar(bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty() || bytes.len() > MAX_AVATAR_BYTES {
        return Err("A miniatura precisa ter entre 1 byte e 16 KiB.".to_owned());
    }
    let (format, width, height) = image_dimensions(bytes)?;
    if format != image::ImageFormat::Jpeg
        || width > MAX_AVATAR_DIMENSION
        || height > MAX_AVATAR_DIMENSION
    {
        return Err("A foto do perfil precisa ser uma miniatura JPEG de até 96×96.".to_owned());
    }
    Ok(())
}

fn image_dimensions(bytes: &[u8]) -> Result<(image::ImageFormat, u32, u32), String> {
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| format!("Não foi possível identificar a imagem: {error}"))?;
    let format = reader
        .format()
        .ok_or_else(|| "O formato da imagem não foi reconhecido.".to_owned())?;
    let (width, height) = reader
        .into_dimensions()
        .map_err(|error| format!("Não foi possível ler as dimensões da imagem: {error}"))?;
    Ok((format, width, height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_path() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after UNIX epoch")
            .as_nanos();
        std::env::temp_dir()
            .join(format!("p2p-avatar-test-{}-{nonce}", std::process::id()))
            .join(AVATAR_FILE)
    }

    fn sample_png() -> Vec<u8> {
        let image = image::RgbImage::from_pixel(24, 16, image::Rgb([120, 140, 160]));
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .expect("sample PNG should encode");
        bytes.into_inner()
    }

    #[test]
    fn normalizes_png_into_small_square_jpeg() {
        let avatar = normalize_avatar(&sample_png()).expect("PNG should normalize");
        let (_, width, height) = image_dimensions(&avatar).expect("thumbnail should decode");
        assert_eq!((width, height), (96, 96));
        assert!(avatar.len() <= MAX_AVATAR_BYTES);
        validate_avatar(&avatar).expect("thumbnail should satisfy protocol constraints");
    }

    #[test]
    fn rejects_invalid_and_oversized_source_images() {
        assert!(normalize_avatar(b"not an image").is_err());
        assert!(normalize_avatar(&vec![0; MAX_INPUT_BYTES + 1]).is_err());
    }

    #[test]
    fn avatar_storage_replaces_and_reads_atomically() {
        let path = temporary_path();
        let first = normalize_avatar(&sample_png()).expect("sample should normalize");
        let second = normalize_avatar(&sample_png()).expect("sample should normalize");
        store_avatar_to(&path, &first).expect("first avatar should save");
        store_avatar_to(&path, &second).expect("replacement avatar should save");
        assert_eq!(load_avatar_from(&path).unwrap(), Some(second));
        fs::remove_dir_all(path.parent().unwrap()).expect("temporary avatar should be removed");
    }

    #[test]
    fn failed_avatar_replacement_preserves_previous_file() {
        let path = temporary_path();
        let previous = normalize_avatar(&sample_png()).expect("sample should normalize");
        store_avatar_to(&path, &previous).expect("previous avatar should save");

        fs::create_dir_all(path.with_extension("jpg.tmp"))
            .expect("blocker directory should be created");
        let replacement = normalize_avatar(&sample_png()).expect("sample should normalize");
        assert!(store_avatar_to(&path, &replacement).is_err());
        assert_eq!(load_avatar_from(&path).unwrap(), Some(previous));

        fs::remove_dir_all(path.parent().unwrap()).expect("temporary avatar should be removed");
    }
}
