//! What is marked on an image's pictures: an atlas's cells, a wide texture's pages.

use pe2_ext::{Asset, Mark, Marks, Rect};

use crate::{cells, is_atlas, is_texture, pages};

/// What is marked on an image: nothing, but for an atlas's cells and a texture wider than a page's pages.
pub(crate) fn of(asset: &Asset) -> Marks {
    let ((width, height), page_width) = pages(asset);
    if is_atlas(asset) {
        let boxes: Vec<Mark> = cells(asset).into_iter().map(|c| Mark { name: c.name, rect: c.rect }).collect();
        let n = boxes.len();
        Marks {
            boxes,
            summary: format!("{n} cell{}", if n == 1 { "" } else { "s" }),
            toggle: "Cells".into(),
            legend: true,
        }
    } else if is_texture(asset) && width > page_width {
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a page count")]
        let n = (width / page_width).ceil() as usize;
        #[expect(clippy::cast_precision_loss, reason = "a page count")]
        let page = |k: usize| {
            let x = k as f32 * page_width;
            Mark { name: format!("page {k}"), rect: Rect { x, y: 0.0, width: (width - x).min(page_width), height } }
        };
        let summary = format!("{n} pages of {page_width} texels");
        Marks { boxes: (0..n).map(page).collect(), summary, toggle: "Pages".into(), legend: true }
    } else {
        Marks::default()
    }
}
