//! pe2.movies: what a movie is, in whatever form it is kept: pictures and sound played in time. A form's extension
//! (`pe2.str`) adds to the kind what it takes to read one: its stack, its `stream` of `Playback` and which frame shows it.
//! Here is what holds for any: what a stream of a movie carries, and a frame taken from one (`frame`). Without such an
//! extension a movie is its files.

use pe2_ext::media::VideoFrame;
use pe2_ext::ty::{self, Registered, ValueType};
use pe2_ext::{Call, Data, Icon, Image, Registry, Result, Tint, Type, anyhow};

pe2_ext::extension!("pe2.movies", "0.1.0", register);

/// An item of a movie's stream: a list of a frame's picture and the sound up to it (`Data::Video`, `Data::Audio`).
pub struct Playback;

impl ValueType for Playback {
    type Value = Data;

    fn ty() -> Type {
        Type::Named(Self::NAME.to_string())
    }

    fn decode(data: Data, _: &Call<'_>) -> Result<Data> {
        Ok(data)
    }

    fn encode(value: Data) -> Data {
        value
    }
}

impl Registered for Playback {
    const NAME: &str = "pe2.movies@playback";
    const LABEL: &str = "Playback";
}

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let _ = r.value_type::<Playback>();
    let _ = r.op("frame").label("Frame").input::<ty::Stream<Playback>>("frames").output::<ty::Image>().parameter::<u32>("frame").run(|call| {
        let number = call.parameter::<u32>()?;
        let frames = call.take::<ty::Stream<Playback>>(0)?;
        frames.seek(number.into());
        let item = frames.next()?.ok_or_else(|| anyhow!("no frame {number}"))?;
        picture(&item).ok_or_else(|| anyhow!("frame {number} has no picture"))?
    });
    let _ = r.kind("movie").alias("movie").label("Movie").icon(Icon::Film, Tint::Motion).register();
    Ok(())
}

/// An item's picture, as an image; none where it has none.
fn picture(item: &Data) -> Option<Result<Image>> {
    let frame = |data: &Data| if let Data::Video(frame) = data { Some(image(frame)) } else { None };
    match item {
        Data::List(parts) => parts.iter().find_map(frame),
        other => frame(other),
    }
}

fn image(frame: &VideoFrame) -> Result<Image> {
    Ok(Image { width: frame.width as usize, height: frame.height as usize, rgba: frame.rgba()? })
}
