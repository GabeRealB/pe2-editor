//! What the contract calls features, against wgpu's: as wasi-gfx's host maps them (Apache-2.0 WITH LLVM-exception).

use crate::wasi::webgpu::webgpu::GpuFeatureName;

const FEATURES: &[(&str, GpuFeatureName, wgpu::FeaturesWebGPU)] = &[
    ("core-features-and-limits", GpuFeatureName::CoreFeaturesAndLimits, wgpu::FeaturesWebGPU::empty()),
    ("depth-clip-control", GpuFeatureName::DepthClipControl, wgpu::FeaturesWebGPU::DEPTH_CLIP_CONTROL),
    ("depth32float-stencil8", GpuFeatureName::Depth32floatStencil8, wgpu::FeaturesWebGPU::DEPTH32FLOAT_STENCIL8),
    ("texture-compression-bc", GpuFeatureName::TextureCompressionBc, wgpu::FeaturesWebGPU::TEXTURE_COMPRESSION_BC),
    (
        "texture-compression-bc-sliced3d",
        GpuFeatureName::TextureCompressionBcSliced3d,
        wgpu::FeaturesWebGPU::TEXTURE_COMPRESSION_BC_SLICED_3D,
    ),
    (
        "texture-compression-etc2",
        GpuFeatureName::TextureCompressionEtc2,
        wgpu::FeaturesWebGPU::TEXTURE_COMPRESSION_ETC2,
    ),
    (
        "texture-compression-astc",
        GpuFeatureName::TextureCompressionAstc,
        wgpu::FeaturesWebGPU::TEXTURE_COMPRESSION_ASTC,
    ),
    (
        "texture-compression-astc-sliced3d",
        GpuFeatureName::TextureCompressionAstcSliced3d,
        wgpu::FeaturesWebGPU::empty(),
    ),
    ("timestamp-query", GpuFeatureName::TimestampQuery, wgpu::FeaturesWebGPU::TIMESTAMP_QUERY),
    ("indirect-first-instance", GpuFeatureName::IndirectFirstInstance, wgpu::FeaturesWebGPU::INDIRECT_FIRST_INSTANCE),
    ("shader-f16", GpuFeatureName::ShaderF16, wgpu::FeaturesWebGPU::SHADER_F16),
    (
        "rg11b10ufloat-renderable",
        GpuFeatureName::Rg11b10ufloatRenderable,
        wgpu::FeaturesWebGPU::RG11B10UFLOAT_RENDERABLE,
    ),
    ("bgra8unorm-storage", GpuFeatureName::Bgra8unormStorage, wgpu::FeaturesWebGPU::BGRA8UNORM_STORAGE),
    ("float32-filterable", GpuFeatureName::Float32Filterable, wgpu::FeaturesWebGPU::FLOAT32_FILTERABLE),
    ("float32-blendable", GpuFeatureName::Float32Blendable, wgpu::FeaturesWebGPU::FLOAT32_BLENDABLE),
    ("clip-distances", GpuFeatureName::ClipDistances, wgpu::FeaturesWebGPU::CLIP_DISTANCES),
    ("dual-source-blending", GpuFeatureName::DualSourceBlending, wgpu::FeaturesWebGPU::DUAL_SOURCE_BLENDING),
    ("subgroups", GpuFeatureName::Subgroups, wgpu::FeaturesWebGPU::empty()),
    ("texture-formats-tier1", GpuFeatureName::TextureFormatsTier1, wgpu::FeaturesWebGPU::empty()),
    ("texture-formats-tier2", GpuFeatureName::TextureFormatsTier2, wgpu::FeaturesWebGPU::empty()),
    ("primitive-index", GpuFeatureName::PrimitiveIndex, wgpu::FeaturesWebGPU::PRIMITIVE_INDEX),
    ("texture-component-swizzle", GpuFeatureName::TextureComponentSwizzle, wgpu::FeaturesWebGPU::empty()),
];

/// A feature by the name the contract gives it.
#[must_use]
pub fn named(name: &str) -> Option<wgpu::FeaturesWebGPU> {
    FEATURES.iter().find(|(known, ..)| *known == name).map(|(.., feature)| *feature)
}

/// What a list of them comes to.
#[must_use]
pub fn of(names: &[GpuFeatureName]) -> wgpu::Features {
    let wanted = |name: &&(&str, GpuFeatureName, wgpu::FeaturesWebGPU)| names.contains(&name.1);
    let features =
        FEATURES.iter().filter(wanted).fold(wgpu::FeaturesWebGPU::empty(), |all, (.., feature)| all | *feature);
    whole(features)
}

/// One of the contract's features, as wgpu says what a device has.
#[must_use]
pub fn whole(features_webgpu: wgpu::FeaturesWebGPU) -> wgpu::Features {
    wgpu::Features { features_wgpu: wgpu::FeaturesWGPU::empty(), features_webgpu }
}
