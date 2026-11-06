//! pe2.movies: what a movie is, in whatever form it is kept: pictures and sound played in time. A form's extension
//! (`pe2.str`) says what makes one a movie: its stack, its stream (`ty::Playback`) and which frame shows it. Without such
//! an extension a movie is its files.

use pe2_ext::{Icon, Registry, Result, Tint};

pe2_ext::extension!(register);

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let _ = r.named_type("movie").alias("movie").label("Movie").icon(Icon::Film, Tint::Motion).register();
    Ok(())
}
