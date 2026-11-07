//! The D3D11 kernels of compute shaders, described like a pass variant's parameters:
//! the compute converter translates these kernels and derives the Vulkan kernel
//! layouts from them.

use anyhow::Result;

use crate::compute_types::{ComputeShaderCB, ComputeShaderKernel};
use crate::{Binding, Constant, ConstantBufferLayout, ProgramParameters};

/// The parameters of a kernel variant, its constant buffers' layouts taken from `layouts`, the
/// platform variant's `constantBuffers`.
pub fn parameters(
    layouts: &[ComputeShaderCB],
    kernel: &ComputeShaderKernel,
) -> Result<ProgramParameters> {
    anyhow::ensure!(
        kernel.cbs.len() == kernel.cbVariantIndices.len(),
        "{} constant buffers with {} layouts",
        kernel.cbs.len(),
        kernel.cbVariantIndices.len()
    );
    let mut parameters = ProgramParameters {
        constant_buffers: vec![ConstantBufferLayout {
            name: String::new(),
            size: 0,
            constants: Vec::new(),
            structs: Vec::new(),
        }],
        bindings: Vec::new(),
    };
    for (cb, &layout) in kernel.cbs.iter().zip(&kernel.cbVariantIndices) {
        let layout = layouts
            .get(layout as usize)
            .ok_or_else(|| anyhow::anyhow!("{}: layout {layout} out of range", cb.name))?;
        parameters.constant_buffers.push(ConstantBufferLayout {
            name: cb.name.clone(),
            size: u32::try_from(layout.byteSize)?,
            constants: layout
                .params
                .iter()
                .map(|p| {
                    Ok(Constant {
                        name: p.name.clone(),
                        data_type: u32::try_from(p.r#type)?,
                        rows: p.rowCount,
                        columns: Some(p.colCount),
                        is_matrix: p.rowCount > 1,
                        array_size: p.arraySize,
                        offset: p.offset,
                    })
                })
                .collect::<Result<_>>()?,
            structs: Vec::new(),
        });
        parameters.bindings.push(Binding::ConstantBuffer {
            name: cb.name.clone(),
            index: cb.bindPoint,
            array_size: 0,
        });
    }
    for texture in &kernel.textures {
        parameters.bindings.push(Binding::Texture {
            name: texture.name.clone(),
            index: texture.bindPoint,
            sampler_index: texture.samplerBindPoint,
            dimension: u32::try_from(texture.texDimension)?,
            // TODO: multisampled textures in compute shaders, once data with them turns up
            multisampled: false,
        });
    }
    for buffer in &kernel.inBuffers {
        parameters.bindings.push(Binding::Buffer {
            name: buffer.name.clone(),
            index: buffer.bindPoint,
            array_size: 0,
        });
    }
    for uav in &kernel.outBuffers {
        parameters.bindings.push(Binding::Uav {
            name: uav.name.clone(),
            index: uav.bindPoint,
            original_index: u32::try_from(uav.bindPoint)?,
        });
    }
    for sampler in &kernel.builtinSamplers {
        parameters.bindings.push(Binding::Sampler {
            name: String::new(),
            index: sampler.bindPoint,
            state: sampler.sampler,
        });
    }
    Ok(parameters)
}
