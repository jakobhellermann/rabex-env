//! Splitting combined image samplers into a texture and a sampler.
//!
//! Unity's shader compiler emits a texture and its sampler as one `OpTypeSampledImage`
//! variable when a shader samples with the texture's own sampler state; builds before
//! that declare the two separately and bind them separately. A player of the older form
//! binds nothing for a combined variable, so its sampling reads black.
//!
//! The rewrite is the one the two forms differ by, and nothing else: every combined
//! variable becomes an image variable plus a sampler variable, and every load of it
//! becomes two loads joined by `OpSampledImage`. The sampling instructions keep their
//! operands, because the id they consume still names a sampled image.
//!
//! The numbering leaves every other resource where it is: a module's set holds storage
//! buffers, images and plain textures too, and moving those would unbind them. The
//! texture half therefore keeps the combined variable's binding, and each sampler takes
//! the next one the set has free.

use std::collections::HashMap;

use ::spirv::{Decoration, Op, StorageClass};
use anyhow::{Result, bail, ensure};

/// Words of the module header before the first instruction.
const HEADER_WORDS: usize = 5;

/// One instruction: the opcode and its operands.
struct Instruction {
    op: u32,
    operands: Vec<u32>,
}

impl Instruction {
    fn new(op: Op, operands: Vec<u32>) -> Self {
        Instruction {
            op: op as u32,
            operands,
        }
    }

    fn words(&self) -> impl Iterator<Item = u32> + '_ {
        let head = ((self.operands.len() as u32 + 1) << 16) | self.op;
        std::iter::once(head).chain(self.operands.iter().copied())
    }
}

/// What one combined variable turns into.
struct Split {
    /// The `OpTypeSampledImage` the variable pointed at.
    sampled_image_type: u32,
    image_variable: u32,
    image_pointer_type: u32,
    image_type: u32,
    sampler_variable: u32,
    sampler_pointer_type: u32,
    sampler_type: u32,
}

/// Rewrite `words` so that no variable is a combined image sampler. Returns the
/// `(texture, sampler)` variable ids per original variable, in declaration order, so the
/// caller can describe them the way the target form's metadata does.
pub fn split_combined_samplers(words: &mut Vec<u32>) -> Result<Vec<(u32, u32)>> {
    ensure!(words.len() > HEADER_WORDS, "a module without a header");
    let mut instructions = parse(words)?;
    let mut bound = words[3];

    // what the module already declares, so the rewrite reuses instead of duplicating
    let mut sampled_image_types: HashMap<u32, u32> = HashMap::new(); // id -> image type
    let mut pointer_types: HashMap<(u32, u32), u32> = HashMap::new(); // (class, pointee) -> id
    let mut sampler_type: Option<u32> = None;
    for instruction in &instructions {
        let op = instruction.op;
        let operands = &instruction.operands;
        if op == Op::TypeSampledImage as u32 && operands.len() >= 2 {
            sampled_image_types.insert(operands[0], operands[1]);
        } else if op == Op::TypeSampler as u32 && !operands.is_empty() {
            sampler_type = Some(operands[0]);
        } else if op == Op::TypePointer as u32 && operands.len() >= 3 {
            pointer_types.insert((operands[1], operands[2]), operands[0]);
        }
    }

    // the variables that hold one
    let mut splits: Vec<(u32, Split)> = Vec::new();
    for instruction in &instructions {
        if instruction.op != Op::Variable as u32 || instruction.operands.len() < 3 {
            continue;
        }
        let (pointer_type, variable, storage) = (
            instruction.operands[0],
            instruction.operands[1],
            instruction.operands[2],
        );
        if storage != StorageClass::UniformConstant as u32 {
            continue;
        }
        let Some((_, pointee)) = pointer_types
            .iter()
            .find(|((_, _), id)| **id == pointer_type)
            .map(|((class, pointee), _)| (*class, *pointee))
        else {
            continue;
        };
        let Some(&image_type) = sampled_image_types.get(&pointee) else {
            continue;
        };
        splits.push((
            variable,
            Split {
                sampled_image_type: pointee,
                image_variable: 0,
                image_pointer_type: 0,
                image_type,
                sampler_variable: 0,
                sampler_pointer_type: 0,
                sampler_type: 0,
            },
        ));
    }
    if splits.is_empty() {
        return Ok(Vec::new());
    }

    // ids for what the module lacks
    let mut new_id = || {
        let id = bound;
        bound += 1;
        id
    };
    let sampler_type = match sampler_type {
        Some(id) => id,
        None => new_id(),
    };
    let sampler_pointer =
        match pointer_types.get(&(StorageClass::UniformConstant as u32, sampler_type)) {
            Some(&id) => id,
            None => new_id(),
        };
    for (_, split) in &mut splits {
        split.sampler_type = sampler_type;
        split.sampler_pointer_type = sampler_pointer;
        split.image_pointer_type =
            match pointer_types.get(&(StorageClass::UniformConstant as u32, split.image_type)) {
                Some(&id) => id,
                None => new_id(),
            };
        split.image_variable = new_id();
        split.sampler_variable = new_id();
    }

    // the binding the combined variable had, to keep the pairs in their order
    let mut original: HashMap<u32, (u32, u32)> = HashMap::new();
    for instruction in &instructions {
        if instruction.op != Op::Decorate as u32 || instruction.operands.len() < 3 {
            continue;
        }
        let (target, decoration, value) = (
            instruction.operands[0],
            instruction.operands[1],
            instruction.operands[2],
        );
        let entry = original.entry(target).or_insert((0, 0));
        if decoration == Decoration::DescriptorSet as u32 {
            entry.0 = value;
        } else if decoration == Decoration::Binding as u32 {
            entry.1 = value;
        }
    }
    splits.sort_by_key(|(variable, _)| original.get(variable).copied().unwrap_or((0, 0)));
    // every binding the set already hands out, whatever holds it
    let mut taken: Vec<u32> = original
        .iter()
        .filter(|(_, (set, _))| {
            splits
                .first()
                .is_none_or(|(first, _)| original.get(first).map(|(s, _)| *s) == Some(*set))
        })
        .map(|(_, (_, binding))| *binding)
        .collect();
    let mut bindings: HashMap<u32, (u32, u32)> = HashMap::new();
    for (variable, _) in &splits {
        let texture = original.get(variable).map(|(_, b)| *b).unwrap_or(0);
        let mut sampler = 0;
        while taken.contains(&sampler) {
            sampler += 1;
        }
        taken.push(sampler);
        bindings.insert(*variable, (sampler, texture));
    }

    rewrite(
        &mut instructions,
        &splits,
        sampler_type,
        sampler_pointer,
        &bindings,
        &mut new_id,
    )?;

    let mut out = words[..HEADER_WORDS].to_vec();
    out[3] = bound;
    for instruction in &instructions {
        out.extend(instruction.words());
    }
    *words = out;
    Ok(splits
        .iter()
        .map(|(_, split)| (split.image_variable, split.sampler_variable))
        .collect())
}

/// Parse the module into instructions, keeping their order.
fn parse(words: &[u32]) -> Result<Vec<Instruction>> {
    let mut instructions = Vec::new();
    let mut pos = HEADER_WORDS;
    while pos < words.len() {
        let head = words[pos];
        let len = (head >> 16) as usize;
        ensure!(len > 0, "zero length instruction at word {pos}");
        let Some(operands) = words.get(pos + 1..pos + len) else {
            bail!("instruction past the end at word {pos}");
        };
        instructions.push(Instruction {
            op: head & 0xffff,
            operands: operands.to_vec(),
        });
        pos += len;
    }
    Ok(instructions)
}

/// Replace the combined declarations and their uses.
fn rewrite(
    instructions: &mut Vec<Instruction>,
    splits: &[(u32, Split)],
    sampler_type: u32,
    sampler_pointer: u32,
    bindings: &HashMap<u32, (u32, u32)>,
    new_id: &mut dyn FnMut() -> u32,
) -> Result<()> {
    let by_variable: HashMap<u32, &Split> = splits.iter().map(|(id, split)| (*id, split)).collect();

    // the declarations the new variables need, next to the ones they replace
    let mut declared_sampler_type = instructions
        .iter()
        .any(|i| i.op == Op::TypeSampler as u32 && i.operands.first() == Some(&sampler_type));
    let mut declared_sampler_pointer = instructions
        .iter()
        .any(|i| i.op == Op::TypePointer as u32 && i.operands.first() == Some(&sampler_pointer));
    let mut declared_image_pointers: Vec<u32> = instructions
        .iter()
        .filter(|i| i.op == Op::TypePointer as u32)
        .filter_map(|i| i.operands.first().copied())
        .collect();

    let mut out: Vec<Instruction> = Vec::with_capacity(instructions.len() + splits.len() * 4);
    for instruction in instructions.drain(..) {
        // a decoration of a combined variable applies to both halves
        if instruction.op == Op::Decorate as u32
            && let Some(split) = instruction
                .operands
                .first()
                .and_then(|id| by_variable.get(id))
            && matches!(
                instruction.operands.get(1).copied(),
                Some(d) if d == Decoration::DescriptorSet as u32 || d == Decoration::Binding as u32
            )
        {
            let variable = instruction.operands[0];
            let mut image = instruction.operands.clone();
            image[0] = split.image_variable;
            let mut sampler = instruction.operands.clone();
            sampler[0] = split.sampler_variable;
            if instruction.operands[1] == Decoration::Binding as u32
                && let Some((sampler_binding, image_binding)) = bindings.get(&variable)
            {
                image[2] = *image_binding;
                sampler[2] = *sampler_binding;
            }
            out.push(Instruction::new(Op::Decorate, image));
            out.push(Instruction::new(Op::Decorate, sampler));
            continue;
        }
        if instruction.op == Op::Name as u32
            && let Some(split) = instruction
                .operands
                .first()
                .and_then(|id| by_variable.get(id))
        {
            let mut image = instruction.operands.clone();
            image[0] = split.image_variable;
            let mut sampler = instruction.operands.clone();
            sampler[0] = split.sampler_variable;
            out.push(Instruction::new(Op::Name, image));
            out.push(Instruction::new(Op::Name, sampler));
            continue;
        }

        // the declaration of a combined variable becomes the two declarations
        if instruction.op == Op::Variable as u32
            && let Some(variable) = instruction.operands.get(1)
            && let Some(split) = by_variable.get(variable)
        {
            if !declared_sampler_type {
                out.push(Instruction::new(Op::TypeSampler, vec![sampler_type]));
                declared_sampler_type = true;
            }
            if !declared_sampler_pointer {
                out.push(Instruction::new(
                    Op::TypePointer,
                    vec![
                        sampler_pointer,
                        StorageClass::UniformConstant as u32,
                        sampler_type,
                    ],
                ));
                declared_sampler_pointer = true;
            }
            if !declared_image_pointers.contains(&split.image_pointer_type) {
                out.push(Instruction::new(
                    Op::TypePointer,
                    vec![
                        split.image_pointer_type,
                        StorageClass::UniformConstant as u32,
                        split.image_type,
                    ],
                ));
                declared_image_pointers.push(split.image_pointer_type);
            }
            out.push(Instruction::new(
                Op::Variable,
                vec![
                    split.image_pointer_type,
                    split.image_variable,
                    StorageClass::UniformConstant as u32,
                ],
            ));
            out.push(Instruction::new(
                Op::Variable,
                vec![
                    split.sampler_pointer_type,
                    split.sampler_variable,
                    StorageClass::UniformConstant as u32,
                ],
            ));
            continue;
        }

        // loading a combined variable becomes loading both halves and joining them,
        // under the id the load produced: everything consuming it stays as it is
        if instruction.op == Op::Load as u32
            && instruction.operands.len() >= 3
            && let Some(split) = by_variable.get(&instruction.operands[2]).copied()
        {
            let result = instruction.operands[1];
            let (image, sampler) = (new_id(), new_id());
            out.push(Instruction::new(
                Op::Load,
                vec![split.image_type, image, split.image_variable],
            ));
            out.push(Instruction::new(
                Op::Load,
                vec![split.sampler_type, sampler, split.sampler_variable],
            ));
            out.push(Instruction::new(
                Op::SampledImage,
                vec![split.sampled_image_type, result, image, sampler],
            ));
            continue;
        }

        out.push(instruction);
    }
    *instructions = out;
    Ok(())
}
