//! The game's LZSS: a 256-byte ring, a flag bit per token (1 a literal byte, 0 a reference), a reference an 8-bit
//! ring offset and a 4-bit length (`length + 2` bytes copied); offset 0 ends the stream. Decoders start with a
//! cleared ring and the write cursor at 0, so a stored offset is one past its ring index (the runtime starts at 1).
//! Image strips are streams in a row, zero bytes between them.

/// Bits read most significant first; past the input, reads fail.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    byte: u8,
    mask: u8,
}

impl Bits<'_> {
    fn bits(&mut self, n: u32) -> Option<u32> {
        let mut x = 0;
        for _ in 0..n {
            if self.mask == 0 {
                self.byte = *self.data.get(self.pos)?;
                self.pos += 1;
                self.mask = 0x80;
            }
            x = (x << 1) | u32::from(self.byte & self.mask != 0);
            self.mask >>= 1;
        }
        Some(x)
    }
}

/// One stream from `start`: its bytes and where the input after it begins. A stream cut short by the end of the
/// input keeps what it decoded.
#[must_use]
pub fn decode_stream(data: &[u8], start: usize) -> (Vec<u8>, usize) {
    let (mut ring, mut cursor) = ([0u8; 256], 0usize);
    let mut out = Vec::new();
    let mut bits = Bits { data, pos: start, byte: 0, mask: 0 };
    while let Some(flag) = bits.bits(1) {
        if flag == 1 {
            let Some(byte) = bits.bits(8) else { break };
            #[expect(clippy::cast_possible_truncation, reason = "eight bits")]
            let byte = byte as u8;
            ring[cursor] = byte;
            cursor = (cursor + 1) & 0xFF;
            out.push(byte);
        } else {
            let Some(offset) = bits.bits(8) else { break };
            if offset == 0 {
                break;
            }
            let Some(length) = bits.bits(4) else { break };
            let mut at = (offset as usize).wrapping_sub(1) & 0xFF;
            for _ in 0..length + 2 {
                let byte = ring[at];
                ring[cursor] = byte;
                cursor = (cursor + 1) & 0xFF;
                out.push(byte);
                at = (at + 1) & 0xFF;
            }
        }
    }
    (out, bits.pos)
}

/// The streams from `start` on, each begun past the zero bytes before it, until one decodes to nothing.
#[must_use]
pub fn decode_streams(data: &[u8], mut start: usize) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        while data.get(start) == Some(&0) {
            start += 1;
        }
        if start >= data.len() {
            return out;
        }
        let (stream, next) = decode_stream(data, start);
        if stream.is_empty() {
            return out;
        }
        out.push(stream);
        start = next;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn literals_and_a_reference() {
        // 1 'A'(0x41), 1 'B'(0x42), then 0 offset=1 length=1 (copies 3 bytes from ring index 0), then 0 offset=0.
        let mut bits = ["1", "01000001", "1", "01000010", "0", "00000001", "0001", "0", "00000000"].concat();
        while bits.len() % 8 != 0 {
            bits.push('0');
        }
        let bytes: Vec<u8> = bits
            .as_bytes()
            .chunks(8)
            .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 2).unwrap())
            .collect();
        assert_eq!(super::decode_stream(&bytes, 0).0, b"ABABA");
    }
}
