//! The program code of Unity's Vulkan programs: one SMOL-V encoded SPIR-V module per
//! stage, decoded and [`VulkanProgram::parse`]. The converter's tests parse Unity's own
//! Vulkan programs with it to compare their translated stages against them; the Vulkan
//! target builds the same container around its translation (SMOL-V encoding:
//! `smolv::encode`).
//!
//! Layout:
//! - word 0: flags, see [`flags`]
//! - words 1..11: offset and size of the stage slots vertex, fragment, hull, domain, geometry
//! - the rest of the header, whose length the slot table gives away: the stages lie
//!   back to back after it, so the first stage's offset is where it ends. Three lengths
//!   occur in shipped games, see [`Form`]:
//!   - 176 bytes (Unity 2022.3, 6000): 9 words [`VulkanProgram::unknown`], a 16 byte
//!     hash per stage slot, 4 words [`VulkanProgram::trailer`]
//!   - 76 bytes (Unity 2020.2): 8 words [`VulkanProgram::unknown`], no hashes
//!   - 52 bytes (Unity 2019.4, A Short Hike 2019.4.29f1): 2 words, zero in every program
//!     seen, no hashes
//! - then the stages, fragment first, then vertex, then geometry

use anyhow::{Context, Result, bail, ensure};

use crate::{Reader, Writer};

/// Bits of [`VulkanProgram::flags`].
pub mod flags {
    /// The fragment stage contains `OpKill`.
    pub const DISCARD: u32 = 0x1;
    /// Seen in a pass whose fragment stage writes UAVs.
    // TODO: confirm with more data; seen in a single pass
    pub const UAV_SEEN: u32 = 0x8;
    /// Set in every program observed.
    pub const ALWAYS_SEEN: u32 = 0x20;
    /// No separate samplers: no inline or comparison samplers, and every texture is sampled
    /// through a combined image sampler.
    pub const COMBINED_SAMPLERS_ONLY: u32 = 0x40;
    /// Set in every program observed.
    pub const HIGH_SEEN: u32 = 0x0200_0000;
}

/// Stage slots, in the order of the slot table.
pub const STAGE_SLOTS: usize = 5;
pub const SLOT_VERTEX: usize = 0;
pub const SLOT_FRAGMENT: usize = 1;
pub const SLOT_GEOMETRY: usize = 4;

/// The order Unity lays out stage data in; slots 2 and 3 (presumably hull and domain) were never
/// observed, so their place is unknown.
// TODO: find where hull and domain stages go once data with them turns up
const DATA_ORDER: [usize; 3] = [SLOT_FRAGMENT, SLOT_VERTEX, SLOT_GEOMETRY];
/// The layout of a program's header, which the first stage's offset gives away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// 52 bytes: the slot table and two words (Unity 2019.4).
    Short,
    /// 76 bytes: the slot table and eight words (Unity 2020.2).
    Plain,
    /// 176 bytes: the slot table, nine words, a hash per stage slot and a trailer (Unity
    /// 2022.3, 6000).
    Hashed,
}

impl Form {
    fn of_header(size: usize) -> Result<Form> {
        Ok(match size {
            52 => Form::Short,
            76 => Form::Plain,
            176 => Form::Hashed,
            other => bail!("a header of {other} bytes, which was never observed"),
        })
    }

    fn header_size(self) -> usize {
        match self {
            Form::Short => 52,
            Form::Plain => 76,
            Form::Hashed => 176,
        }
    }

    /// The words after the slot table, before any hashes.
    fn unknown_words(self) -> usize {
        match self {
            Form::Short => 2,
            Form::Plain => 8,
            Form::Hashed => 9,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VulkanStage {
    /// A content hash of the stage, of unknown algorithm: not MD5, xxHash, MurmurHash3 or SpookyHash
    /// of the SPIR-V or SMOL-V.
    // TODO: find the algorithm, or whether Unity checks the hash at all
    pub hash: [u8; 16],
    pub smolv: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VulkanProgram {
    pub flags: u32,
    /// The header's layout; a program has to be written back in the form it came in.
    pub form: Form,
    /// The words after the slot table, as many as [`Form`] has: in the hashed form eight
    /// zeros and then 1, or 15 for `Sprites/Diffuse` and some `Standard` passes; meaning
    /// unknown.
    // TODO: find what the last word, 1 or 15, means
    pub unknown: [u32; 9],
    pub stages: [Option<VulkanStage>; STAGE_SLOTS],
    /// Zero in all observed programs.
    pub trailer: [u32; 4],
}

impl VulkanProgram {
    pub fn parse(code: &[u8]) -> Result<Self> {
        let mut r = Reader::new(code);
        let flags = r.u32()?;
        let slots: Vec<(usize, usize)> = (0..STAGE_SLOTS)
            .map(|_| Ok((r.u32()? as usize, r.u32()? as usize)))
            .collect::<Result<_>>()?;
        // the stages follow the header back to back, so the first one starts where it
        // ends: the form is in the data, not in a version
        let header_size = slots
            .iter()
            .filter(|(_, size)| *size != 0)
            .map(|(offset, _)| *offset)
            .min()
            .unwrap_or(Form::Hashed.header_size());
        let form = Form::of_header(header_size)?;

        let mut unknown = [0; 9];
        for word in &mut unknown[..form.unknown_words()] {
            *word = r.u32()?;
        }
        let mut hashes = [[0u8; 16]; STAGE_SLOTS];
        let mut trailer = [0; 4];
        if form == Form::Hashed {
            for hash in &mut hashes {
                hash.copy_from_slice(r.take(16)?);
            }
            for word in &mut trailer {
                *word = r.u32()?;
            }
        }

        let mut expected_offset = header_size;
        for (slot, &(offset, size)) in slots.iter().enumerate() {
            if size == 0 {
                ensure!(
                    offset == 0 && hashes[slot] == [0; 16],
                    "empty slot {slot} with data"
                );
            } else {
                ensure!(
                    DATA_ORDER.contains(&slot),
                    "stage slot {slot}, which was never observed"
                );
            }
        }
        for slot in DATA_ORDER {
            let (offset, size) = slots[slot];
            if size == 0 {
                continue;
            }
            ensure!(
                offset == expected_offset,
                "slot {slot} at {offset}, not right after the previous stage at {expected_offset}"
            );
            expected_offset += size;
        }
        ensure!(
            expected_offset == code.len(),
            "{} bytes after the last stage",
            code.len() as isize - expected_offset as isize
        );

        let stages = std::array::from_fn(|slot| {
            let (offset, size) = slots[slot];
            (size > 0).then(|| VulkanStage {
                hash: hashes[slot],
                smolv: code[offset..offset + size].to_vec(),
            })
        });
        Ok(VulkanProgram {
            flags,
            form,
            unknown,
            stages,
            trailer,
        })
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let header_size = self.form.header_size();
        let mut locations = [(0u32, 0u32); STAGE_SLOTS];
        let mut offset = header_size;
        for (slot, stage) in self.stages.iter().enumerate() {
            if stage.is_some() && !DATA_ORDER.contains(&slot) {
                bail!("stage slot {slot}, whose place Unity's layout is unknown for");
            }
        }
        for slot in DATA_ORDER {
            if let Some(stage) = &self.stages[slot] {
                let size = stage.smolv.len();
                locations[slot] = (
                    u32::try_from(offset)?,
                    u32::try_from(size).context("stage too large")?,
                );
                offset += size;
            }
        }

        let mut w = Writer::default();
        w.u32(self.flags);
        for (offset, size) in locations {
            w.u32(offset);
            w.u32(size);
        }
        for word in &self.unknown[..self.form.unknown_words()] {
            w.u32(*word);
        }
        if self.form == Form::Hashed {
            for stage in &self.stages {
                w.data
                    .extend_from_slice(&stage.as_ref().map_or([0; 16], |s| s.hash));
            }
            for word in self.trailer {
                w.u32(word);
            }
        }
        ensure!(
            w.data.len() == header_size,
            "header has {} bytes",
            w.data.len()
        );
        for slot in DATA_ORDER {
            if let Some(stage) = &self.stages[slot] {
                w.data.extend_from_slice(&stage.smolv);
            }
        }
        Ok(w.finish())
    }
}
