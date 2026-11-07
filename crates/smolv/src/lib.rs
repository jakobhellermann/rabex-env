//! [SMOL-V](https://github.com/aras-p/smol-v), a compact lossless encoding of SPIR-V, as used by
//! Unity for its Vulkan shaders. A port of Aras Pranckevičius' reference implementation.

use std::fmt;

mod ops;

use ops::OP_DATA;

const SPIRV_MAGIC: u32 = 0x0723_0203;
const SMOLV_MAGIC: u32 = 0x534D_4F4C;
const CURRENT_VERSION: u32 = 1;

// SPIR-V opcodes the encoding treats specially
const OP_NOP: u32 = 0;
const OP_UNDEF: u32 = 1;
const OP_SOURCE_CONTINUED: u32 = 2;
const OP_SOURCE: u32 = 3;
const OP_SOURCE_EXTENSION: u32 = 4;
const OP_STRING: u32 = 7;
const OP_LINE: u32 = 8;
const OP_EXTENSION: u32 = 10;
const OP_EXT_INST_IMPORT: u32 = 11;
/// Not SPIR-V: a `VectorShuffle` with up to four components below 4, packed into one byte.
const OP_VECTOR_SHUFFLE_COMPACT: u32 = 13;
const OP_MEMORY_MODEL: u32 = 14;
const OP_ENTRY_POINT: u32 = 15;
const OP_TYPE_POINTER: u32 = 32;
const OP_VARIABLE: u32 = 59;
const OP_LOAD: u32 = 61;
const OP_STORE: u32 = 62;
const OP_ACCESS_CHAIN: u32 = 65;
const OP_DECORATE: u32 = 71;
const OP_MEMBER_DECORATE: u32 = 72;
const OP_VECTOR_SHUFFLE: u32 = 79;
const OP_FNEGATE: u32 = 127;
const OP_FADD: u32 = 129;
const OP_FMUL: u32 = 133;
const OP_CONTROL_BARRIER: u32 = 224;
const OP_MEMORY_BARRIER: u32 = 225;
const OP_LOOP_MERGE: u32 = 246;
const OP_SELECTION_MERGE: u32 = 247;
const OP_LABEL: u32 = 248;
const OP_BRANCH: u32 = 249;
const OP_BRANCH_CONDITIONAL: u32 = 250;
const OP_MEMORY_NAMED_BARRIER: u32 = 329;
const OP_MODULE_PROCESSED: u32 = 330;

const DECORATION_OFFSET: u32 = 35;

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    InvalidSpirv(&'static str),
    InvalidSmolv(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidSpirv(reason) => write!(f, "invalid SPIR-V: {reason}"),
            Error::InvalidSmolv(reason) => write!(f, "invalid SMOL-V: {reason}"),
        }
    }
}

impl std::error::Error for Error {}

/// Number of opcodes an encoding version has properties for.
fn known_ops(version: u32) -> u32 {
    match version {
        0 => OP_MODULE_PROCESSED + 1,
        _ => OP_DATA.len() as u32,
    }
}

fn op_data(op: u32, known: u32) -> Option<&'static ops::OpData> {
    (op < known).then(|| &OP_DATA[op as usize])
}

/// Swaps common opcodes into the range below 16 for one-byte encodings, and back.
fn remap_op(op: u32) -> u32 {
    const SWAPS: [(u32, u32); 12] = [
        (OP_DECORATE, OP_NOP),
        (OP_LOAD, OP_UNDEF),
        (OP_STORE, OP_SOURCE_CONTINUED),
        (OP_ACCESS_CHAIN, OP_SOURCE),
        (OP_VECTOR_SHUFFLE, OP_SOURCE_EXTENSION),
        (OP_MEMBER_DECORATE, OP_STRING),
        (OP_LABEL, OP_LINE),
        (OP_VARIABLE, 9),
        (OP_FMUL, OP_EXTENSION),
        (OP_FADD, OP_EXT_INST_IMPORT),
        (OP_TYPE_POINTER, OP_MEMORY_MODEL),
        (OP_FNEGATE, OP_ENTRY_POINT),
    ];
    for (a, b) in SWAPS {
        if op == a {
            return b;
        }
        if op == b {
            return a;
        }
    }
    op
}

/// How much shorter than its word count an instruction's encoded length is.
fn length_bias(op: u32) -> u32 {
    1 + match op {
        OP_VECTOR_SHUFFLE | OP_VECTOR_SHUFFLE_COMPACT => 4,
        OP_DECORATE => 2,
        OP_LOAD | OP_ACCESS_CHAIN => 3,
        _ => 0,
    }
}

/// Extra words of a member decoration with a known shape, `None` if its length is encoded.
fn decoration_extra_words(decoration: u32) -> Option<u32> {
    match decoration {
        0 | 2..=5 => Some(0),
        29..=37 => Some(1),
        _ => None,
    }
}

fn zig(value: u32) -> u32 {
    let i = value as i32;
    ((i as u32) << 1) ^ ((i >> 31) as u32)
}

fn unzig(value: u32) -> u32 {
    if value & 1 != 0 {
        (value >> 1) ^ !0
    } else {
        value >> 1
    }
}

struct Writer(Vec<u8>);

impl Writer {
    fn word(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn varint(&mut self, mut value: u32) {
        while value > 127 {
            self.0.push((value & 127) as u8 | 128);
            value >>= 7;
        }
        self.0.push(value as u8);
    }
}

/// Encode a SPIR-V module, keeping its debug information.
pub fn encode(spirv: &[u32]) -> Result<Vec<u8>, Error> {
    let header = spirv
        .get(..5)
        .ok_or(Error::InvalidSpirv("shorter than its header"))?;
    if header[0] != SPIRV_MAGIC {
        return Err(Error::InvalidSpirv("wrong magic"));
    }
    if !(0x0001_0000..=0x0001_0600).contains(&header[1]) {
        return Err(Error::InvalidSpirv("unsupported version"));
    }
    let mut w = Writer(Vec::with_capacity(spirv.len() * 2));
    w.word(SMOLV_MAGIC);
    w.word((header[1] & 0x00FF_FFFF) | CURRENT_VERSION << 24);
    w.word(header[2]);
    w.word(header[3]);
    w.word(header[4]);
    w.word(u32::try_from(spirv.len() * 4).map_err(|_| Error::InvalidSpirv("too large"))?);

    let known = known_ops(CURRENT_VERSION);
    let mut prev_result = 0u32;
    let mut prev_decorate = 0u32;
    let mut pos = 5;
    while pos < spirv.len() {
        let len = (spirv[pos] >> 16) as usize;
        if len == 0 {
            return Err(Error::InvalidSpirv("zero length instruction"));
        }
        let instruction = spirv
            .get(pos..pos + len)
            .ok_or(Error::InvalidSpirv("instruction past the end"))?;
        let mut op = spirv[pos] & 0xFFFF;

        let mut swizzle = 0u32;
        if op == OP_VECTOR_SHUFFLE && len <= 9 {
            let component = |i: usize| instruction.get(i).copied().unwrap_or(0);
            let components = [component(5), component(6), component(7), component(8)];
            if components.iter().all(|&c| c < 4) {
                op = OP_VECTOR_SHUFFLE_COMPACT;
                swizzle =
                    components[0] << 6 | components[1] << 4 | components[2] << 2 | components[3];
            }
        }

        let encoded_len = (len as u32).wrapping_sub(length_bias(op));
        if encoded_len > 0xFFFF {
            return Err(Error::InvalidSpirv(
                "instruction shorter than its opcode requires",
            ));
        }
        let remapped = remap_op(op);
        w.varint(
            (encoded_len >> 4) << 20
                | (remapped >> 4) << 8
                | (encoded_len & 0xF) << 4
                | (remapped & 0xF),
        );

        let data = op_data(op, known);
        let mut i = 1;
        let next = |i: &mut usize| -> Result<u32, Error> {
            let word = *instruction
                .get(*i)
                .ok_or(Error::InvalidSpirv("instruction shorter than its operands"))?;
            *i += 1;
            Ok(word)
        };
        if data.is_some_and(|d| d.has_type) {
            w.varint(next(&mut i)?);
        }
        if data.is_some_and(|d| d.has_result) {
            let result = next(&mut i)?;
            w.varint(zig(result.wrapping_sub(prev_result)));
            prev_result = result;
        }
        if op == OP_DECORATE || op == OP_MEMBER_DECORATE {
            let target = next(&mut i)?;
            w.varint(zig(target.wrapping_sub(prev_decorate)));
            prev_decorate = target;
        }

        if op == OP_MEMBER_DECORATE {
            // a run of member decorations of the same type is encoded as one
            let target = prev_decorate;
            let count_at = w.0.len();
            w.0.push(0);
            let (mut count, mut prev_index, mut prev_offset) = (0u8, 0u32, 0u32);
            while pos < spirv.len() && count < 255 {
                let len = (spirv[pos] >> 16) as usize;
                if len == 0 {
                    return Err(Error::InvalidSpirv("zero length instruction"));
                }
                let member = spirv
                    .get(pos..pos + len)
                    .ok_or(Error::InvalidSpirv("instruction past the end"))?;
                if member[0] & 0xFFFF != OP_MEMBER_DECORATE {
                    break;
                }
                if len < 4 {
                    return Err(Error::InvalidSpirv("short member decoration"));
                }
                if member[1] != target {
                    break;
                }
                w.varint(member[2].wrapping_sub(prev_index));
                prev_index = member[2];
                let decoration = member[3];
                w.varint(decoration);
                match decoration_extra_words(decoration) {
                    None => w.varint(len as u32 - 4),
                    Some(extra) if extra as usize + 4 != len => {
                        return Err(Error::InvalidSpirv(
                            "member decoration of unexpected length",
                        ));
                    }
                    Some(_) => {}
                }
                if decoration == DECORATION_OFFSET {
                    if len != 5 {
                        return Err(Error::InvalidSpirv(
                            "offset decoration of unexpected length",
                        ));
                    }
                    w.varint(member[4].wrapping_sub(prev_offset));
                    prev_offset = member[4];
                } else {
                    for &word in &member[4..] {
                        w.varint(word);
                    }
                }
                pos += len;
                count += 1;
            }
            w.0[count_at] = count;
            continue;
        }

        let deltas = data.map_or(0, |d| d.delta_from_result as usize);
        for _ in 0..deltas {
            if i >= len {
                break;
            }
            w.varint(zig(prev_result.wrapping_sub(instruction[i])));
            i += 1;
        }

        if op == OP_VECTOR_SHUFFLE_COMPACT {
            w.0.push(swizzle as u8);
        } else if data.is_some_and(|d| d.var_rest) {
            for &word in &instruction[i..] {
                w.varint(word);
            }
        } else {
            for &word in &instruction[i..] {
                w.word(word);
            }
        }
        pos += len;
    }
    Ok(w.0)
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn word(&mut self) -> Result<u32, Error> {
        let bytes = self
            .data
            .get(self.pos..self.pos + 4)
            .ok_or(Error::InvalidSmolv("truncated word"))?;
        self.pos += 4;
        Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn byte(&mut self) -> Result<u8, Error> {
        let byte = *self
            .data
            .get(self.pos)
            .ok_or(Error::InvalidSmolv("truncated byte"))?;
        self.pos += 1;
        Ok(byte)
    }

    fn varint(&mut self) -> Result<u32, Error> {
        let mut value = 0u32;
        let mut shift = 0;
        loop {
            let byte = self.byte()?;
            if shift > 28 {
                return Err(Error::InvalidSmolv("varint longer than 5 bytes"));
            }
            value |= u32::from(byte & 127) << shift;
            shift += 7;
            if byte & 128 == 0 {
                return Ok(value);
            }
        }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.data.len()
    }
}

/// The size of the decoded SPIR-V module in bytes, from the header.
pub fn decoded_size(smolv: &[u8]) -> Result<usize, Error> {
    header(smolv).map(|h| h.decoded_size)
}

struct Header {
    version: u32,
    decoded_size: usize,
}

fn header(smolv: &[u8]) -> Result<Header, Error> {
    let mut r = Reader {
        data: smolv,
        pos: 0,
    };
    if r.word()? != SMOLV_MAGIC {
        return Err(Error::InvalidSmolv("wrong magic"));
    }
    let version_word = r.word()?;
    if !(0x0001_0000..=0x0001_0600).contains(&(version_word & 0x00FF_FFFF)) {
        return Err(Error::InvalidSmolv("unsupported SPIR-V version"));
    }
    let version = version_word >> 24;
    if version > CURRENT_VERSION {
        return Err(Error::InvalidSmolv("unsupported SMOL-V version"));
    }
    r.pos = 20;
    let decoded_size = r.word()? as usize;
    Ok(Header {
        version,
        decoded_size,
    })
}

/// Decode SMOL-V of any encoding version.
pub fn decode(smolv: &[u8]) -> Result<Vec<u32>, Error> {
    decode_impl(smolv, false)
}

/// Decode SMOL-V whose header says version 0 as the format of 2016-08-31, which Unity 2017 to
/// 2020 wrote without marking it differently.
// TODO: test with Vulkan programs from Unity 2017 to 2020
pub fn decode_2016_08_31(smolv: &[u8]) -> Result<Vec<u32>, Error> {
    decode_impl(smolv, true)
}

fn decode_impl(smolv: &[u8], version_zero_is_2016: bool) -> Result<Vec<u32>, Error> {
    let header = header(smolv)?;
    if header.decoded_size % 4 != 0 {
        return Err(Error::InvalidSmolv("decoded size isn't whole words"));
    }
    let mut out = Vec::with_capacity(header.decoded_size / 4);
    let mut r = Reader {
        data: smolv,
        pos: 4,
    };
    out.push(SPIRV_MAGIC);
    out.push(r.word()? & 0x00FF_FFFF);
    out.push(r.word()?);
    out.push(r.word()?);
    out.push(r.word()?);
    r.word()?;

    let before_zero = header.version == 0 && version_zero_is_2016;
    let known = known_ops(header.version);
    let mut prev_result = 0u32;
    let mut prev_decorate = 0u32;
    while !r.at_end() {
        let packed = r.varint()?;
        let len = (packed >> 20) << 4 | (packed >> 4) & 0xF;
        let op = remap_op((packed >> 4) & 0xFFF0 | packed & 0xF);
        let len = len + length_bias(op);
        let was_swizzle = op == OP_VECTOR_SHUFFLE_COMPACT;
        let op = if was_swizzle { OP_VECTOR_SHUFFLE } else { op };
        out.push(len << 16 | op);

        let data = op_data(op, known);
        let mut i = 1;
        if data.is_some_and(|d| d.has_type) {
            out.push(r.varint()?);
            i += 1;
        }
        if data.is_some_and(|d| d.has_result) {
            prev_result = prev_result.wrapping_add(unzig(r.varint()?));
            out.push(prev_result);
            i += 1;
        }
        if op == OP_DECORATE || op == OP_MEMBER_DECORATE {
            let delta = r.varint()?;
            prev_decorate =
                prev_decorate.wrapping_add(if before_zero { delta } else { unzig(delta) });
            out.push(prev_decorate);
            i += 1;
        }

        if op == OP_MEMBER_DECORATE && !before_zero {
            let count = r.byte()?;
            let (mut prev_index, mut prev_offset) = (0u32, 0u32);
            for m in 0..count {
                prev_index = prev_index.wrapping_add(r.varint()?);
                let decoration = r.varint()?;
                let len = match decoration_extra_words(decoration) {
                    Some(extra) => 4 + extra,
                    None => 4 + r.varint()?,
                };
                if m != 0 {
                    out.push(len << 16 | op);
                    out.push(prev_decorate);
                }
                out.push(prev_index);
                out.push(decoration);
                if decoration == DECORATION_OFFSET {
                    if len != 5 {
                        return Err(Error::InvalidSmolv(
                            "offset decoration of unexpected length",
                        ));
                    }
                    prev_offset = prev_offset.wrapping_add(r.varint()?);
                    out.push(prev_offset);
                } else {
                    for _ in 4..len {
                        out.push(r.varint()?);
                    }
                }
            }
            continue;
        }

        let zig_ids = !before_zero
            || matches!(
                op,
                OP_CONTROL_BARRIER
                    | OP_MEMORY_BARRIER
                    | OP_LOOP_MERGE
                    | OP_SELECTION_MERGE
                    | OP_BRANCH
                    | OP_BRANCH_CONDITIONAL
                    | OP_MEMORY_NAMED_BARRIER
            );
        let deltas = data.map_or(0, |d| d.delta_from_result as u32);
        for _ in 0..deltas {
            if i >= len {
                break;
            }
            let delta = r.varint()?;
            out.push(prev_result.wrapping_sub(if zig_ids { unzig(delta) } else { delta }));
            i += 1;
        }

        if was_swizzle && len <= 9 {
            let swizzle = u32::from(r.byte()?);
            for (component, shift) in [(5, 6), (6, 4), (7, 2), (8, 0)] {
                if len > component {
                    out.push((swizzle >> shift) & 3);
                }
            }
        } else if data.is_some_and(|d| d.var_rest) {
            for _ in i..len {
                out.push(r.varint()?);
            }
        } else {
            for _ in i..len {
                out.push(r.word()?);
            }
        }
    }
    if out.len() * 4 != header.decoded_size {
        return Err(Error::InvalidSmolv(
            "decoded size disagrees with the header",
        ));
    }
    Ok(out)
}
