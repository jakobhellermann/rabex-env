//! The descriptor resources of a SPIR-V module and the layouts of its uniform blocks.

use std::collections::{BTreeSet, HashMap};

use ::spirv::{Decoration, Op, StorageClass};
use anyhow::{Context, Result, anyhow, bail, ensure};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scalar {
    Float,
    Int,
    Uint,
    Bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Type {
    Scalar(Scalar),
    Vector(Scalar, u32),
    /// Column-vector matrix: `columns` columns of `rows` components.
    Matrix {
        scalar: Scalar,
        columns: u32,
        rows: u32,
        stride: u32,
    },
    Array {
        element: Box<Type>,
        length: u32,
        stride: u32,
    },
    Struct(Vec<Member>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub offset: u32,
    pub ty: Type,
}

/// `Dim` of an image type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dim {
    D1,
    D2,
    D3,
    Cube,
    Buffer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Image {
    pub dim: Dim,
    pub arrayed: bool,
    pub multisampled: bool,
    pub depth: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceKind {
    UniformBuffer {
        size: u32,
        members: Vec<Member>,
        /// Names of the members the code accesses.
        accessed: BTreeSet<String>,
    },
    StorageBuffer {
        writable: bool,
    },
    Texture {
        image: Image,
        combined: bool,
    },
    StorageImage(Image),
    Sampler,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    /// The variable's result id.
    pub id: u32,
    /// The variable's name, or its block type's for nameless blocks.
    pub name: String,
    pub set: u32,
    pub binding: u32,
    pub kind: ResourceKind,
}

/// One target's decorations: opcode and operands, without the target id.
type Decorations = Vec<(u32, Vec<u32>)>;

#[derive(Default)]
struct Module {
    names: HashMap<u32, String>,
    member_names: HashMap<(u32, u32), String>,
    decorations: HashMap<u32, Decorations>,
    member_decorations: HashMap<(u32, u32), Decorations>,
    /// Type declarations by result id: opcode and operands after the result id.
    types: HashMap<u32, (Op, Vec<u32>)>,
    /// Value of constants, and the default of specialization constants, by result id.
    constants: HashMap<u32, u32>,
    /// `(result id, type)` of `OpSpecConstant`s.
    spec_constants: Vec<(u32, u32)>,
    /// `(result id, pointer type, storage class)`
    variables: Vec<(u32, u32, StorageClass)>,
    /// `(base, first index)` of access chains whose first index is a constant.
    access_chains: Vec<(u32, u32)>,
}

/// Words of the module header before the first instruction.
const HEADER_WORDS: usize = 5;
/// `Sampled` operand of `OpTypeImage` for images used without a sampler.
const IMAGE_SAMPLED_STORAGE: u32 = 2;

fn instructions(words: &[u32]) -> impl Iterator<Item = Result<(u32, &[u32])>> {
    let mut pos = HEADER_WORDS;
    std::iter::from_fn(move || {
        let &head = words.get(pos)?;
        let len = (head >> 16) as usize;
        if len == 0 {
            pos = words.len();
            return Some(Err(anyhow!("zero length instruction at word {pos}")));
        }
        let Some(operands) = words.get(pos + 1..pos + len) else {
            pos = words.len();
            return Some(Err(anyhow!("instruction past the end")));
        };
        pos += len;
        Some(Ok((head & 0xffff, operands)))
    })
}

impl Module {
    fn parse(words: &[u32]) -> Result<Self> {
        ensure!(words.first() == Some(&::spirv::MAGIC_NUMBER), "not SPIR-V");
        let mut module = Module::default();
        for instruction in instructions(words) {
            let (op, operands) = instruction?;
            let Some(op) = Op::from_u32(op) else {
                continue;
            };
            match op {
                Op::Name => {
                    module.names.insert(operands[0], string(&operands[1..]));
                }
                Op::MemberName => {
                    module
                        .member_names
                        .insert((operands[0], operands[1]), string(&operands[2..]));
                }
                Op::Decorate => module
                    .decorations
                    .entry(operands[0])
                    .or_default()
                    .push((operands[1], operands[2..].to_vec())),
                Op::MemberDecorate => module
                    .member_decorations
                    .entry((operands[0], operands[1]))
                    .or_default()
                    .push((operands[2], operands[3..].to_vec())),
                Op::TypeVoid
                | Op::TypeBool
                | Op::TypeInt
                | Op::TypeFloat
                | Op::TypeVector
                | Op::TypeMatrix
                | Op::TypeImage
                | Op::TypeSampler
                | Op::TypeSampledImage
                | Op::TypeArray
                | Op::TypeRuntimeArray
                | Op::TypeStruct
                | Op::TypeOpaque
                | Op::TypePointer => {
                    module
                        .types
                        .insert(operands[0], (op, operands[1..].to_vec()));
                }
                Op::Constant => {
                    module.constants.insert(operands[1], operands[2]);
                }
                Op::SpecConstant => {
                    module.constants.insert(operands[1], operands[2]);
                    module.spec_constants.push((operands[1], operands[0]));
                }
                Op::AccessChain | Op::InBoundsAccessChain if operands.len() > 3 => {
                    module.access_chains.push((operands[2], operands[3]));
                }
                Op::Variable => module.variables.push((
                    operands[1],
                    operands[0],
                    StorageClass::from_u32(operands[2])
                        .with_context(|| format!("unknown storage class {}", operands[2]))?,
                )),
                _ => {}
            }
        }
        Ok(module)
    }

    fn decoration(&self, id: u32, decoration: Decoration) -> Option<&[u32]> {
        self.decorations
            .get(&id)?
            .iter()
            .find(|(d, _)| *d == decoration as u32)
            .map(|(_, v)| v.as_slice())
    }

    fn member_decoration(&self, id: u32, member: u32, decoration: Decoration) -> Option<&[u32]> {
        self.member_decorations
            .get(&(id, member))?
            .iter()
            .find(|(d, _)| *d == decoration as u32)
            .map(|(_, v)| v.as_slice())
    }

    fn ty(&self, id: u32) -> Result<&(Op, Vec<u32>)> {
        self.types
            .get(&id)
            .with_context(|| format!("no type %{id}"))
    }

    fn scalar(&self, id: u32) -> Result<Scalar> {
        Ok(match self.ty(id)? {
            (Op::TypeBool, _) => Scalar::Bool,
            (Op::TypeInt, operands) if operands[0] == 32 => {
                if operands[1] == 1 {
                    Scalar::Int
                } else {
                    Scalar::Uint
                }
            }
            (Op::TypeFloat, operands) if operands[0] == 32 => Scalar::Float,
            other => bail!("%{id} isn't a 32 bit scalar: {other:?}"),
        })
    }

    /// The type of a block member; matrices need their stride from the member decoration.
    fn member_type(&self, id: u32, matrix_stride: Option<u32>) -> Result<Type> {
        let (op, operands) = self.ty(id)?;
        Ok(match op {
            Op::TypeBool | Op::TypeInt | Op::TypeFloat => Type::Scalar(self.scalar(id)?),
            Op::TypeVector => Type::Vector(self.scalar(operands[0])?, operands[1]),
            Op::TypeMatrix => {
                let (_, column) = self.ty(operands[0])?;
                Type::Matrix {
                    scalar: self.scalar(column[0])?,
                    columns: operands[1],
                    rows: column[1],
                    stride: matrix_stride.context("matrix without stride")?,
                }
            }
            Op::TypeArray => Type::Array {
                element: Box::new(self.member_type(operands[0], matrix_stride)?),
                length: *self
                    .constants
                    .get(&operands[1])
                    .context("array length isn't a constant")?,
                stride: self
                    .decoration(id, Decoration::ArrayStride)
                    .context("array without stride")?[0],
            },
            Op::TypeStruct => Type::Struct(self.members(id)?),
            other => bail!("unsupported member type {other:?}"),
        })
    }

    fn members(&self, struct_id: u32) -> Result<Vec<Member>> {
        let (_, member_types) = self.ty(struct_id)?;
        member_types
            .iter()
            .enumerate()
            .map(|(i, &ty)| {
                let i = i as u32;
                Ok(Member {
                    name: self
                        .member_names
                        .get(&(struct_id, i))
                        .cloned()
                        .unwrap_or_default(),
                    offset: self
                        .member_decoration(struct_id, i, Decoration::Offset)
                        .context("member without offset")?[0],
                    ty: self.member_type(
                        ty,
                        self.member_decoration(struct_id, i, Decoration::MatrixStride)
                            .map(|s| s[0]),
                    )?,
                })
            })
            .collect()
    }

    fn image(&self, id: u32) -> Result<Image> {
        let (op, operands) = self.ty(id)?;
        ensure!(*op == Op::TypeImage, "%{id} isn't an image");
        Ok(Image {
            dim: match ::spirv::Dim::from_u32(operands[1]) {
                Some(::spirv::Dim::Dim1D) => Dim::D1,
                Some(::spirv::Dim::Dim2D) => Dim::D2,
                Some(::spirv::Dim::Dim3D) => Dim::D3,
                Some(::spirv::Dim::DimCube) => Dim::Cube,
                Some(::spirv::Dim::DimBuffer) => Dim::Buffer,
                other => bail!("unsupported image dimension {} ({other:?})", operands[1]),
            },
            depth: operands[2] == 1,
            arrayed: operands[3] == 1,
            multisampled: operands[4] == 1,
        })
    }
}

fn string(words: &[u32]) -> String {
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Byte size of a member type, as laid out by its offsets and strides.
pub fn size(ty: &Type) -> u32 {
    match ty {
        Type::Scalar(_) => 4,
        Type::Vector(_, n) => 4 * n,
        Type::Matrix {
            columns,
            rows,
            stride,
            ..
        } => stride * (columns - 1) + 4 * rows,
        // std140 pads every struct element to the array stride, the last one too
        Type::Array {
            element,
            length,
            stride,
        } if matches!(**element, Type::Struct(_)) => stride * length,
        Type::Array {
            element,
            length,
            stride,
        } => stride * (length - 1) + size(element),
        Type::Struct(members) => members
            .iter()
            .map(|m| m.offset + size(&m.ty))
            .max()
            .unwrap_or(0),
    }
}

/// Every descriptor resource of a SPIR-V module.
pub fn resources(words: &[u32]) -> Result<Vec<Resource>> {
    let module = Module::parse(words)?;
    let mut resources = Vec::new();
    for &(id, pointer, storage) in &module.variables {
        let (Some(set), Some(binding)) = (
            module.decoration(id, Decoration::DescriptorSet),
            module.decoration(id, Decoration::Binding),
        ) else {
            continue;
        };
        let (_, pointer_operands) = module.ty(pointer)?;
        let pointee = pointer_operands[1];
        let (op, operands) = module.ty(pointee)?;
        let name = match module.names.get(&id) {
            Some(name) if !name.is_empty() => name.clone(),
            _ => module.names.get(&pointee).cloned().unwrap_or_default(),
        };
        let kind = match (storage, *op) {
            (StorageClass::Uniform, Op::TypeStruct)
                if module.decoration(pointee, Decoration::Block).is_some() =>
            {
                let members = module.members(pointee)?;
                let accessed = module
                    .access_chains
                    .iter()
                    .filter(|(base, _)| *base == id)
                    .filter_map(|(_, index)| module.constants.get(index))
                    .filter_map(|&index| members.get(index as usize))
                    .map(|m| m.name.clone())
                    .collect();
                ResourceKind::UniformBuffer {
                    size: size(&Type::Struct(members.clone())),
                    members,
                    accessed,
                }
            }
            (StorageClass::Uniform, Op::TypeStruct)
                if module
                    .decoration(pointee, Decoration::BufferBlock)
                    .is_some() =>
            {
                ResourceKind::StorageBuffer {
                    writable: buffer_writable(&module, pointee)?,
                }
            }
            (StorageClass::StorageBuffer, Op::TypeStruct) => ResourceKind::StorageBuffer {
                writable: buffer_writable(&module, pointee)?,
            },
            (StorageClass::UniformConstant, Op::TypeImage)
                if operands[5] == IMAGE_SAMPLED_STORAGE =>
            {
                ResourceKind::StorageImage(module.image(pointee)?)
            }
            (StorageClass::UniformConstant, Op::TypeImage) => ResourceKind::Texture {
                image: module.image(pointee)?,
                combined: false,
            },
            (StorageClass::UniformConstant, Op::TypeSampledImage) => ResourceKind::Texture {
                image: module.image(operands[0])?,
                combined: true,
            },
            (StorageClass::UniformConstant, Op::TypeSampler) => ResourceKind::Sampler,
            other => {
                bail!("{name}: unsupported resource of storage class and type {other:?}")
            }
        };
        resources.push(Resource {
            id,
            name,
            set: set[0],
            binding: binding[0],
            kind,
        });
    }
    Ok(resources)
}

fn buffer_writable(module: &Module, struct_id: u32) -> Result<bool> {
    let (_, members) = module.ty(struct_id)?;
    Ok((0..members.len() as u32).any(|i| {
        module
            .member_decoration(struct_id, i, Decoration::NonWritable)
            .is_none()
    }))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpecConstant {
    pub name: String,
    pub spec_id: u32,
    pub scalar: Scalar,
    pub default: u32,
}

/// The scalar specialization constants of a SPIR-V module.
pub fn spec_constants(words: &[u32]) -> Result<Vec<SpecConstant>> {
    let module = Module::parse(words)?;
    module
        .spec_constants
        .iter()
        .map(|&(id, ty)| {
            Ok(SpecConstant {
                name: module.names.get(&id).cloned().unwrap_or_default(),
                spec_id: module
                    .decoration(id, Decoration::SpecId)
                    .context("spec constant without SpecId")?[0],
                scalar: module.scalar(ty)?,
                default: module.constants[&id],
            })
        })
        .collect()
}

/// Change the `Binding` decorations of variables, by result id.
pub fn rebind(words: &mut [u32], bindings: &HashMap<u32, u32>) -> Result<()> {
    let mut changed = 0;
    let mut pos = HEADER_WORDS;
    while pos < words.len() {
        let len = (words[pos] >> 16) as usize;
        ensure!(len > 0, "zero length instruction at word {pos}");
        let is_binding = words[pos] & 0xffff == Op::Decorate as u32
            && len >= 4
            && words[pos + 2] == Decoration::Binding as u32;
        if is_binding && let Some(&binding) = bindings.get(&words[pos + 1]) {
            words[pos + 3] = binding;
            changed += 1;
        }
        pos += len;
    }
    ensure!(
        changed == bindings.len(),
        "{changed} of {} variables have a binding",
        bindings.len()
    );
    Ok(())
}

/// Whether a SPIR-V module contains `OpKill`, which `discard` and `clip` compile to.
pub fn has_kill(words: &[u32]) -> Result<bool> {
    for instruction in instructions(words) {
        if instruction?.0 == Op::Kill as u32 {
            return Ok(true);
        }
    }
    Ok(false)
}
