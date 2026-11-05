//! The game's LZSS: a 256-byte ring, a flag bit per token (1 a literal byte, 0 a reference), a reference an 8-bit
//! ring offset and a 4-bit length (`length + 2` bytes copied); offset 0 ends the stream. Decoders start with a
//! cleared ring and the write cursor at 0, so a stored offset is one past its ring index (the runtime starts at 1).
//! Image strips are streams in a row, zero bytes between them.
//!
//! The streams were made by Mark Nelson's LZSS (The Data Compression Book, 1991) with an 8-bit index and a 4-bit
//! length: a window of 256, a lookahead of 17, `END_OF_STREAM` and `UNUSED` both 0. `encode` is that encoder, and makes
//! every stream of both discs' stage containers byte for byte (`examples/lzss_identity.rs`) - packages, image strips
//! and palettes alike. What makes it that encoder and not another is in its search: one binary tree of the strings
//! at each window position, a match taken from the last node at the longest length on the path, a deleted node
//! replaced by its predecessor, an equal string of 17 bytes replaced by the newest. And in what `UNUSED` = 0 does:
//! position 0 is never inserted nor searched from, `tree[0]` takes the writes meant for an absent child, and
//! deleting position 0 acts on them, cutting a node's smaller subtree off the tree once a lap of the window.

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

const WINDOW: usize = 256;
const LOOK_AHEAD: usize = 17;
/// A reference shorter than this is a literal: `(1 + 8 + 4) / 9`.
const BREAK_EVEN: usize = 1;
const ROOT: usize = WINDOW;
/// Both no node and the end of the stream.
const UNUSED: usize = 0;

#[derive(Clone, Copy, Default)]
struct Node {
    parent: usize,
    smaller: usize,
    larger: usize,
}

/// The encoder's state: the window and the tree of the strings at its positions, `tree[ROOT].larger` its root.
struct Encoder {
    window: [u8; WINDOW],
    tree: [Node; WINDOW + 1],
}

impl Encoder {
    /// `new` takes `old`'s place under its parent.
    fn contract(&mut self, old: usize, new: usize) {
        let parent = self.tree[old].parent;
        self.tree[new].parent = parent;
        if self.tree[parent].larger == old {
            self.tree[parent].larger = new;
        } else {
            self.tree[parent].smaller = new;
        }
        self.tree[old].parent = UNUSED;
    }

    /// `new` takes `old`'s place and its children.
    fn replace(&mut self, old: usize, new: usize) {
        let parent = self.tree[old].parent;
        if self.tree[parent].smaller == old {
            self.tree[parent].smaller = new;
        } else {
            self.tree[parent].larger = new;
        }
        self.tree[new] = self.tree[old];
        let (smaller, larger) = (self.tree[new].smaller, self.tree[new].larger);
        self.tree[smaller].parent = new;
        self.tree[larger].parent = new;
        self.tree[old].parent = UNUSED;
    }

    /// The node before `node`: the largest of its smaller subtree.
    fn predecessor(&self, node: usize) -> usize {
        let mut next = self.tree[node].smaller;
        while self.tree[next].larger != UNUSED {
            next = self.tree[next].larger;
        }
        next
    }

    fn delete(&mut self, p: usize) {
        if self.tree[p].parent == UNUSED {
            return;
        }
        if self.tree[p].larger == UNUSED {
            self.contract(p, self.tree[p].smaller);
        } else if self.tree[p].smaller == UNUSED {
            self.contract(p, self.tree[p].larger);
        } else {
            let replacement = self.predecessor(p);
            self.delete(replacement);
            self.replace(p, replacement);
        }
    }

    /// Insert the string at `new`, searching on the way: the longest match, its position.
    fn add(&mut self, new: usize) -> (usize, usize) {
        if new == UNUSED {
            return (0, 0);
        }
        let (mut test, mut length, mut position) = (self.tree[ROOT].larger, 0, 0);
        loop {
            let (mut i, mut delta) = (0, 0i32);
            while i < LOOK_AHEAD {
                delta = i32::from(self.window[(new + i) % WINDOW]) - i32::from(self.window[(test + i) % WINDOW]);
                if delta != 0 {
                    break;
                }
                i += 1;
            }
            if i >= length {
                (length, position) = (i, test);
                if length >= LOOK_AHEAD {
                    self.replace(test, new);
                    return (length, position);
                }
            }
            let child = if delta >= 0 { self.tree[test].larger } else { self.tree[test].smaller };
            if child == UNUSED {
                if delta >= 0 {
                    self.tree[test].larger = new;
                } else {
                    self.tree[test].smaller = new;
                }
                self.tree[new] = Node { parent: test, smaller: UNUSED, larger: UNUSED };
                return (length, position);
            }
            test = child;
        }
    }
}

/// Bits written most significant first.
#[derive(Default)]
struct BitWriter {
    out: Vec<u8>,
    byte: u8,
    count: u32,
}

impl BitWriter {
    fn put(&mut self, value: usize, bits: u32) {
        for i in (0..bits).rev() {
            self.byte = (self.byte << 1) | u8::from((value >> i) & 1 == 1);
            self.count += 1;
            if self.count == 8 {
                self.out.push(self.byte);
                (self.byte, self.count) = (0, 0);
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.count > 0 {
            self.out.push(self.byte << (8 - self.count));
        }
        self.out
    }
}

/// One stream of `data`, as the game's tools made it, through its end token.
#[must_use]
pub fn encode(data: &[u8]) -> Vec<u8> {
    let mut e = Encoder { window: [0; WINDOW], tree: [Node::default(); WINDOW + 1] };
    let mut bits = BitWriter::default();
    let mut input = data.iter().copied();
    let mut current = 1;
    let mut look_ahead = 0;
    while look_ahead < LOOK_AHEAD {
        let Some(c) = input.next() else { break };
        e.window[current + look_ahead] = c;
        look_ahead += 1;
    }
    e.tree[ROOT].larger = current;
    e.tree[current] = Node { parent: ROOT, smaller: UNUSED, larger: UNUSED };
    let (mut length, mut position) = (0, 0);
    while look_ahead > 0 {
        length = length.min(look_ahead);
        let count = if length <= BREAK_EVEN {
            bits.put(1, 1);
            bits.put(usize::from(e.window[current]), 8);
            1
        } else {
            bits.put(0, 1);
            bits.put(position, 8);
            bits.put(length - (BREAK_EVEN + 1), 4);
            length
        };
        for _ in 0..count {
            let incoming = (current + LOOK_AHEAD) % WINDOW;
            e.delete(incoming);
            match input.next() {
                Some(c) => e.window[incoming] = c,
                None => look_ahead -= 1,
            }
            current = (current + 1) % WINDOW;
            if look_ahead > 0 {
                (length, position) = e.add(current);
            }
        }
    }
    bits.put(0, 1);
    bits.put(UNUSED, 8);
    bits.finish()
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

    /// Inputs of the shapes the format has edges at: empty, shorter than a lookahead, runs (references overlapping
    /// what they copy), the longest reference, more than a window, and noise.
    fn samples() -> Vec<Vec<u8>> {
        let mut noise = Vec::new();
        let mut x: u32 = 1;
        for _ in 0..5000 {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            noise.push(x.to_le_bytes()[2]);
        }
        let text = b"the game's tools made every stream with this, and this is what they made ".repeat(20);
        vec![
            Vec::new(),
            vec![7],
            b"ABABA".to_vec(),
            vec![0; 3000],
            vec![0x55; 17],
            vec![0x55; 18],
            (0..=255).collect(),
            text,
            noise,
        ]
    }

    #[test]
    fn encoded_streams_decode_back() {
        for data in samples() {
            let stream = super::encode(&data);
            let (decoded, end) = super::decode_stream(&stream, 0);
            assert_eq!(decoded, data);
            assert_eq!(end, stream.len(), "the stream ends at its end token");
        }
    }

    #[test]
    fn runs_take_the_longest_references() {
        // A literal, then a reference to it overlapping itself: 17 at most a reference.
        let stream = super::encode(&[9; 35]);
        assert!(stream.len() <= 2 + 3 * 2 + 2, "{stream:02x?}");
    }
}
