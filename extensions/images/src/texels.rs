//! A texture's or atlas's native form as a value (`pe2.images@texels`): its texels at their depth - indices into its
//! palettes, or 16-bit colors - and each palette it declares with its colors, where they are stored (an executable may
//! upload them instead). Stored as files: `native.texels` as VRAM holds them, and a palette's colors in
//! `native.<palette>.colors` (ABGR1555); decoded from them once, by what takes it.

use pe2_ext::ty::{Registered, ValueType};
use pe2_ext::{Asset, Call, Data, File, Result, Table, Type, Typed, anyhow, bail};

use crate::{TEXELS, bpp, colors_file, halfwords, pair, palette_names};

/// The type: an image's texels and palettes.
pub struct Texels;

impl Registered for Texels {
    const NAME: &str = "pe2.images@texels";
    const LABEL: &str = "Texels and palettes";
}

/// An image's texels and palettes.
#[derive(Clone, Debug, PartialEq)]
pub struct TexelImage {
    /// In texels.
    pub width: usize,
    pub height: usize,
    /// Bits a texel: 4, 8 or 16.
    pub bpp: u8,
    /// As VRAM holds them: halfwords of packed indices, or 16-bit colors.
    pub texels: Vec<u8>,
    /// Each palette the image declares, in order, with its colors where they are stored.
    pub palettes: Vec<(String, Option<Vec<u16>>)>,
    /// What was derived with them: settings taking the place of the declaration's (its size, its cells' rectangles).
    pub derived: Table,
    /// What was taken otherwise than given.
    pub notes: Vec<String>,
}

impl TexelImage {
    /// An image's, read from its files and its settings (its size and mode, its palettes' names).
    ///
    /// # Errors
    /// It has no stored texels, no size, or a file cannot be read.
    pub fn read(asset: &Asset) -> Result<TexelImage> {
        let (width, height) = pair(asset.field("size")).ok_or_else(|| anyhow!("{}: no size", asset.path()))?;
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "image sizes")]
        let (width, height) = (width as usize, height as usize);
        let palettes = palette_names(asset)
            .into_iter()
            .map(|name| {
                let file = colors_file(&name);
                let colors = asset.has_file(&file).then(|| asset.read(&file).map(|b| halfwords(&b))).transpose()?;
                Ok((name, colors))
            })
            .collect::<Result<_>>()?;
        let texels = asset.read(TEXELS)?;
        Ok(TexelImage { width, height, bpp: bpp(asset), texels, palettes, derived: Table::new(), notes: Vec::new() })
    }

    /// Each palette's colors, in the order declared: none where they are not stored.
    #[must_use]
    pub fn colors(&self) -> Vec<Option<Vec<u16>>> {
        self.palettes.iter().map(|(_, colors)| colors.clone()).collect()
    }

    /// A palette's colors, by its name.
    #[must_use]
    pub fn palette(&self, name: &str) -> Option<&[u16]> {
        self.palettes.iter().find(|(n, _)| n == name)?.1.as_deref()
    }

    /// Its files, as stored: each stored palette's colors, then the texels.
    fn files(&self) -> Vec<File> {
        let colors = self.palettes.iter().filter_map(|(name, colors)| {
            let colors = colors.as_ref()?;
            Some(File::new(&colors_file(name), colors.iter().flat_map(|c| c.to_le_bytes()).collect()))
        });
        colors.chain(std::iter::once(File::new(TEXELS, self.texels.clone()))).collect()
    }
}

impl ValueType for Texels {
    type Value = TexelImage;

    fn ty() -> Type {
        Type::Named(Texels::NAME.to_string())
    }

    fn decode(data: Data, call: &Call<'_>) -> Result<TexelImage> {
        let typed = match data {
            Data::Typed(t) if t.ty == Texels::NAME => t,
            other => bail!("given {}, not {}", other.ty(), Texels::NAME),
        };
        let lent = call.lend(&typed);
        let mut image = TexelImage::read(&lent.asset())?;
        image.derived = typed.settings;
        image.notes = typed.notes;
        Ok(image)
    }

    fn encode(image: TexelImage) -> Data {
        let files = image.files();
        Data::Typed(Typed {
            ty: Texels::NAME.to_string(),
            files,
            settings: image.derived,
            notes: image.notes,
            ..Typed::default()
        })
    }
}
