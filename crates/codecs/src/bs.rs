//! BS version 2: the `PlayStation`'s MDEC still pictures, PE2's backgrounds.
//!
//! An 8-byte header (`u16` MDEC code count / 2, magic `0x3800`, quantization scale, version), then a bitstream of
//! 16-bit little-endian words, each read from its most significant bit. Every 16x16 macroblock is six 8x8 blocks - Cr,
//! Cb, Y1..Y4 - each a signed 10-bit DC coefficient and MPEG-1 AC codes (a run of zeros, a level, a sign; `000001`
//! escapes a 6-bit run and a 10-bit level) up to the end-of-block code `10`. Coefficients go in zigzag order, are
//! dequantized (DC by 2, AC by the table times the scale over 8) and inverse-transformed with the MDEC's fixed-point
//! cosine table; macroblocks fill the picture column by column. Decoding is for display: BS is lossy.
//! (After `tools/peassets/bs_codec.py` and the public `PlayStation` STR documentation.)

use std::collections::HashMap;
use std::fmt;
use std::sync::LazyLock;

pub const MAGIC: u16 = 0x3800;
const HEADER: usize = 8;
/// PE2's backgrounds: 20 x 15 macroblocks.
pub const WIDTH: usize = 320;
pub const HEIGHT: usize = 240;

/// The AC codes without their sign bit: (code, zero run, level).
#[rustfmt::skip]
const AC_CODES: [(&str, u8, u8); 111] = [
    ("11", 0, 1), ("011", 1, 1), ("0100", 0, 2), ("0101", 2, 1),
    ("00101", 0, 3), ("00110", 4, 1), ("00111", 3, 1), ("000100", 7, 1),
    ("000101", 6, 1), ("000110", 1, 2), ("000111", 5, 1), ("0000100", 2, 2),
    ("0000101", 9, 1), ("0000110", 0, 4), ("0000111", 8, 1), ("00100000", 13, 1),
    ("00100001", 0, 6), ("00100010", 12, 1), ("00100011", 11, 1), ("00100100", 3, 2),
    ("00100101", 1, 3), ("00100110", 0, 5), ("00100111", 10, 1), ("0000001000", 16, 1),
    ("0000001001", 5, 2), ("0000001010", 0, 7), ("0000001011", 2, 3), ("0000001100", 1, 4),
    ("0000001101", 15, 1), ("0000001110", 14, 1), ("0000001111", 4, 2), ("000000010000", 0, 11),
    ("000000010001", 8, 2), ("000000010010", 4, 3), ("000000010011", 0, 10), ("000000010100", 2, 4),
    ("000000010101", 7, 2), ("000000010110", 21, 1), ("000000010111", 20, 1), ("000000011000", 0, 9),
    ("000000011001", 19, 1), ("000000011010", 18, 1), ("000000011011", 1, 5), ("000000011100", 3, 3),
    ("000000011101", 0, 8), ("000000011110", 6, 2), ("000000011111", 17, 1), ("0000000010000", 10, 2),
    ("0000000010001", 9, 2), ("0000000010010", 5, 3), ("0000000010011", 3, 4), ("0000000010100", 2, 5),
    ("0000000010101", 1, 7), ("0000000010110", 1, 6), ("0000000010111", 0, 15), ("0000000011000", 0, 14),
    ("0000000011001", 0, 13), ("0000000011010", 0, 12), ("0000000011011", 26, 1), ("0000000011100", 25, 1),
    ("0000000011101", 24, 1), ("0000000011110", 23, 1), ("0000000011111", 22, 1), ("00000000010000", 0, 31),
    ("00000000010001", 0, 30), ("00000000010010", 0, 29), ("00000000010011", 0, 28), ("00000000010100", 0, 27),
    ("00000000010101", 0, 26), ("00000000010110", 0, 25), ("00000000010111", 0, 24), ("00000000011000", 0, 23),
    ("00000000011001", 0, 22), ("00000000011010", 0, 21), ("00000000011011", 0, 20), ("00000000011100", 0, 19),
    ("00000000011101", 0, 18), ("00000000011110", 0, 17), ("00000000011111", 0, 16), ("000000000010000", 0, 40),
    ("000000000010001", 0, 39), ("000000000010010", 0, 38), ("000000000010011", 0, 37), ("000000000010100", 0, 36),
    ("000000000010101", 0, 35), ("000000000010110", 0, 34), ("000000000010111", 0, 33), ("000000000011000", 0, 32),
    ("000000000011001", 1, 14), ("000000000011010", 1, 13), ("000000000011011", 1, 12), ("000000000011100", 1, 11),
    ("000000000011101", 1, 10), ("000000000011110", 1, 9), ("000000000011111", 1, 8), ("0000000000010000", 1, 18),
    ("0000000000010001", 1, 17), ("0000000000010010", 1, 16), ("0000000000010011", 1, 15), ("0000000000010100", 6, 3),
    ("0000000000010101", 16, 2), ("0000000000010110", 15, 2), ("0000000000010111", 14, 2), ("0000000000011000", 13, 2),
    ("0000000000011001", 12, 2), ("0000000000011010", 11, 2), ("0000000000011011", 31, 1), ("0000000000011100", 30, 1),
    ("0000000000011101", 29, 1), ("0000000000011110", 28, 1), ("0000000000011111", 27, 1),
];

/// The AC codes by (length, value).
static AC: LazyLock<HashMap<(u32, u32), (u32, i32)>> = LazyLock::new(|| {
    AC_CODES
        .iter()
        .map(|&(code, run, level)| {
            let len = u32::try_from(code.len()).unwrap_or(0);
            ((len, u32::from_str_radix(code, 2).unwrap_or(0)), (u32::from(run), i32::from(level)))
        })
        .collect()
});

/// The coefficient index each zigzag position stands for, [row * 8 + column] in zigzag order.
const UNZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21,
    28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54,
    47, 55, 62, 63,
];

const QUANT: [f64; 64] = [
    2.0, 16.0, 19.0, 22.0, 26.0, 27.0, 29.0, 34.0, 16.0, 16.0, 22.0, 24.0, 27.0, 29.0, 34.0, 37.0, 19.0, 22.0, 26.0,
    27.0, 29.0, 34.0, 34.0, 38.0, 22.0, 22.0, 26.0, 27.0, 29.0, 34.0, 37.0, 40.0, 22.0, 26.0, 27.0, 29.0, 32.0, 35.0,
    40.0, 48.0, 26.0, 27.0, 29.0, 32.0, 35.0, 40.0, 48.0, 58.0, 26.0, 27.0, 29.0, 34.0, 38.0, 46.0, 56.0, 69.0, 27.0,
    29.0, 35.0, 38.0, 46.0, 56.0, 69.0, 83.0,
];

/// The MDEC's cosine table, [frequency][position], in 1/65536.
const COS: [[i32; 8]; 8] = [
    [23170, 23170, 23170, 23170, 23170, 23170, 23170, 23170],
    [32138, 27245, 18204, 6392, -6393, -18205, -27246, -32139],
    [30273, 12539, -12540, -30274, -30274, -12540, 12539, 30273],
    [27245, -6393, -32139, -18205, 18204, 32138, 6392, -27246],
    [23170, -23171, -23171, 23170, 23170, -23171, -23171, 23170],
    [18204, -32139, 6392, 27245, -27246, -6393, 32138, -18205],
    [12539, -30274, 30273, -12540, -12540, 30273, -30274, 12539],
    [6392, -18205, 27245, -32139, 32138, -27246, 18204, -6393],
];

#[derive(Debug)]
pub enum Error {
    NotBs,
    Version(u16),
    End,
    Code,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotBs => write!(f, "not a BS picture (no 0x3800 magic)"),
            Error::Version(v) => write!(f, "BS version {v}: only version 2 decodes"),
            Error::End => write!(f, "the bitstream ends before the picture"),
            Error::Code => write!(f, "an unknown AC code"),
        }
    }
}

impl std::error::Error for Error {}

/// A decoded picture: RGB, row by row.
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    word: u32,
    left: u32,
}

impl Bits<'_> {
    fn bits(&mut self, mut n: u32) -> Result<u32, Error> {
        let mut out = 0;
        while n > 0 {
            if self.left == 0 {
                let pair = self.data.get(self.pos..self.pos + 2).ok_or(Error::End)?;
                self.word = u32::from(u16::from_le_bytes([pair[0], pair[1]]));
                self.pos += 2;
                self.left = 16;
            }
            let take = n.min(self.left);
            out = (out << take) | ((self.word >> (self.left - take)) & ((1 << take) - 1));
            self.left -= take;
            n -= take;
        }
        Ok(out)
    }

    fn signed(&mut self, n: u32) -> Result<i32, Error> {
        let v = self.bits(n)?.cast_signed();
        Ok(if v & (1 << (n - 1)) != 0 { v - (1 << n) } else { v })
    }

    /// An AC coefficient as (zero run, level), or None at the end of the block.
    fn ac(&mut self) -> Result<Option<(u32, i32)>, Error> {
        let mut code = 0u32;
        for len in 1..=17 {
            code = (code << 1) | self.bits(1)?;
            if len == 2 && code == 0b10 {
                return Ok(None);
            }
            if len == 6 && code == 1 {
                let run = self.bits(6)?;
                return Ok(Some((run, self.signed(10)?)));
            }
            if let Some(&(run, level)) = AC.get(&(len, code)) {
                return Ok(Some((run, if self.bits(1)? == 1 { -level } else { level })));
            }
        }
        Err(Error::Code)
    }
}

/// One block's samples, [row * 8 + column], from its coefficients.
fn block(bits: &mut Bits<'_>, scale: f64) -> Result<[f64; 64], Error> {
    let mut coeffs = [0.0f64; 64];
    coeffs[0] = f64::from(bits.signed(10)?) * QUANT[0];
    let mut i = 0usize;
    while let Some((run, level)) = bits.ac()? {
        i += 1 + run as usize;
        if i >= 64 {
            break;
        }
        let at = UNZIGZAG[i];
        coeffs[at] = f64::from(level) * QUANT[at] * scale;
    }
    // out[y][x] = sum over v, u of C[v][y] C[u][x] F[v][u].
    let c = |f: usize, p: usize| f64::from(COS[f][p]) / 65536.0;
    let mut rows = [0.0f64; 64];
    for v in 0..8 {
        for x in 0..8 {
            rows[v * 8 + x] = (0..8).map(|u| coeffs[v * 8 + u] * c(u, x)).sum();
        }
    }
    let mut out = [0.0f64; 64];
    for y in 0..8 {
        for x in 0..8 {
            out[y * 8 + x] = (0..8).map(|v| rows[v * 8 + x] * c(v, y)).sum();
        }
    }
    Ok(out)
}

#[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to 0..=255 first")]
fn channel(v: f64) -> u8 {
    v.clamp(0.0, 255.0) as u8
}

/// Decode a BS version 2 picture of PE2's background size, 320x240.
///
/// # Errors
/// The bytes are not BS version 2, or the bitstream ends or breaks before the last macroblock.
pub fn decode(data: &[u8]) -> Result<Picture, Error> {
    let word = |i: usize| data.get(i..i + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
    if data.len() < HEADER || word(2) != MAGIC {
        return Err(Error::NotBs);
    }
    if word(6) != 2 {
        return Err(Error::Version(word(6)));
    }
    let scale = f64::from(word(4)) / 8.0;
    let mut bits = Bits { data, pos: HEADER, word: 0, left: 0 };
    let (mb_w, mb_h) = (WIDTH / 16, HEIGHT / 16);
    let mut rgb = vec![0u8; WIDTH * HEIGHT * 3];
    for mb in 0..mb_w * mb_h {
        let (mx, my) = (mb / mb_h, mb % mb_h);
        let cr = block(&mut bits, scale)?;
        let cb = block(&mut bits, scale)?;
        let mut luma = [[0.0f64; 64]; 4];
        for l in &mut luma {
            *l = block(&mut bits, scale)?;
        }
        for y in 0..16 {
            for x in 0..16 {
                let l = luma[(y / 8) * 2 + x / 8][(y % 8) * 8 + x % 8] + 128.0;
                let (r, b) = (cr[(y / 2) * 8 + x / 2], cb[(y / 2) * 8 + x / 2]);
                let at = ((my * 16 + y) * WIDTH + mx * 16 + x) * 3;
                rgb[at] = channel(l + 1.402 * r);
                rgb[at + 1] = channel(l - 0.3437 * b - 0.7143 * r);
                rgb[at + 2] = channel(l + 1.772 * b);
            }
        }
    }
    Ok(Picture { width: WIDTH, height: HEIGHT, rgb })
}
