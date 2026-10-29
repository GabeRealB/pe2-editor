//! pe2.formats: the common file formats, recognized for every extension that reads them - by their own bytes, never
//! by a file's name, which proves nothing. An importer reads one of these by its name (`pe2.formats@png`) or, not to
//! depend on who recognizes it, by its media type (`image/png`).

use pe2_ext::{Registry, Result};

pe2_ext::extension!("pe2.formats", "0.1.0", register);

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    r.format("png").label("PNG image").media_type("image/png").bytes(0, b"\x89PNG\r\n\x1a\n").register();
    // An OpenRaster file is a zip whose first entry, stored, is `mimetype`: its name at 30, its content after it.
    r.format("ora")
        .label("OpenRaster image")
        .media_type("image/openraster")
        .bytes(30, b"mimetypeimage/openraster")
        .register();
    r.format("zip").label("Zip archive").media_type("application/zip").bytes(0, b"PK\x03\x04").register();
    r.format("text").label("Text").media_type("text/plain").text().register();
    Ok(())
}
