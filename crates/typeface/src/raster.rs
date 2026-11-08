//! A glyph's outlines drawn as the game's art: a glyph of a UFO with contours and no image is rasterized at the face's
//! size, in the UI faces' style - the shape anti-aliased into the letter's values (`fill`, 11-15), a ring around it
//! (`outline` texels wide) into the outline's (6-10), nothing elsewhere.
//!
//! Each texel's coverage is counted on a grid of `SAMPLES` x `SAMPLES` points, a point inside where the contours wind
//! around it (nonzero); the ring's is the shape grown by its width, a point inside where it lies within that distance of
//! one inside the shape. A texel any of the shape covers takes the letter's value for how much, one the ring alone does
//! the outline's.
//!
//! Font units go up from the baseline; texels go down, row -1 the last above the baseline (the row a record's `yOffset`
//! of 0 draws its last on).

use codecs::ufo;

use crate::{Art, TRANSPARENT};

/// Points a texel's coverage is counted on, along each side.
const SAMPLES: usize = 8;
/// Line segments a curve is drawn with.
const STEPS: usize = 16;

/// How outlines are drawn: texels to the em, the ring's width in texels, the values a texel takes for the letter and for
/// the ring, first (least covered) to last.
#[derive(Clone, Debug, PartialEq)]
pub struct Raster {
    pub pixels_per_em: Option<f64>,
    pub outline: f64,
    pub fill: (u8, u8),
    pub ring: (u8, u8),
}

impl Default for Raster {
    /// The UI faces' style: a unit a texel, a ring a texel wide, the letter 11-15 and the ring 6-10.
    fn default() -> Raster {
        Raster { pixels_per_em: None, outline: 1.0, fill: (11, 15), ring: (6, 10) }
    }
}

/// A glyph rasterized: its art, the texel column its left edge is on (from the pen) and the row its last row is on
/// (from the baseline's, -1).
pub struct Drawn {
    pub art: Art,
    pub left: i64,
    pub bottom: i64,
}

type Polygon = Vec<(f64, f64)>;

/// A glyph's outlines as the game draws them; none for one that has none, or whose outlines cover nothing.
///
/// # Errors
/// A component names a glyph the font lacks, or components name each other round.
pub fn rasterize(font: &ufo::Font, glyph: &ufo::Glyph, raster: &Raster) -> Result<Option<Drawn>, String> {
    let units = font.info.iter().find(|(key, _)| key == "unitsPerEm").and_then(|(_, v)| v.as_f64()).unwrap_or(1000.0);
    let scale = raster.pixels_per_em.map_or(1.0, |pixels| pixels / units);
    let mut polygons = Vec::new();
    outlines(font, glyph, [scale, 0.0, 0.0, scale, 0.0, 0.0], &mut polygons, 0)?;
    // In texels, down: y flipped.
    for polygon in &mut polygons {
        for point in polygon.iter_mut() {
            point.1 = -point.1;
        }
    }
    let points = polygons.iter().flatten();
    let Some(((x0, y0), (x1, y1))) = points.fold(None, |bounds: Option<((f64, f64), (f64, f64))>, &(x, y)| {
        Some(bounds.map_or(((x, y), (x, y)), |((a, b), (c, d))| ((a.min(x), b.min(y)), (c.max(x), d.max(y)))))
    }) else {
        return Ok(None);
    };
    // The texels the shape and its ring can reach.
    let reach = raster.outline.max(0.0).ceil();
    #[expect(clippy::cast_possible_truncation, reason = "a glyph's few texels")]
    let (left, top, right, bottom) = (
        (x0 - reach).floor() as i64,
        (y0 - reach).floor() as i64,
        (x1 + reach).ceil() as i64,
        (y1 + reach).ceil() as i64,
    );
    let (width, height) = (usize::try_from(right - left).unwrap_or(0), usize::try_from(bottom - top).unwrap_or(0));
    if width == 0 || height == 0 {
        return Ok(None);
    }
    let (sw, sh) = (width * SAMPLES, height * SAMPLES);
    let inside = fill(&polygons, (left, top), (sw, sh));
    let near = grow(&inside, (sw, sh), raster.outline * f64::from(u32::try_from(SAMPLES).unwrap_or(8)));
    let mut texels = vec![TRANSPARENT; width * height];
    let full = SAMPLES * SAMPLES;
    for row in 0..height {
        for col in 0..width {
            let count = |mask: &[bool]| {
                (0..SAMPLES)
                    .flat_map(|j| (0..SAMPLES).map(move |i| ((row * SAMPLES + j) * sw) + col * SAMPLES + i))
                    .filter(|&at| mask[at])
                    .count()
            };
            let (shape, ring) = (count(&inside), count(&near));
            texels[row * width + col] = if shape > 0 {
                level(raster.fill, shape, full)
            } else if ring > 0 {
                level(raster.ring, ring, full)
            } else {
                TRANSPARENT
            };
        }
    }
    // Cut to what it paints.
    let painted = |col: usize, row: usize| texels[row * width + col] != TRANSPARENT;
    let rows: Vec<usize> = (0..height).filter(|&row| (0..width).any(|col| painted(col, row))).collect();
    let cols: Vec<usize> = (0..width).filter(|&col| (0..height).any(|row| painted(col, row))).collect();
    let (Some(&r0), Some(&r1), Some(&c0), Some(&c1)) = (rows.first(), rows.last(), cols.first(), cols.last()) else {
        return Ok(None);
    };
    let (aw, ah) = (c1 - c0 + 1, r1 - r0 + 1);
    let mut art = vec![TRANSPARENT; aw * ah];
    for row in 0..ah {
        let from = (r0 + row) * width;
        art[row * aw..(row + 1) * aw].copy_from_slice(&texels[from + c0..=from + c1]);
    }
    let signed = |n: usize| i64::try_from(n).unwrap_or(i64::MAX);
    Ok(Some(Drawn {
        art: Art { width: aw, height: ah, texels: art },
        left: left + signed(c0),
        bottom: top + signed(r1),
    }))
}

/// A coverage's value: of the values from first to last, the one as far along as the coverage, rounded up.
fn level((first, last): (u8, u8), count: usize, full: usize) -> u8 {
    let steps = usize::from(last.saturating_sub(first)) + 1;
    let step = (count * steps).div_ceil(full).clamp(1, steps) - 1;
    first + u8::try_from(step).unwrap_or(0)
}

/// A glyph's contours as polygons, in units scaled by `transform`, its components' with theirs.
fn outlines(
    font: &ufo::Font,
    glyph: &ufo::Glyph,
    transform: [f64; 6],
    out: &mut Vec<Polygon>,
    depth: usize,
) -> Result<(), String> {
    if depth > 16 {
        return Err(format!("{}: its components name each other round", glyph.name));
    }
    let [xx, xy, yx, yy, dx, dy] = transform;
    let apply = |(x, y): (f64, f64)| (xx * x + yx * y + dx, xy * x + yy * y + dy);
    for contour in &glyph.contours {
        if let Some(polygon) = flatten(contour) {
            out.push(polygon.into_iter().map(apply).collect());
        }
    }
    for component in &glyph.components {
        let base = font
            .glyphs
            .iter()
            .find(|g| g.name == component.base)
            .ok_or_else(|| format!("{}: no glyph {}", glyph.name, component.base))?;
        let [cxx, cxy, cyx, cyy, cdx, cdy] = component.transform;
        // This glyph's transform after the component's.
        let combined = [
            xx * cxx + yx * cxy,
            xy * cxx + yy * cxy,
            xx * cyx + yx * cyy,
            xy * cyx + yy * cyy,
            xx * cdx + yx * cdy + dx,
            xy * cdx + yy * cdy + dy,
        ];
        outlines(font, base, combined, out, depth + 1)?;
    }
    Ok(())
}

/// A closed contour as a polygon: its lines, its cubic curves (`curve`, after two off-curve points) and its quadratic ones
/// (`qcurve`, after any, an on-curve point implied between each two; a contour of off-curve points alone is one round
/// the midpoints) drawn in `STEPS` segments; none for an open one.
fn flatten(contour: &[ufo::Point]) -> Option<Polygon> {
    if contour.first()?.kind.as_deref() == Some("move") {
        return None;
    }
    let point = |p: &ufo::Point| (p.x, p.y);
    let mut polygon = Vec::new();
    if let Some(at) = contour.iter().position(|p| p.kind.is_some()) {
        let points: Vec<&ufo::Point> = contour[at..].iter().chain(&contour[..at]).collect();
        polygon.push(point(points[0]));
        let mut off = Vec::new();
        for p in points[1..].iter().chain(std::iter::once(&points[0])) {
            match p.kind.as_deref() {
                None => off.push(point(p)),
                Some(kind) => {
                    segment(&mut polygon, kind, &off, point(p));
                    off.clear();
                }
            }
        }
    } else {
        let controls: Vec<(f64, f64)> = contour.iter().map(point).collect();
        let (last, first) = (controls[controls.len() - 1], controls[0]);
        let start = (f64::midpoint(last.0, first.0), f64::midpoint(last.1, first.1));
        polygon.push(start);
        quadratic(&mut polygon, &controls, start);
    }
    (polygon.len() >= 3).then_some(polygon)
}

/// A segment from the polygon's last point to `to`: a line, a cubic curve through two off-curve points, else
/// quadratic ones through any.
fn segment(polygon: &mut Polygon, kind: &str, off: &[(f64, f64)], to: (f64, f64)) {
    let Some(&from) = polygon.last() else { return };
    match (kind, off) {
        (_, []) => polygon.push(to),
        ("curve", [p1, p2]) => {
            for t in steps() {
                let u = 1.0 - t;
                let x = u * u * u * from.0 + 3.0 * u * u * t * p1.0 + 3.0 * u * t * t * p2.0 + t * t * t * to.0;
                let y = u * u * u * from.1 + 3.0 * u * u * t * p1.1 + 3.0 * u * t * t * p2.1 + t * t * t * to.1;
                polygon.push((x, y));
            }
        }
        _ => quadratic(polygon, off, to),
    }
}

/// Quadratic curves through each control point in turn, from the polygon's last point to `end`: between two controls,
/// an on-curve point at their midpoint.
fn quadratic(polygon: &mut Polygon, controls: &[(f64, f64)], end: (f64, f64)) {
    let Some(&start) = polygon.last() else { return };
    let mut from = start;
    for (i, control) in controls.iter().enumerate() {
        let to = controls
            .get(i + 1)
            .map_or(end, |next| (f64::midpoint(control.0, next.0), f64::midpoint(control.1, next.1)));
        for t in steps() {
            let u = 1.0 - t;
            polygon.push((
                u * u * from.0 + 2.0 * u * t * control.0 + t * t * to.0,
                u * u * from.1 + 2.0 * u * t * control.1 + t * t * to.1,
            ));
        }
        from = to;
    }
}

/// The places along a curve its segments end at, past its start.
fn steps() -> impl Iterator<Item = f64> {
    (1..=STEPS).map(|step| f64::from(u32::try_from(step).unwrap_or(0)) / f64::from(u32::try_from(STEPS).unwrap_or(16)))
}

/// Which of a grid's points (`size`, `SAMPLES` to a texel, its first texel at `origin`) the polygons wind around.
fn fill(polygons: &[Polygon], origin: (i64, i64), (width, height): (usize, usize)) -> Vec<bool> {
    let samples = f64::from(u32::try_from(SAMPLES).unwrap_or(8));
    #[expect(clippy::cast_precision_loss, reason = "a glyph's few texels")]
    let (ox, oy) = (origin.0 as f64, origin.1 as f64);
    let mut inside = vec![false; width * height];
    for row in 0..height {
        #[expect(clippy::cast_precision_loss, reason = "a glyph's few texels")]
        let y = oy + (row as f64 + 0.5) / samples;
        // Where each edge crosses the row, and which way it winds.
        let mut crossings: Vec<(f64, i32)> = Vec::new();
        for polygon in polygons {
            for (i, &(x0, y0)) in polygon.iter().enumerate() {
                let (x1, y1) = polygon[(i + 1) % polygon.len()];
                if (y0 <= y) != (y1 <= y) {
                    crossings.push((x0 + (y - y0) / (y1 - y0) * (x1 - x0), if y1 > y0 { 1 } else { -1 }));
                }
            }
        }
        crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut winding = 0;
        let mut next = crossings.iter().peekable();
        for col in 0..width {
            #[expect(clippy::cast_precision_loss, reason = "a glyph's few texels")]
            let x = ox + (col as f64 + 0.5) / samples;
            while let Some(&&(at, turn)) = next.peek()
                && at <= x
            {
                winding += turn;
                next.next();
            }
            inside[row * width + col] = winding != 0;
        }
    }
    inside
}

/// The points within `radius` of one inside: the inside grown, by the squared distance to the nearest point inside
/// (Felzenszwalb and Huttenlocher's transform, by columns then rows).
fn grow(inside: &[bool], (width, height): (usize, usize), radius: f64) -> Vec<bool> {
    const FAR: f64 = 1e20;
    if radius <= 0.0 {
        return inside.to_vec();
    }
    let mut distance: Vec<f64> = inside.iter().map(|&i| if i { 0.0 } else { FAR }).collect();
    let mut line = Vec::new();
    for col in 0..width {
        line.clear();
        line.extend((0..height).map(|row| distance[row * width + col]));
        let done = transform(&line);
        for (row, d) in done.into_iter().enumerate() {
            distance[row * width + col] = d;
        }
    }
    for row in 0..height {
        let done = transform(&distance[row * width..(row + 1) * width]);
        distance[row * width..(row + 1) * width].copy_from_slice(&done);
    }
    distance.into_iter().map(|d| d <= radius * radius).collect()
}

/// One dimension of the squared distance transform: each place's least `f(q) + (p - q)^2`.
fn transform(cost: &[f64]) -> Vec<f64> {
    let len = cost.len();
    let mut out = vec![0.0; len];
    if len == 0 {
        return out;
    }
    let mut hull = vec![0usize; len];
    let mut bounds = vec![0.0f64; len + 1];
    let mut k = 0;
    bounds[0] = f64::NEG_INFINITY;
    bounds[1] = f64::INFINITY;
    #[expect(clippy::cast_precision_loss, reason = "a glyph's few texels")]
    let at = |i: usize| i as f64;
    for q in 1..len {
        loop {
            let p = hull[k];
            let meet = ((cost[q] + at(q) * at(q)) - (cost[p] + at(p) * at(p))) / (2.0 * at(q) - 2.0 * at(p));
            if meet <= bounds[k] && k > 0 {
                k -= 1;
                continue;
            }
            if meet <= bounds[k] {
                // The first parabola is passed by this one everywhere.
                hull[0] = q;
                bounds[1] = f64::INFINITY;
                break;
            }
            k += 1;
            hull[k] = q;
            bounds[k] = meet;
            bounds[k + 1] = f64::INFINITY;
            break;
        }
    }
    let mut k = 0;
    for (q, value) in out.iter_mut().enumerate() {
        while bounds[k + 1] < at(q) {
            k += 1;
        }
        let p = hull[k];
        *value = (at(q) - at(p)) * (at(q) - at(p)) + cost[p];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Raster, rasterize};
    use crate::TRANSPARENT;
    use codecs::ufo;

    /// A square glyph, 4 x 4 units from (1, 0), a unit a texel.
    fn square() -> (ufo::Font, ufo::Glyph) {
        let point = |x: f64, y: f64| ufo::Point { x, y, kind: Some("line".into()), ..ufo::Point::default() };
        let glyph = ufo::Glyph {
            name: "square".into(),
            advance: 6.0,
            contours: vec![vec![point(1.0, 0.0), point(1.0, 4.0), point(5.0, 4.0), point(5.0, 0.0)]],
            ..ufo::Glyph::default()
        };
        let font = ufo::Font {
            info: vec![("unitsPerEm".into(), ufo::Plist::Integer(16))],
            glyphs: vec![glyph.clone()],
            ..ufo::Font::default()
        };
        (font, glyph)
    }

    #[test]
    fn a_square_on_texels_is_the_letter_whole_in_a_ring_a_texel_wide() {
        let (font, glyph) = square();
        let drawn =
            rasterize(&font, &glyph, &Raster { pixels_per_em: Some(16.0), ..Raster::default() }).unwrap().unwrap();
        // 4 x 4 of the letter in a ring: 6 x 6, its left a texel before the square's, its last row below the baseline's.
        assert_eq!((drawn.art.width, drawn.art.height, drawn.left, drawn.bottom), (6, 6, 0, 0));
        let at = |x: usize, y: usize| drawn.art.texels[y * 6 + x];
        assert_eq!(at(2, 2), 15, "inside, whole");
        assert_eq!(at(0, 2), 10, "the ring beside an edge, whole");
        assert!(at(0, 0) < 10 && at(0, 0) >= 6, "the ring at a corner, rounded: {}", at(0, 0));
        // Without a ring, the letter alone.
        let bare = rasterize(&font, &glyph, &Raster { pixels_per_em: Some(16.0), outline: 0.0, ..Raster::default() })
            .unwrap()
            .unwrap();
        assert_eq!((bare.art.width, bare.art.height, bare.left, bare.bottom), (4, 4, 1, -1));
        assert!(bare.art.texels.iter().all(|t| *t == 15));
        // Half the size, from 0.5 to 2.5: three columns, the outer two half covered - the third of the five values.
        let half = rasterize(&font, &glyph, &Raster { pixels_per_em: Some(8.0), outline: 0.0, ..Raster::default() })
            .unwrap()
            .unwrap();
        assert_eq!((half.art.width, half.art.height), (3, 2));
        assert_eq!(&half.art.texels[..3], &[13, 15, 13]);
        assert!(!half.art.texels.contains(&TRANSPARENT));
    }

    #[test]
    fn a_component_is_its_base_moved() {
        let (mut font, square) = square();
        // The square, moved 3 units right and 2 up.
        let moved = ufo::Glyph {
            name: "moved".into(),
            advance: 9.0,
            components: vec![ufo::Component { base: "square".into(), transform: [1.0, 0.0, 0.0, 1.0, 3.0, 2.0] }],
            ..ufo::Glyph::default()
        };
        font.glyphs.push(moved.clone());
        let raster = Raster { pixels_per_em: Some(16.0), outline: 0.0, ..Raster::default() };
        let base = rasterize(&font, &square, &raster).unwrap().unwrap();
        let drawn = rasterize(&font, &moved, &raster).unwrap().unwrap();
        assert_eq!((drawn.left, drawn.bottom), (base.left + 3, base.bottom - 2));
        assert_eq!(drawn.art, base.art);
    }

    #[test]
    fn a_quadratic_contour_of_off_curve_points_alone_is_round() {
        // A circle of four off-curve points, radius about 4 round (5, 5).
        let off = |x: f64, y: f64| ufo::Point { x, y, ..ufo::Point::default() };
        let glyph = ufo::Glyph {
            name: "o".into(),
            advance: 10.0,
            contours: vec![vec![off(1.0, 1.0), off(1.0, 9.0), off(9.0, 9.0), off(9.0, 1.0)]],
            ..ufo::Glyph::default()
        };
        let font = ufo::Font {
            info: vec![("unitsPerEm".into(), ufo::Plist::Integer(16))],
            glyphs: vec![glyph.clone()],
            ..ufo::Font::default()
        };
        let drawn = rasterize(&font, &glyph, &Raster { pixels_per_em: Some(16.0), outline: 0.0, ..Raster::default() })
            .unwrap()
            .unwrap();
        let at = |x: usize, y: usize| drawn.art.texels[y * drawn.art.width + x];
        let (w, h) = (drawn.art.width, drawn.art.height);
        assert_eq!(at(w / 2, h / 2), 15, "its middle whole");
        assert!(at(0, 0) == TRANSPARENT || at(0, 0) < 15, "its corners rounded off");
    }
}
