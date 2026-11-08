//! pe2.formats: the common file types, recognized for every extension that takes them - by their own bytes, never by a
//! file's name, which proves nothing. An operation takes one of these by its marker (`pe2_formats::Png`) or, not to
//! depend on who recognizes it, by a marker of its media type (`image/png`).

use pe2_ext::ty::{Files, Registered};
use pe2_ext::{Registry, Result, Signature};

pe2_ext::extension!(register);

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let _ = r.value_type::<Png>();
    let _ = r.value_type::<Ora>();
    let _ = r.value_type::<Zip>();
    let _ = r.value_type::<Text>();
    let _ = r.value_type::<Wav>();
    Ok(())
}

/// These bytes at the start of a file.
fn at(offset: usize, bytes: &[u8]) -> Signature {
    Signature::Bytes { offset, bytes: bytes.to_vec() }
}

/// A PNG image.
pub struct Png;

impl Registered for Png {
    const NAME: &str = "pe2.formats@png";
    const LABEL: &str = "PNG image";
    const MEDIA_TYPE: Option<&str> = Some("image/png");

    fn signatures() -> Vec<Signature> {
        vec![at(0, b"\x89PNG\r\n\x1a\n"), Signature::Name("*.png".into())]
    }
}

impl Files for Png {}

/// An `OpenRaster` image: a zip whose first entry, stored, is `mimetype` - its name at 30, its content after it.
pub struct Ora;

impl Registered for Ora {
    const NAME: &str = "pe2.formats@ora";
    const LABEL: &str = "OpenRaster image";
    const MEDIA_TYPE: Option<&str> = Some("image/openraster");

    fn signatures() -> Vec<Signature> {
        vec![at(30, b"mimetypeimage/openraster"), Signature::Name("*.ora".into())]
    }
}

impl Files for Ora {}

/// A zip archive.
pub struct Zip;

impl Registered for Zip {
    const NAME: &str = "pe2.formats@zip";
    const LABEL: &str = "Zip archive";
    const MEDIA_TYPE: Option<&str> = Some("application/zip");

    fn signatures() -> Vec<Signature> {
        vec![at(0, b"PK\x03\x04")]
    }
}

impl Files for Zip {}

/// A WAV file: a RIFF file whose form is `WAVE`.
pub struct Wav;

impl Registered for Wav {
    const NAME: &str = "pe2.formats@wav";
    const LABEL: &str = "WAV sound";
    const MEDIA_TYPE: Option<&str> = Some("audio/wav");

    fn signatures() -> Vec<Signature> {
        vec![at(8, b"WAVE"), Signature::Name("*.wav".into())]
    }
}

impl Files for Wav {}

/// Text: UTF-8 without a NUL byte.
pub struct Text;

impl Registered for Text {
    const NAME: &str = "pe2.formats@text";
    const LABEL: &str = "Text";
    const MEDIA_TYPE: Option<&str> = Some("text/plain");

    fn signatures() -> Vec<Signature> {
        vec![Signature::Text]
    }
}

impl Files for Text {}
