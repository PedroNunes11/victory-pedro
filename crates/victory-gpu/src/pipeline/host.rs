//! Host pipelines for title pipelines (decision 0007, step 3): the
//! title's graphics state with the translated shaders, under a root
//! signature of our own that the translated shaders' bindings define
//! (`victory_shader::emit`): per stage one block of root constants for the
//! user data, root views for the title's root views, static samplers for
//! the immediate samplers, and directly indexed heaps.

use std::ffi::{CStr, CString};
use std::sync::Arc;

use victory_shader::emit::{ROOT_CBV_SPACE, SAMPLER_SPACE, USER_DATA_SPACE};
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::PCSTR;

use super::{Shader, Translated};

/// The most lanes a host wave has (`WaveLaneCountMax`; 0 until the device
/// is made, then nothing is refused for it).
pub(crate) static HOST_MAX_WAVE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The wave size a translated program requires (its `PSV0` runtime info's
/// minimum wave lanes), if it requires one.
pub(crate) fn wave_lanes_needed(dxil: &[u8]) -> Option<u32> {
    let c = victory_shader::Container::parse(dxil).ok()?;
    let psv = c.part(victory_shader::FourCC::PSV0)?.data;
    let info = u32::from_le_bytes(psv.get(0..4)?.try_into().ok()?) as usize;
    if info < 24 {
        return None;
    }
    let min = u32::from_le_bytes(psv.get(4 + 16..4 + 20)?.try_into().ok()?);
    (min != 0).then_some(min)
}
use crate::host::host;

/// A stream's graphics state, owned (the title's stream lives only for the
/// creation call). Subobjects the stream leaves out keep D3D12's defaults.
pub(crate) struct GraphicsState {
    blend: D3D12_BLEND_DESC,
    sample_mask: u32,
    rasterizer: D3D12_RASTERIZER_DESC,
    depth: D3D12_DEPTH_STENCIL_DESC,
    depth_bounds: bool,
    /// Input elements, their names pointing into `names`.
    inputs: Vec<D3D12_INPUT_ELEMENT_DESC>,
    names: Vec<CString>,
    strip_cut: D3D12_INDEX_BUFFER_STRIP_CUT_VALUE,
    pub(crate) topology: D3D12_PRIMITIVE_TOPOLOGY_TYPE,
    pub(crate) targets: D3D12_RT_FORMAT_ARRAY,
    depth_format: DXGI_FORMAT,
    samples: DXGI_SAMPLE_DESC,
}

impl Default for GraphicsState {
    fn default() -> Self {
        let target = D3D12_RENDER_TARGET_BLEND_DESC {
            SrcBlend: D3D12_BLEND_ONE,
            DestBlend: D3D12_BLEND_ZERO,
            BlendOp: D3D12_BLEND_OP_ADD,
            SrcBlendAlpha: D3D12_BLEND_ONE,
            DestBlendAlpha: D3D12_BLEND_ZERO,
            BlendOpAlpha: D3D12_BLEND_OP_ADD,
            LogicOp: D3D12_LOGIC_OP_NOOP,
            RenderTargetWriteMask: D3D12_COLOR_WRITE_ENABLE_ALL.0 as u8,
            ..Default::default()
        };
        let keep = D3D12_DEPTH_STENCILOP_DESC {
            StencilFailOp: D3D12_STENCIL_OP_KEEP,
            StencilDepthFailOp: D3D12_STENCIL_OP_KEEP,
            StencilPassOp: D3D12_STENCIL_OP_KEEP,
            StencilFunc: D3D12_COMPARISON_FUNC_ALWAYS,
        };
        GraphicsState {
            blend: D3D12_BLEND_DESC {
                RenderTarget: [target; 8],
                ..Default::default()
            },
            sample_mask: u32::MAX,
            rasterizer: D3D12_RASTERIZER_DESC {
                FillMode: D3D12_FILL_MODE_SOLID,
                CullMode: D3D12_CULL_MODE_BACK,
                DepthClipEnable: true.into(),
                ..Default::default()
            },
            depth: D3D12_DEPTH_STENCIL_DESC {
                DepthEnable: true.into(),
                DepthWriteMask: D3D12_DEPTH_WRITE_MASK_ALL,
                DepthFunc: D3D12_COMPARISON_FUNC_LESS,
                StencilReadMask: 0xff,
                StencilWriteMask: 0xff,
                FrontFace: keep,
                BackFace: keep,
                ..Default::default()
            },
            depth_bounds: false,
            inputs: Vec::new(),
            names: Vec::new(),
            strip_cut: D3D12_INDEX_BUFFER_STRIP_CUT_VALUE_DISABLED,
            topology: D3D12_PRIMITIVE_TOPOLOGY_TYPE_UNDEFINED,
            targets: D3D12_RT_FORMAT_ARRAY::default(),
            depth_format: DXGI_FORMAT_UNKNOWN,
            samples: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
        }
    }
}

/// Integer render target formats: D3D12 cannot blend into them.
fn integer_format(f: DXGI_FORMAT) -> bool {
    matches!(
        f,
        DXGI_FORMAT_R32G32B32A32_UINT
            | DXGI_FORMAT_R32G32B32A32_SINT
            | DXGI_FORMAT_R32G32B32_UINT
            | DXGI_FORMAT_R32G32B32_SINT
            | DXGI_FORMAT_R16G16B16A16_UINT
            | DXGI_FORMAT_R16G16B16A16_SINT
            | DXGI_FORMAT_R32G32_UINT
            | DXGI_FORMAT_R32G32_SINT
            | DXGI_FORMAT_R10G10B10A2_UINT
            | DXGI_FORMAT_R8G8B8A8_UINT
            | DXGI_FORMAT_R8G8B8A8_SINT
            | DXGI_FORMAT_R16G16_UINT
            | DXGI_FORMAT_R16G16_SINT
            | DXGI_FORMAT_R32_UINT
            | DXGI_FORMAT_R32_SINT
            | DXGI_FORMAT_R8G8_UINT
            | DXGI_FORMAT_R8G8_SINT
            | DXGI_FORMAT_R16_UINT
            | DXGI_FORMAT_R16_SINT
            | DXGI_FORMAT_R8_UINT
            | DXGI_FORMAT_R8_SINT
    )
}

/// The title's blend state as the host takes it: blending off on integer
/// render targets. The console ignores blending there, and titles leave it
/// on (the world sets it on an `R8_UINT` target), but D3D12 does not allow
/// it: vkd3d-proton refuses such a pipeline with `E_INVALIDARG` ("using
/// integer format is not supported"), and its draws are lost. With one
/// description for every target, each target gets its own copy so the
/// others keep theirs.
fn host_blend(blend: &D3D12_BLEND_DESC, targets: &D3D12_RT_FORMAT_ARRAY) -> D3D12_BLEND_DESC {
    let count = (targets.NumRenderTargets as usize).min(8);
    let per_target = blend.IndependentBlendEnable.as_bool();
    let blends = |i: usize| {
        blend.RenderTarget[if per_target { i } else { 0 }]
            .BlendEnable
            .as_bool()
    };
    if !(0..count).any(|i| integer_format(targets.RTFormats[i]) && blends(i)) {
        return *blend;
    }
    let mut host = *blend;
    if !per_target {
        host.IndependentBlendEnable = true.into();
        host.RenderTarget = [blend.RenderTarget[0]; 8];
    }
    for i in 0..count {
        if integer_format(targets.RTFormats[i]) {
            host.RenderTarget[i].BlendEnable = false.into();
        }
    }
    host
}

impl GraphicsState {
    pub(super) fn writes_depth_only(&self) -> bool {
        self.targets.NumRenderTargets == 0
            && self.depth_format != DXGI_FORMAT_UNKNOWN
            && self.depth.DepthEnable.as_bool()
            && self.depth.DepthWriteMask == D3D12_DEPTH_WRITE_MASK_ALL
    }

    /// The guest color buffer bypasses its blender for integer surfaces,
    /// even when the draw asks for blending. D3D12 validates blend enable
    /// against every active target format when the pipeline is created.
    pub(crate) fn apply_integer_blend_bypass(&mut self) {
        let integer = |f: &DXGI_FORMAT| {
            matches!(
                *f,
                DXGI_FORMAT_R32G32B32A32_UINT
                    | DXGI_FORMAT_R32G32B32A32_SINT
                    | DXGI_FORMAT_R32G32B32_UINT
                    | DXGI_FORMAT_R32G32B32_SINT
                    | DXGI_FORMAT_R16G16B16A16_UINT
                    | DXGI_FORMAT_R16G16B16A16_SINT
                    | DXGI_FORMAT_R32G32_UINT
                    | DXGI_FORMAT_R32G32_SINT
                    | DXGI_FORMAT_R10G10B10A2_UINT
                    | DXGI_FORMAT_R8G8B8A8_UINT
                    | DXGI_FORMAT_R8G8B8A8_SINT
                    | DXGI_FORMAT_R16G16_UINT
                    | DXGI_FORMAT_R16G16_SINT
                    | DXGI_FORMAT_R32_UINT
                    | DXGI_FORMAT_R32_SINT
                    | DXGI_FORMAT_R8G8_UINT
                    | DXGI_FORMAT_R8G8_SINT
                    | DXGI_FORMAT_R16_UINT
                    | DXGI_FORMAT_R16_SINT
                    | DXGI_FORMAT_R8_UINT
                    | DXGI_FORMAT_R8_SINT
            )
        };
        let formats = &self.targets.RTFormats[..(self.targets.NumRenderTargets as usize).min(8)];
        if !self.blend.IndependentBlendEnable.as_bool() {
            let shared = self.blend.RenderTarget[0];
            if !shared.BlendEnable.as_bool() || !formats.iter().any(integer) {
                return;
            }
            // With shared blending D3D12 reads slot zero for every target.
            // Expand it before overriding individual targets, including
            // their shared write mask.
            self.blend.RenderTarget[..formats.len()].fill(shared);
            self.blend.IndependentBlendEnable = (formats.len() > 1).into();
        }
        for (format, target) in formats.iter().zip(&mut self.blend.RenderTarget) {
            if integer(format) {
                target.BlendEnable = false.into();
            }
        }
    }

    /// Takes in one subobject of a stream: its type, then its data at +4
    /// (+8 for those holding pointers).
    ///
    /// # Safety
    /// `at` points at a whole subobject of type `kind`.
    pub(crate) unsafe fn read(&mut self, kind: u32, at: *const u8) {
        // SAFETY: the caller passes a whole subobject.
        unsafe {
            let data = at.add(4);
            match kind {
                8 => self.blend = data.cast::<D3D12_BLEND_DESC>().read_unaligned(),
                9 => self.sample_mask = data.cast::<u32>().read_unaligned(),
                10 => self.rasterizer = data.cast::<D3D12_RASTERIZER_DESC>().read_unaligned(),
                11 => {
                    self.depth = data.cast::<D3D12_DEPTH_STENCIL_DESC>().read_unaligned();
                    self.depth_bounds = false;
                }
                21 => {
                    let d = data.cast::<D3D12_DEPTH_STENCIL_DESC1>().read_unaligned();
                    self.depth_bounds = d.DepthBoundsTestEnable.as_bool();
                    self.depth = D3D12_DEPTH_STENCIL_DESC {
                        DepthEnable: d.DepthEnable,
                        DepthWriteMask: d.DepthWriteMask,
                        DepthFunc: d.DepthFunc,
                        StencilEnable: d.StencilEnable,
                        StencilReadMask: d.StencilReadMask,
                        StencilWriteMask: d.StencilWriteMask,
                        FrontFace: d.FrontFace,
                        BackFace: d.BackFace,
                    };
                }
                12 => {
                    let layout = at.add(8).cast::<D3D12_INPUT_LAYOUT_DESC>().read_unaligned();
                    if layout.pInputElementDescs.is_null() {
                        return;
                    }
                    let elements = std::slice::from_raw_parts(
                        layout.pInputElementDescs,
                        layout.NumElements as usize,
                    );
                    self.names = elements
                        .iter()
                        .map(|e| CStr::from_ptr(e.SemanticName.0.cast()).to_owned())
                        .collect();
                    self.inputs = elements
                        .iter()
                        .zip(&self.names)
                        .map(|(e, name)| D3D12_INPUT_ELEMENT_DESC {
                            SemanticName: PCSTR(name.as_ptr().cast()),
                            Format: DXGI_FORMAT(
                                crate::resource::pc_format(e.Format.0 as u32) as i32
                            ),
                            ..*e
                        })
                        .collect();
                }
                13 => {
                    self.strip_cut =
                        D3D12_INDEX_BUFFER_STRIP_CUT_VALUE(data.cast::<i32>().read_unaligned())
                }
                14 => {
                    self.topology =
                        D3D12_PRIMITIVE_TOPOLOGY_TYPE(data.cast::<i32>().read_unaligned())
                }
                15 => {
                    self.targets = data.cast::<D3D12_RT_FORMAT_ARRAY>().read_unaligned();
                    for f in &mut self.targets.RTFormats {
                        f.0 = crate::resource::target_format(crate::resource::pc_format(
                            f.0 as u32,
                        )) as i32;
                    }
                }
                16 => {
                    let f = data.cast::<u32>().read_unaligned();
                    self.depth_format = DXGI_FORMAT(crate::resource::pc_format(f) as i32);
                }
                17 => self.samples = data.cast::<DXGI_SAMPLE_DESC>().read_unaligned(),
                _ => {}
            }
        }
    }

    /// Whether a pipeline of this state draws a pass over the screen: no
    /// vertex input, no depth or stencil test, triangles. Titles fill tables
    /// (lighting lookups) with such draws once; their shaders are few.
    pub(crate) fn screen_pass(&self) -> bool {
        self.inputs.is_empty()
            && (self.depth_format == DXGI_FORMAT_UNKNOWN
                || !self.depth.DepthEnable.as_bool() && !self.depth.StencilEnable.as_bool())
            && self.topology == D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE
    }

    /// Everything the state holds, as text: part of a pipeline's name in
    /// the library and of the key a title pipeline is started early by.
    pub(crate) fn describe(&self) -> String {
        let inputs: Vec<String> = self
            .inputs
            .iter()
            .zip(&self.names)
            .map(|(e, name)| {
                format!(
                    "{name:?} {} {} {} {} {} {}",
                    e.SemanticIndex, e.Format.0, e.InputSlot, e.AlignedByteOffset, e.InputSlotClass.0, e.InstanceDataStepRate
                )
            })
            .collect();
        let bounds = if self.depth_bounds {
            " depth-bounds"
        } else {
            ""
        };
        format!(
            "{:?} {} {:?} {:?}{bounds} {inputs:?} {} {} {:?} {} {:?}",
            self.blend,
            self.sample_mask,
            self.rasterizer,
            self.depth,
            self.strip_cut.0,
            self.topology.0,
            self.targets,
            self.depth_format.0,
            self.samples
        )
    }

    fn depth1(&self) -> D3D12_DEPTH_STENCIL_DESC1 {
        let d = &self.depth;
        D3D12_DEPTH_STENCIL_DESC1 {
            DepthEnable: d.DepthEnable,
            DepthWriteMask: d.DepthWriteMask,
            DepthFunc: d.DepthFunc,
            StencilEnable: d.StencilEnable,
            StencilReadMask: d.StencilReadMask,
            StencilWriteMask: d.StencilWriteMask,
            FrontFace: d.FrontFace,
            BackFace: d.BackFace,
            DepthBoundsTestEnable: self.depth_bounds.into(),
        }
    }

    /// The vertex buffer slots the input layout reads.
    fn slots(&self) -> u32 {
        self.inputs
            .iter()
            .fold(0, |m, e| m | 1 << e.InputSlot.min(31))
    }

    /// The input elements with their byte offsets worked out.
    fn elements(&self) -> Vec<Input> {
        let mut next = [0u32; 32];
        self.inputs
            .iter()
            .map(|e| {
                let slot = e.InputSlot.min(31) as usize;
                let format = e.Format.0 as u32;
                let offset = if e.AlignedByteOffset == D3D12_APPEND_ALIGNED_ELEMENT {
                    next[slot]
                } else {
                    e.AlignedByteOffset
                };
                let bytes = crate::resource::element(format).map_or(0, |(b, _)| b as u32);
                next[slot] = offset + bytes;
                Input {
                    slot: slot as u32,
                    offset,
                    format,
                    per_instance: e.InputSlotClass == D3D12_INPUT_CLASSIFICATION_PER_INSTANCE_DATA,
                }
            })
            .collect()
    }
}

/// One input element: vertex buffer slot, byte offset, format.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Input {
    pub(crate) slot: u32,
    pub(crate) offset: u32,
    pub(crate) format: u32,
    pub(crate) per_instance: bool,
}

/// The console's own primitive topology types.
const RECTANGLES: i32 = 19;
const QUADS: i32 = 32;

/// Whether the title's root SRVs are bound as one-view tables of the
/// buffer's host copy, whose reads past its end return zeros, rather than
/// as root descriptors, which have no size: a mesh shader's vertex fetch
/// has no record limit, and a read past the copy reaches whatever the host
/// placed after it (memory it has freed: the device is removed, upstream
/// PR #493). `VICTORY_ROOT_SRV_DIRECT=1` goes back to root descriptors.
pub(crate) fn bounded_root_srvs() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_ROOT_SRV_DIRECT").is_none())
}

/// One stage of a host pipeline and where its bindings go.
pub(crate) struct Stage {
    pub(crate) shader: Arc<Shader>,
    /// The root parameter of its user data, and its size in dwords.
    pub(crate) user_data: Option<(u32, u32)>,
    /// Root parameters of its root views: (title root parameter, ours).
    pub(crate) root_cbvs: Vec<(u8, u32)>,
    pub(crate) root_srvs: Vec<(u8, u32)>,
    /// Root UAVs: one-view tables like the SRVs (or root descriptors with
    /// `VICTORY_ROOT_SRV_DIRECT=1`).
    pub(crate) root_uavs: Vec<(u8, u32)>,
}

impl Stage {
    pub(crate) fn translated(&self) -> &Translated {
        self.shader
            .translated()
            .expect("host pipelines are made from translated shaders")
    }
}

pub(crate) struct HostPipeline {
    /// A compute pipeline (else graphics).
    pub(crate) compute: bool,
    /// A mesh pipeline (graphics without vertex input).
    pub(crate) mesh: bool,
    pub(crate) pso: ID3D12PipelineState,
    pub(crate) root: ID3D12RootSignature,
    /// Vertex, then pixel stage, or the compute stage.
    pub(crate) stages: Vec<Stage>,
    /// The vertex buffer slots it reads (bit per slot).
    pub(crate) slots: u32,
    pub(crate) inputs: Vec<Input>,
    /// Has the geometry stage that completes the console's rectangles
    /// (`victory_shader::emit::rect_list_gs`): its rectangle draws go out
    /// as the title gives them, three vertices a rectangle.
    pub(crate) rects: bool,
    /// Its shaders, for reports of slow submissions.
    pub(crate) name: String,
    /// Its depth, stencil, cull and blend state in short, for the frame
    /// listing (empty for compute).
    #[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
    pub(crate) state: String,
}

/// Logs a pipeline that gets no host pipeline, once per reason.
fn refuse(reason: &'static str, shaders: &[Arc<Shader>]) -> Option<HostPipeline> {
    static SEEN: std::sync::Mutex<Vec<&str>> = std::sync::Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if !seen.contains(&reason) {
        seen.push(reason);
        tracing::warn!(?shaders, "no host pipeline: {reason} (logged once)");
    }
    None
}

impl HostPipeline {
    /// A host pipeline for vertex and pixel shaders with the graphics
    /// state `g`, or for one compute shader.
    pub(crate) fn new(shaders: &[Arc<Shader>], g: Option<&GraphicsState>) -> Option<HostPipeline> {
        // For the report when the host device is removed: what this thread
        // has in the library from here on.
        super::library::label(format!("{shaders:?}"));
        let find = |stage| shaders.iter().find(|s| s.stage == stage);
        let compute = match shaders {
            [cs] if cs.stage == "compute" => Some(cs),
            _ => None,
        };
        if compute.is_none()
            && shaders
                .iter()
                .any(|s| !matches!(s.stage, "vertex" | "pixel" | "mesh"))
        {
            return refuse(
                "only vertex, mesh, pixel and compute stages are translated",
                shaders,
            );
        }
        let mesh = find("mesh");
        let stages = if let Some(cs) = compute {
            vec![(cs, D3D12_SHADER_VISIBILITY_ALL)]
        } else {
            let first = match (mesh, find("vertex")) {
                (Some(ms), _) => (ms, D3D12_SHADER_VISIBILITY_MESH),
                (None, Some(vs)) => (vs, D3D12_SHADER_VISIBILITY_VERTEX),
                (None, None) => return refuse("no vertex or mesh shader", shaders),
            };
            let mut stages = vec![first];
            stages.extend(find("pixel").map(|ps| (ps, D3D12_SHADER_VISIBILITY_PIXEL)));
            stages
        };
        if mesh.is_some() && !host()?.mesh_shaders() {
            return refuse("the host GPU has no mesh shaders (or no shader model 6.6)", shaders);
        }
        for (s, _) in &stages {
            s.translated()?;
        }
        let host = host()?;

        // The root signature the translated shaders' bindings define.
        let mut params: Vec<D3D12_ROOT_PARAMETER1> = Vec::new();
        // The ranges table parameters point at, alive until the root
        // signature is made.
        let mut ranges: Vec<Box<D3D12_DESCRIPTOR_RANGE1>> = Vec::new();
        let mut samplers = Vec::new();
        let mut bound = Vec::new();
        for &(shader, visibility) in &stages {
            let t = &shader.translated()?.host;
            let mut stage = Stage {
                shader: shader.clone(),
                user_data: None,
                root_cbvs: Vec::new(),
                root_srvs: Vec::new(),
                root_uavs: Vec::new(),
            };
            let dwords = t.user_data_dwords();
            if dwords > 0 {
                stage.user_data = Some((params.len() as u32, dwords));
                params.push(D3D12_ROOT_PARAMETER1 {
                    ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
                    Anonymous: D3D12_ROOT_PARAMETER1_0 {
                        Constants: D3D12_ROOT_CONSTANTS {
                            ShaderRegister: 0,
                            RegisterSpace: USER_DATA_SPACE,
                            Num32BitValues: dwords,
                        },
                    },
                    ShaderVisibility: visibility,
                });
            }
            for (slots, kind, out) in [
                (
                    &t.root_cbvs,
                    D3D12_ROOT_PARAMETER_TYPE_CBV,
                    &mut stage.root_cbvs,
                ),
                (
                    &t.root_srvs,
                    D3D12_ROOT_PARAMETER_TYPE_SRV,
                    &mut stage.root_srvs,
                ),
                (
                    &t.root_uavs,
                    D3D12_ROOT_PARAMETER_TYPE_UAV,
                    &mut stage.root_uavs,
                ),
            ] {
                for &slot in slots {
                    let uav = kind == D3D12_ROOT_PARAMETER_TYPE_UAV;
                    // Ours, not the title's, stay root descriptors: the
                    // table of strides, the directory of title memory by
                    // address and its fault notes, the write marks' table
                    // and bitmap.
                    let ours = if uav {
                        slot == victory_shader::emit::SLOT_FAULTS || slot == victory_shader::emit::SLOT_VIEW_WRITES
                    } else {
                        slot == victory_shader::emit::SLOT_STRIDES
                            || slot == victory_shader::emit::SLOT_VIEW_ADDRESSES
                            || slot == victory_shader::emit::SLOT_MEMORY
                            || slot == victory_shader::emit::SLOT_VIEW_MARKS
                    };
                    if (uav || kind == D3D12_ROOT_PARAMETER_TYPE_SRV) && !ours && bounded_root_srvs() {
                        // A one-view table: the view's size bounds the
                        // reads (and a root UAV's writes), which a root
                        // descriptor leaves unbounded.
                        // (Our own table of strides stays a descriptor.)
                        ranges.push(Box::new(D3D12_DESCRIPTOR_RANGE1 {
                            RangeType: if uav {
                                D3D12_DESCRIPTOR_RANGE_TYPE_UAV
                            } else {
                                D3D12_DESCRIPTOR_RANGE_TYPE_SRV
                            },
                            NumDescriptors: 1,
                            BaseShaderRegister: u32::from(slot),
                            RegisterSpace: ROOT_CBV_SPACE,
                            Flags: D3D12_DESCRIPTOR_RANGE_FLAG_DESCRIPTORS_VOLATILE
                                | D3D12_DESCRIPTOR_RANGE_FLAG_DATA_VOLATILE,
                            OffsetInDescriptorsFromTableStart: 0,
                        }));
                        let range: *const D3D12_DESCRIPTOR_RANGE1 = &**ranges.last()?;
                        out.push((slot, params.len() as u32));
                        params.push(D3D12_ROOT_PARAMETER1 {
                            ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
                            Anonymous: D3D12_ROOT_PARAMETER1_0 {
                                DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE1 {
                                    NumDescriptorRanges: 1,
                                    pDescriptorRanges: range,
                                },
                            },
                            ShaderVisibility: visibility,
                        });
                        continue;
                    }
                    out.push((slot, params.len() as u32));
                    params.push(D3D12_ROOT_PARAMETER1 {
                        ParameterType: kind,
                        Anonymous: D3D12_ROOT_PARAMETER1_0 {
                            Descriptor: D3D12_ROOT_DESCRIPTOR1 {
                                ShaderRegister: u32::from(slot),
                                RegisterSpace: ROOT_CBV_SPACE,
                                Flags: D3D12_ROOT_DESCRIPTOR_FLAG_DATA_VOLATILE,
                            },
                        },
                        ShaderVisibility: visibility,
                    });
                }
            }
            for (n, s) in t.static_samplers.iter().enumerate() {
                samplers.push(D3D12_STATIC_SAMPLER_DESC {
                    Filter: D3D12_FILTER(s.filter as i32),
                    AddressU: D3D12_TEXTURE_ADDRESS_MODE(s.address[0] as i32),
                    AddressV: D3D12_TEXTURE_ADDRESS_MODE(s.address[1] as i32),
                    AddressW: D3D12_TEXTURE_ADDRESS_MODE(s.address[2] as i32),
                    MipLODBias: s.mip_lod_bias,
                    MaxAnisotropy: s.max_anisotropy,
                    ComparisonFunc: D3D12_COMPARISON_FUNC(s.comparison as i32),
                    BorderColor: D3D12_STATIC_BORDER_COLOR(s.border as i32),
                    MinLOD: s.min_lod,
                    MaxLOD: s.max_lod,
                    ShaderRegister: n as u32,
                    RegisterSpace: SAMPLER_SPACE,
                    ShaderVisibility: visibility,
                });
            }
            bound.push(stage);
        }
        let (root, root_blob) = match root_signature(&host.device, &params, &samplers, compute.is_some()) {
            Ok(root) => root,
            Err(e) => {
                tracing::warn!(error = %e, ?shaders, "host root signature refused");
                return None;
            }
        };
        let code = |i: usize| {
            stages.get(i).and_then(|(s, _)| s.translated()).map_or(
                D3D12_SHADER_BYTECODE::default(),
                |t| D3D12_SHADER_BYTECODE {
                    pShaderBytecode: t.host.dxil.as_ptr().cast(),
                    BytecodeLength: t.host.dxil.len(),
                },
            )
        };
        // A program that asks for wider waves than the host runs is refused
        // by the driver with only "the parameter is incorrect": said here,
        // once per pipeline, and the driver not asked.
        let max_lanes = HOST_MAX_WAVE.load(std::sync::atomic::Ordering::Relaxed);
        for i in 0..stages.len() {
            let c = code(i);
            if c.BytecodeLength == 0 {
                continue;
            }
            // SAFETY: the translated program's bytes, alive for this call.
            let bytes = unsafe { std::slice::from_raw_parts(c.pShaderBytecode.cast::<u8>(), c.BytecodeLength) };
            if let Some(lanes) = wave_lanes_needed(bytes)
                && max_lanes != 0
                && lanes > max_lanes
            {
                tracing::warn!(
                    ?shaders,
                    lanes,
                    max_lanes,
                    "host pipeline not made: the program needs waves of {lanes} lanes and the host runs at most {max_lanes} (the translator kept the console's wave64: VICTORY_WAVE_WHY=1 says why)"
                );
                return None;
            }
        }
        if compute.is_some() {
            let desc = D3D12_COMPUTE_PIPELINE_STATE_DESC {
                // SAFETY: a borrowed pointer the description never releases.
                pRootSignature: unsafe { std::mem::transmute_copy(&root) },
                CS: code(0),
                ..Default::default()
            };
            let name = super::library::name("compute", &root_blob, &[&code_bytes(&code(0))], "");
            let made = super::library::pipeline(
                &name,
                // SAFETY: the description points at live data.
                |library, name| unsafe { library.LoadComputePipeline(name, &desc) },
                // SAFETY: as above.
                || unsafe { host.device.CreateComputePipelineState(&desc) },
            );
            let pso = match made {
                Ok(pso) => pso,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        ?shaders,
                        root_parameters = params.len(),
                        static_samplers = samplers.len(),
                        dxil_bytes = code(0).BytecodeLength,
                        wave_lanes = ?wave_lanes_needed(&code_bytes(&code(0))),
                        "host pipeline refused"
                    );
                    host.log_messages();
                    return None;
                }
            };
            tracing::info!(?shaders, "host pipeline");
            crate::host::name(&pso, || format!("{shaders:?}"));
            return Some(HostPipeline {
                compute: true,
                mesh: false,
                pso,
                root,
                stages: bound,
                name: format!("{shaders:?}"),
                slots: 0,
                inputs: Vec::new(),
                rects: false,
                state: String::new(),
            });
        }
        let Some(g) = g else {
            return refuse("graphics state not kept (plain description)", shaders);
        };
        let depth_bounds = if g.depth_bounds {
            if host.depth_bounds() {
                true
            } else {
                static SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                if SEEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 4 {
                    tracing::warn!(
                        "a pipeline enables the depth bounds test, which the host GPU does not support: its draws pass at every depth"
                    );
                }
                false
            }
        } else {
            false
        };
        let state = g.describe();
        if mesh.is_some() {
            // "triangles": the stream names its topology since it did not
            // load from the library without one (see `mesh_pipeline`); a
            // new name keeps such entries, stored before, from being
            // looked up again.
            let name = super::library::name(
                "mesh triangles",
                &root_blob,
                &[&code_bytes(&code(0)), &code_bytes(&code(1))],
                &state,
            );
            let pso = match mesh_pipeline(&host.device, &root, code(0), code(1), g, depth_bounds, &name) {
                Ok(pso) => pso,
                Err(e) => {
                    tracing::warn!(error = %e, ?shaders, "host mesh pipeline refused");
                    host.log_messages();
                    return None;
                }
            };
            tracing::info!(?shaders, "host mesh pipeline");
            crate::host::name(&pso, || format!("{shaders:?}"));
            return Some(HostPipeline {
                compute: false,
                mesh: true,
                pso,
                root,
                stages: bound,
                name: format!("{shaders:?}"),
                slots: 0,
                inputs: Vec::new(),
                rects: false,
                state: String::new(),
            });
        }

        // A pipeline the title makes for rectangles gets a geometry stage
        // that completes each one from the three vertices the vertex
        // shader made. Without it (refused here or by the host) the draws
        // fall back on what `draw` does for rectangles.
        let mut rect_gs = None;
        if g.topology.0 == RECTANGLES {
            let ps = stages.get(1).and_then(|(s, _)| s.translated());
            let vs = &stages[0].0.translated()?.host.dxil;
            match victory_shader::emit::rect_list_gs(vs, ps.map(|t| &t.host.dxil[..])) {
                Ok(gs) => rect_gs = Some(gs),
                Err(e) => tracing::warn!(error = %e, ?shaders, "no rectangle geometry stage"),
            }
        }
        let mut desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
            // SAFETY: a borrowed pointer the description never releases.
            pRootSignature: unsafe { std::mem::transmute_copy(&root) },
            VS: code(0),
            PS: code(1),
            GS: rect_gs.as_ref().map_or(D3D12_SHADER_BYTECODE::default(), |gs| {
                D3D12_SHADER_BYTECODE {
                    pShaderBytecode: gs.as_ptr().cast(),
                    BytecodeLength: gs.len(),
                }
            }),
            BlendState: host_blend(&g.blend, &g.targets),
            SampleMask: g.sample_mask,
            RasterizerState: g.rasterizer,
            DepthStencilState: g.depth,
            InputLayout: D3D12_INPUT_LAYOUT_DESC {
                pInputElementDescs: g.inputs.as_ptr(),
                NumElements: g.inputs.len() as u32,
            },
            IBStripCutValue: g.strip_cut,
            // The console's rectangle and quad types draw as triangles on
            // the host (see `draw`).
            PrimitiveTopologyType: match g.topology.0 {
                RECTANGLES | QUADS => D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
                _ => g.topology,
            },
            NumRenderTargets: g.targets.NumRenderTargets,
            RTVFormats: g.targets.RTFormats,
            DSVFormat: g.depth_format,
            SampleDesc: g.samples,
            ..Default::default()
        };
        // A pipeline with the geometry stage has a name of its own in the
        // library; the others keep theirs.
        let name = |gs: Option<&Vec<u8>>| match gs {
            Some(gs) => super::library::name(
                "graphics rectangles",
                &root_blob,
                &[&code_bytes(&code(0)), &code_bytes(&code(1)), &gs[..]],
                &state,
            ),
            None => super::library::name(
                "graphics",
                &root_blob,
                &[&code_bytes(&code(0)), &code_bytes(&code(1))],
                &state,
            ),
        };
        let make = |desc: &D3D12_GRAPHICS_PIPELINE_STATE_DESC, name: &str| {
            if depth_bounds {
                return graphics_depth_bounds_pipeline(&host.device, &root, desc, g.depth1(), name);
            }
            super::library::pipeline(
                name,
                // SAFETY: the description points at live data.
                |library, name| unsafe { library.LoadGraphicsPipeline(name, desc) },
                // SAFETY: as above.
                || unsafe { host.device.CreateGraphicsPipelineState(desc) },
            )
        };
        let mut made = make(&desc, &name(rect_gs.as_ref()));
        if made.is_err() && rect_gs.is_some() {
            // The host refused the geometry stage: once more without it.
            tracing::warn!(?shaders, "host pipeline refused with the rectangle geometry stage");
            host.log_messages();
            desc.GS = D3D12_SHADER_BYTECODE::default();
            rect_gs = None;
            made = make(&desc, &name(None));
        }
        let pso = match made {
            Ok(pso) => pso,
            Err(e) => {
                tracing::warn!(error = %e, ?shaders, "host pipeline refused");
                host.log_messages();
                return None;
            }
        };
        tracing::info!(?shaders, "host pipeline");
        crate::host::name(&pso, || format!("{shaders:?}"));
        tracing::debug!(
            ?shaders,
            depth = ?g.depth,
            depth_bounds,
            dsv = g.depth_format.0,
            targets = ?&g.targets.RTFormats[..g.targets.NumRenderTargets.min(8) as usize],
            blend = ?g.blend.RenderTarget[0],
            cull = g.rasterizer.CullMode.0,
            "host graphics state"
        );
        Some(HostPipeline {
            compute: false,
            mesh: false,
            pso,
            root,
            stages: bound,
            name: format!("{shaders:?}"),
            slots: g.slots(),
            inputs: g.elements(),
            rects: rect_gs.is_some(),
            state: {
                let (d, r, b) = (&g.depth, &g.rasterizer, &g.blend.RenderTarget[0]);
                let face = |f: &D3D12_DEPTH_STENCILOP_DESC| {
                    format!(
                        "fail {} depth-fail {} pass {} func {}",
                        f.StencilFailOp.0, f.StencilDepthFailOp.0, f.StencilPassOp.0, f.StencilFunc.0
                    )
                };
                let bounds = if depth_bounds { " bounds true" } else { "" };
                format!(
                    "depth {} write {} func {}{bounds}; stencil {} read {:#x} write {:#x} front [{}] back [{}]; cull {} front-ccw {}; blend {} src {} dst {} op {} mask {:#x}",
                    d.DepthEnable.as_bool(),
                    d.DepthWriteMask.0,
                    d.DepthFunc.0,
                    d.StencilEnable.as_bool(),
                    d.StencilReadMask,
                    d.StencilWriteMask,
                    face(&d.FrontFace),
                    face(&d.BackFace),
                    r.CullMode.0,
                    r.FrontCounterClockwise.as_bool(),
                    b.BlendEnable.as_bool(),
                    b.SrcBlend.0,
                    b.DestBlend.0,
                    b.BlendOp.0,
                    b.RenderTargetWriteMask
                )
            },
        })
    }
}

/// A stream subobject: its type, then its description, pointer-aligned.
#[repr(C, align(8))]
struct Sub<T>(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE, T);

fn stream_pipeline<S>(
    device: &ID3D12Device,
    stream: &mut S,
    name: &str,
) -> windows::core::Result<ID3D12PipelineState> {
    use windows::core::Interface;
    let desc = D3D12_PIPELINE_STATE_STREAM_DESC {
        SizeInBytes: size_of::<S>(),
        pPipelineStateSubobjectStream: std::ptr::from_mut(stream).cast(),
    };
    let device = device.cast::<ID3D12Device2>()?;
    super::library::pipeline(
        name,
        // SAFETY: the stream holds whole subobjects pointing at live data.
        |library, name| unsafe { library.LoadPipeline(name, &desc) },
        // SAFETY: as above.
        || unsafe { device.CreatePipelineState(&desc) },
    )
}

/// A graphics pipeline enabling the depth bounds test: `D3D12_GRAPHICS_PIPELINE_STATE_DESC`
/// has no field for `DepthBoundsTestEnable`, so it is created and loaded as a
/// pipeline-state stream with `DEPTH_STENCIL1`.
fn graphics_depth_bounds_pipeline(
    device: &ID3D12Device,
    root: &ID3D12RootSignature,
    d: &D3D12_GRAPHICS_PIPELINE_STATE_DESC,
    depth: D3D12_DEPTH_STENCIL_DESC1,
    name: &str,
) -> windows::core::Result<ID3D12PipelineState> {
    use windows::core::Interface;
    #[repr(C)]
    struct Stream {
        root: Sub<*mut std::ffi::c_void>,
        vs: Sub<D3D12_SHADER_BYTECODE>,
        ps: Sub<D3D12_SHADER_BYTECODE>,
        blend: Sub<D3D12_BLEND_DESC>,
        mask: Sub<u32>,
        raster: Sub<D3D12_RASTERIZER_DESC>,
        depth: Sub<D3D12_DEPTH_STENCIL_DESC1>,
        input: Sub<D3D12_INPUT_LAYOUT_DESC>,
        strip_cut: Sub<D3D12_INDEX_BUFFER_STRIP_CUT_VALUE>,
        topology: Sub<D3D12_PRIMITIVE_TOPOLOGY_TYPE>,
        targets: Sub<D3D12_RT_FORMAT_ARRAY>,
        dsv: Sub<DXGI_FORMAT>,
        samples: Sub<DXGI_SAMPLE_DESC>,
    }
    #[repr(C)]
    struct StreamGs {
        base: Stream,
        gs: Sub<D3D12_SHADER_BYTECODE>,
    }
    let targets = D3D12_RT_FORMAT_ARRAY {
        RTFormats: d.RTVFormats,
        NumRenderTargets: d.NumRenderTargets,
    };
    let base = Stream {
        root: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_ROOT_SIGNATURE, root.as_raw()),
        vs: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_VS, d.VS),
        ps: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_PS, d.PS),
        blend: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_BLEND, d.BlendState),
        mask: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_SAMPLE_MASK, d.SampleMask),
        raster: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_RASTERIZER, d.RasterizerState),
        depth: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL1, depth),
        input: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_INPUT_LAYOUT, d.InputLayout),
        strip_cut: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_IB_STRIP_CUT_VALUE, d.IBStripCutValue),
        topology: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_PRIMITIVE_TOPOLOGY, d.PrimitiveTopologyType),
        targets: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_RENDER_TARGET_FORMATS, targets),
        dsv: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL_FORMAT, d.DSVFormat),
        samples: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_SAMPLE_DESC, d.SampleDesc),
    };
    if d.GS.pShaderBytecode.is_null() {
        let mut stream = base;
        stream_pipeline(device, &mut stream, name)
    } else {
        let mut stream = StreamGs {
            base,
            gs: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_GS, d.GS),
        };
        stream_pipeline(device, &mut stream, name)
    }
}

/// A mesh pipeline from the title's graphics state: the pipeline-state
/// stream form (no input layout).
///
/// The stream names a triangle topology, which D3D12 does not need for a
/// mesh shader: without it the pipeline library refuses to load a stored
/// pipeline with the very stream it was made from (`E_INVALIDARG`; debug
/// layer message 972, "does not match the one used to create the PSO"), so
/// every mesh pipeline was compiled again, and its store then refused (970,
/// "already exists"). The translation makes triangle mesh shaders only
/// (`mesh_prologue` refuses others), and the debug layer wants the two to
/// agree (1323).
fn mesh_pipeline(
    device: &ID3D12Device,
    root: &ID3D12RootSignature,
    ms: D3D12_SHADER_BYTECODE,
    ps: D3D12_SHADER_BYTECODE,
    g: &GraphicsState,
    depth_bounds: bool,
    name: &str,
) -> windows::core::Result<ID3D12PipelineState> {
    use windows::core::Interface;
    #[repr(C)]
    struct Stream<D> {
        root: Sub<*mut std::ffi::c_void>,
        ms: Sub<D3D12_SHADER_BYTECODE>,
        ps: Sub<D3D12_SHADER_BYTECODE>,
        blend: Sub<D3D12_BLEND_DESC>,
        mask: Sub<u32>,
        raster: Sub<D3D12_RASTERIZER_DESC>,
        depth: Sub<D>,
        targets: Sub<D3D12_RT_FORMAT_ARRAY>,
        dsv: Sub<DXGI_FORMAT>,
        samples: Sub<DXGI_SAMPLE_DESC>,
        topology: Sub<D3D12_PRIMITIVE_TOPOLOGY_TYPE>,
    }
    let build = |depth: Sub<_>| {
        let mut stream = Stream {
            root: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_ROOT_SIGNATURE, root.as_raw()),
            ms: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_MS, ms),
            ps: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_PS, ps),
            blend: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_BLEND, host_blend(&g.blend, &g.targets)),
            mask: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_SAMPLE_MASK, g.sample_mask),
            raster: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_RASTERIZER, g.rasterizer),
            depth,
            targets: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_RENDER_TARGET_FORMATS, g.targets),
            dsv: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL_FORMAT, g.depth_format),
            samples: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_SAMPLE_DESC, g.samples),
            topology: Sub(
                D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_PRIMITIVE_TOPOLOGY,
                D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
            ),
        };
        stream_pipeline(device, &mut stream, name)
    };
    if depth_bounds {
        let mut stream = Stream {
            root: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_ROOT_SIGNATURE, root.as_raw()),
            ms: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_MS, ms),
            ps: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_PS, ps),
            blend: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_BLEND, host_blend(&g.blend, &g.targets)),
            mask: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_SAMPLE_MASK, g.sample_mask),
            raster: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_RASTERIZER, g.rasterizer),
            depth: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL1, g.depth1()),
            targets: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_RENDER_TARGET_FORMATS, g.targets),
            dsv: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL_FORMAT, g.depth_format),
            samples: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_SAMPLE_DESC, g.samples),
            topology: Sub(
                D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_PRIMITIVE_TOPOLOGY,
                D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
            ),
        };
        stream_pipeline(device, &mut stream, name)
    } else {
        build(Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL, g.depth))
    }
}

/// The bytes of shader code (none for an absent stage).
fn code_bytes(code: &D3D12_SHADER_BYTECODE) -> Vec<u8> {
    if code.pShaderBytecode.is_null() {
        return Vec::new();
    }
    // SAFETY: a translation's own bytes, alive while its shader is.
    unsafe { std::slice::from_raw_parts(code.pShaderBytecode.cast::<u8>(), code.BytecodeLength) }.to_vec()
}

fn root_signature(
    device: &ID3D12Device,
    params: &[D3D12_ROOT_PARAMETER1],
    samplers: &[D3D12_STATIC_SAMPLER_DESC],
    compute: bool,
) -> windows::core::Result<(ID3D12RootSignature, Vec<u8>)> {
    let input = if compute {
        D3D12_ROOT_SIGNATURE_FLAG_NONE
    } else {
        D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT
    };
    let desc = D3D12_VERSIONED_ROOT_SIGNATURE_DESC {
        Version: windows::Win32::Graphics::Direct3D12::D3D_ROOT_SIGNATURE_VERSION_1_1,
        Anonymous: D3D12_VERSIONED_ROOT_SIGNATURE_DESC_0 {
            Desc_1_1: D3D12_ROOT_SIGNATURE_DESC1 {
                NumParameters: params.len() as u32,
                pParameters: params.as_ptr(),
                NumStaticSamplers: samplers.len() as u32,
                pStaticSamplers: samplers.as_ptr(),
                Flags: input
                    | D3D12_ROOT_SIGNATURE_FLAG_CBV_SRV_UAV_HEAP_DIRECTLY_INDEXED
                    | D3D12_ROOT_SIGNATURE_FLAG_SAMPLER_HEAP_DIRECTLY_INDEXED,
            },
        },
    };
    // SAFETY: the description points at live arrays; the blob is read in
    // full before it is released.
    unsafe {
        let mut blob: Option<ID3DBlob> = None;
        let mut error: Option<ID3DBlob> = None;
        if let Err(e) = D3D12SerializeVersionedRootSignature(&desc, &mut blob, Some(&mut error)) {
            if let Some(error) = error {
                let text = std::slice::from_raw_parts(
                    error.GetBufferPointer().cast::<u8>(),
                    error.GetBufferSize(),
                );
                tracing::warn!("root signature: {}", String::from_utf8_lossy(text));
            }
            return Err(e);
        }
        let blob = blob.ok_or_else(windows::core::Error::empty)?;
        let bytes =
            std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize());
        Ok((device.CreateRootSignature(0, bytes)?, bytes.to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::Ordering;

    use super::*;

    #[test]
    fn integer_target_bypasses_blending_and_preserves_mixed_target_masks() {
        let mut g = GraphicsState::default();
        g.targets.NumRenderTargets = 3;
        g.targets.RTFormats[..3].copy_from_slice(&[
            DXGI_FORMAT_R32_FLOAT,
            DXGI_FORMAT_B4G4R4A4_UNORM,
            DXGI_FORMAT_R8_UINT,
        ]);
        g.blend.IndependentBlendEnable = true.into();
        for (i, target) in g.blend.RenderTarget[..3].iter_mut().enumerate() {
            target.BlendEnable = true.into();
            target.BlendOp = D3D12_BLEND_OP_MAX;
            target.RenderTargetWriteMask = [15, 15, 1][i];
        }
        let before = g.blend;
        g.apply_integer_blend_bypass();
        assert_eq!(g.blend.RenderTarget[0..2], before.RenderTarget[0..2]);
        let mut expected = before.RenderTarget[2];
        expected.BlendEnable = false.into();
        assert_eq!(g.blend.RenderTarget[2], expected);

        g.blend = before;
        g.blend.IndependentBlendEnable = false.into();
        g.blend.RenderTarget[0].RenderTargetWriteMask = 3;
        g.targets.RTFormats.swap(0, 2);
        g.apply_integer_blend_bypass();
        assert!(g.blend.IndependentBlendEnable.as_bool());
        assert!(!g.blend.RenderTarget[0].BlendEnable.as_bool());
        for target in &g.blend.RenderTarget[1..3] {
            assert!(target.BlendEnable.as_bool());
            assert_eq!(target.BlendOp, D3D12_BLEND_OP_MAX);
            assert_eq!(target.RenderTargetWriteMask, 3);
        }
    }

    /// A triangle mesh shader of our own and a pixel shader for it.
    const MS: &str = "struct V { float4 p : SV_Position; };
        [outputtopology(\"triangle\")] [numthreads(1, 1, 1)]
        void main(out vertices V v[3], out indices uint3 t[1]) {
            SetMeshOutputCounts(3, 1);
            v[0].p = float4(0, 0, 0, 1);
            v[1].p = float4(1, 0, 0, 1);
            v[2].p = float4(0, 1, 0, 1);
            t[0] = uint3(0, 1, 2);
        }";
    const PS: &str = "float4 main() : SV_Target { return 1; }";

    /// The PC `dxc.exe` (`VICTORY_PC_DXC`, else the newest Windows SDK's).
    fn dxc() -> Option<PathBuf> {
        let dxc = match std::env::var_os("VICTORY_PC_DXC") {
            Some(p) => PathBuf::from(p),
            None => victory_shader::emit::validator::Validator::default_path()?
                .with_file_name("dxc.exe"),
        };
        dxc.exists().then_some(dxc)
    }

    fn compile(dxc: &Path, dir: &Path, src: &str, profile: &str) -> Vec<u8> {
        let (hlsl, out) = (
            dir.join(format!("{profile}.hlsl")),
            dir.join(format!("{profile}.dxil")),
        );
        std::fs::write(&hlsl, src).unwrap();
        let status = std::process::Command::new(dxc)
            .args(["-nologo", "-T", profile, "-E", "main", "-Fo"])
            .arg(&out)
            .arg(&hlsl)
            .status()
            .unwrap();
        assert!(status.success(), "dxc failed for {profile}");
        std::fs::read(out).unwrap()
    }

    fn code(bytes: &[u8]) -> D3D12_SHADER_BYTECODE {
        D3D12_SHADER_BYTECODE {
            pShaderBytecode: bytes.as_ptr().cast(),
            BytecodeLength: bytes.len(),
        }
    }

    /// A mesh pipeline made once is loaded from the library the next time,
    /// not compiled again (a stream without a topology never loads).
    #[test]
    fn a_mesh_pipeline_loads_from_the_library() {
        let Some(host) = host() else {
            return; // no host GPU here
        };
        if !host.mesh_shaders() {
            eprintln!("no host mesh shader support; skipped");
            return;
        }
        let Some(dxc) = dxc() else {
            eprintln!("no PC dxc.exe; skipped");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let ms = compile(&dxc, dir.path(), MS, "ms_6_5");
        let ps = compile(&dxc, dir.path(), PS, "ps_6_5");
        let (root, root_blob) = root_signature(&host.device, &[], &[], false).unwrap();
        let mut g = GraphicsState::default();
        // This pipeline has only a color target, with no depth buffer.
        g.depth.DepthEnable = false.into();
        g.targets.NumRenderTargets = 1;
        g.targets.RTFormats[0] = DXGI_FORMAT_R8G8B8A8_UNORM;
        let name = super::super::library::name("mesh test", &root_blob, &[&ms, &ps], &g.describe());
        mesh_pipeline(&host.device, &root, code(&ms), code(&ps), &g, false, &name).expect("made");
        let loaded = super::super::library::LOADED.load(Ordering::Relaxed);
        mesh_pipeline(&host.device, &root, code(&ms), code(&ps), &g, false, &name).expect("made again");
        assert!(
            super::super::library::LOADED.load(Ordering::Relaxed) > loaded,
            "the mesh pipeline was compiled again instead of loaded from the library"
        );
    }

    fn targets(formats: &[DXGI_FORMAT]) -> D3D12_RT_FORMAT_ARRAY {
        let mut t = D3D12_RT_FORMAT_ARRAY {
            NumRenderTargets: formats.len() as u32,
            ..Default::default()
        };
        t.RTFormats[..formats.len()].copy_from_slice(formats);
        t
    }

    fn blending(independent: bool) -> D3D12_BLEND_DESC {
        let mut b = GraphicsState::default().blend;
        b.IndependentBlendEnable = independent.into();
        for t in &mut b.RenderTarget {
            t.BlendEnable = true.into();
            t.SrcBlend = D3D12_BLEND_SRC_ALPHA;
            t.DestBlend = D3D12_BLEND_INV_SRC_ALPHA;
        }
        b
    }

    #[test]
    fn integer_targets_do_not_blend() {
        let t = targets(&[
            DXGI_FORMAT_R8G8B8A8_UNORM,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            DXGI_FORMAT_R8_UINT,
        ]);
        for independent in [false, true] {
            let host = host_blend(&blending(independent), &t);
            assert!(host.IndependentBlendEnable.as_bool());
            assert!(host.RenderTarget[0].BlendEnable.as_bool());
            assert!(host.RenderTarget[1].BlendEnable.as_bool());
            assert!(!host.RenderTarget[2].BlendEnable.as_bool());
            // The rest of the target's state is the title's.
            assert_eq!(host.RenderTarget[2].SrcBlend, D3D12_BLEND_SRC_ALPHA);
            assert_eq!(host.RenderTarget[0].DestBlend, D3D12_BLEND_INV_SRC_ALPHA);
        }
    }

    #[test]
    fn blend_states_without_integer_blending_are_kept() {
        // No integer target.
        let b = blending(false);
        let t = targets(&[DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R11G11B10_FLOAT]);
        assert_eq!(host_blend(&b, &t), b);
        // An integer target that does not blend.
        let mut b = blending(true);
        b.RenderTarget[1].BlendEnable = false.into();
        let t = targets(&[DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R32_UINT]);
        assert_eq!(host_blend(&b, &t), b);
        // Integer formats past the targets in use.
        let mut t = targets(&[DXGI_FORMAT_R8G8B8A8_UNORM]);
        t.RTFormats[1] = DXGI_FORMAT_R8_UINT;
        assert_eq!(host_blend(&blending(false), &t), blending(false));
    }

    const VS: &str = "float4 main(uint i : SV_VertexID) : SV_Position {
        float2 p = float2((i << 1) & 2, i & 2);
        return float4(p * 2.0 - 1.0, 0.5, 1.0);
    }";

    /// A graphics pipeline with `DepthBoundsTestEnable` loads back from the
    /// pipeline library and culls or passes fragments according to
    /// `OMSetDepthBounds` (including across a command-list cut).
    #[test]
    fn depth_bounds_pipeline_loads_and_filters_draws() {
        let Some(host) = host() else {
            return;
        };
        if !host.depth_bounds() {
            eprintln!("no host depth bounds support; skipped");
            return;
        }
        // WARP does not cull draws on OMSetDepthBounds on Windows Server CI even
        // when D3D12_OPTIONS2 advertises support.
        let software = unsafe {
            use windows::Win32::Graphics::Dxgi::{
                CreateDXGIFactory2, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_CREATE_FACTORY_FLAGS,
                IDXGIAdapter1, IDXGIFactory4,
            };
            CreateDXGIFactory2::<IDXGIFactory4>(DXGI_CREATE_FACTORY_FLAGS(0))
                .and_then(|f| f.EnumAdapterByLuid::<IDXGIAdapter1>(host.device.GetAdapterLuid()))
                .and_then(|a| a.GetDesc1())
                .is_ok_and(|d| d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0)
        };
        if software {
            eprintln!("software WARP adapter does not enforce OMSetDepthBounds; skipped");
            return;
        }
        let Some(dxc) = dxc() else {
            eprintln!("no PC dxc.exe; skipped");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let vs = compile(&dxc, dir.path(), VS, "vs_6_5");
        let ps = compile(&dxc, dir.path(), PS, "ps_6_5");
        let (root, root_blob) = root_signature(&host.device, &[], &[], false).unwrap();

        let mut g = GraphicsState::default();
        // Subobject 21 (`DEPTH_STENCIL1`) with `DepthBoundsTestEnable = true`,
        // depth test itself off so only depth bounds gates the draw.
        #[repr(C, align(8))]
        struct Sub21(u32, D3D12_DEPTH_STENCIL_DESC1);
        let sub = Sub21(
            21,
            D3D12_DEPTH_STENCIL_DESC1 {
                DepthEnable: false.into(),
                DepthWriteMask: D3D12_DEPTH_WRITE_MASK_ZERO,
                DepthFunc: D3D12_COMPARISON_FUNC_ALWAYS,
                DepthBoundsTestEnable: true.into(),
                ..Default::default()
            },
        );
        // SAFETY: `sub` is a complete subobject 21.
        unsafe { g.read(21, (&raw const sub).cast()) };
        assert!(g.depth_bounds);
        assert!(g.describe().contains("depth-bounds"));
        g.rasterizer.CullMode = D3D12_CULL_MODE_NONE;
        g.targets.NumRenderTargets = 1;
        g.targets.RTFormats[0] = DXGI_FORMAT_R8G8B8A8_UNORM;
        g.depth_format = DXGI_FORMAT_D32_FLOAT;

        let desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
            // SAFETY: borrowed pointer, never released by the descriptor.
            pRootSignature: unsafe { std::mem::transmute_copy(&root) },
            VS: code(&vs),
            PS: code(&ps),
            BlendState: g.blend,
            SampleMask: g.sample_mask,
            RasterizerState: g.rasterizer,
            DepthStencilState: g.depth,
            IBStripCutValue: g.strip_cut,
            PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
            NumRenderTargets: 1,
            RTVFormats: g.targets.RTFormats,
            DSVFormat: g.depth_format,
            SampleDesc: g.samples,
            ..Default::default()
        };
        let name = super::super::library::name(
            "graphics depth-bounds test",
            &root_blob,
            &[&vs, &ps],
            &g.describe(),
        );
        let pso = graphics_depth_bounds_pipeline(&host.device, &root, &desc, g.depth1(), &name)
            .expect("depth-bounds PSO made");
        let loaded = super::super::library::LOADED.load(Ordering::Relaxed);
        graphics_depth_bounds_pipeline(&host.device, &root, &desc, g.depth1(), &name)
            .expect("depth-bounds PSO made again");
        assert!(
            super::super::library::LOADED.load(Ordering::Relaxed) > loaded,
            "the depth-bounds pipeline was compiled again instead of loaded from the library"
        );

        // Execute two draws against a depth buffer cleared to 0.5:
        // 1) bounds [0.6, 1.0] -> rejected (target stays 0)
        // 2) bounds [0.4, 0.6] replayed onto a fresh HostList -> passes (target becomes 255)
        let make_tex = |format, flags, state, clear: D3D12_CLEAR_VALUE| {
            let heap = D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_DEFAULT,
                ..Default::default()
            };
            let rd = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                Width: 4,
                Height: 4,
                DepthOrArraySize: 1,
                MipLevels: 1,
                Format: format,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Flags: flags,
                ..Default::default()
            };
            let mut out: Option<ID3D12Resource> = None;
            // SAFETY: valid heap and resource descriptions.
            unsafe {
                host.device
                    .CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &rd, state, Some(&clear), &mut out)
                    .unwrap();
            }
            out.unwrap()
        };
        let color = make_tex(
            DXGI_FORMAT_R8G8B8A8_UNORM,
            D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
            D3D12_RESOURCE_STATE_RENDER_TARGET,
            D3D12_CLEAR_VALUE {
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                Anonymous: D3D12_CLEAR_VALUE_0 { Color: [0.0; 4] },
            },
        );
        let depth = make_tex(
            DXGI_FORMAT_D32_FLOAT,
            D3D12_RESOURCE_FLAG_ALLOW_DEPTH_STENCIL,
            D3D12_RESOURCE_STATE_DEPTH_WRITE,
            D3D12_CLEAR_VALUE {
                Format: DXGI_FORMAT_D32_FLOAT,
                Anonymous: D3D12_CLEAR_VALUE_0 {
                    DepthStencil: D3D12_DEPTH_STENCIL_VALUE {
                        Depth: 0.5,
                        Stencil: 0,
                    },
                },
            },
        );
        let readback = {
            let heap = D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_READBACK,
                ..Default::default()
            };
            let rd = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Width: 1024,
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                ..Default::default()
            };
            let mut out: Option<ID3D12Resource> = None;
            // SAFETY: valid readback buffer description.
            unsafe {
                host.device
                    .CreateCommittedResource(
                        &heap,
                        D3D12_HEAP_FLAG_NONE,
                        &rd,
                        D3D12_RESOURCE_STATE_COPY_DEST,
                        None,
                        &mut out,
                    )
                    .unwrap();
            }
            out.unwrap()
        };

        let run_draw = |min: f32, max: f32| -> u8 {
            let mut state = crate::draw::State::default();
            let first_list = host.new_list().unwrap();
            state.set_depth_bounds(&first_list, min, max);
            first_list.close();
            // Replay onto a second host list, exercising `State::replay`.
            let list = host.new_list().unwrap();
            state.replay(&list);
            let rtv = host.rtv(Some(&color), None);
            let dsv = host.dsv(&depth, None);
            let vp = D3D12_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: 4.0,
                Height: 4.0,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            };
            let rect = windows::Win32::Foundation::RECT {
                left: 0,
                top: 0,
                right: 4,
                bottom: 4,
            };
            // SAFETY: open command list, live resources and views.
            unsafe {
                list.list.ClearRenderTargetView(rtv, &[0.0; 4], None);
                list.list.ClearDepthStencilView(dsv, D3D12_CLEAR_FLAG_DEPTH, 0.5, 0, None);
                list.list.OMSetRenderTargets(1, Some(&rtv), false, Some(&dsv));
                list.list.RSSetViewports(&[vp]);
                list.list.RSSetScissorRects(&[rect]);
                list.list.SetGraphicsRootSignature(&root);
                list.list.SetPipelineState(&pso);
                list.list
                    .IASetPrimitiveTopology(windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                list.list.DrawInstanced(3, 1, 0, 0);
                list.list.ResourceBarrier(&[crate::host::transition(
                    &color,
                    D3D12_RESOURCE_STATE_RENDER_TARGET,
                    D3D12_RESOURCE_STATE_COPY_SOURCE,
                )]);
                let dst = D3D12_TEXTURE_COPY_LOCATION {
                    pResource: std::mem::transmute_copy(&readback),
                    Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                        PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                            Offset: 0,
                            Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                                Width: 4,
                                Height: 4,
                                Depth: 1,
                                RowPitch: 256,
                            },
                        },
                    },
                };
                let src = D3D12_TEXTURE_COPY_LOCATION {
                    pResource: std::mem::transmute_copy(&color),
                    Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
                };
                list.list.CopyTextureRegion(&dst, 0, 0, 0, &src, None);
                list.list.ResourceBarrier(&[crate::host::transition(
                    &color,
                    D3D12_RESOURCE_STATE_COPY_SOURCE,
                    D3D12_RESOURCE_STATE_RENDER_TARGET,
                )]);
            }
            list.close();
            host.execute(&[&list]);
            let mut ptr = std::ptr::null_mut();
            // SAFETY: `execute` waited for the GPU; readback holds 1024 bytes.
            unsafe {
                readback.Map(0, None, Some(&mut ptr)).unwrap();
                let pixel = *ptr.cast::<u8>();
                readback.Unmap(0, None);
                pixel
            }
        };

        // The software adapter of the CI's machine reports the depth bounds
        // test and draws the pixel outside the bounds all the same (the
        // pipeline above was still made and loaded back from the library);
        // a card rejects it, which is what the two draws check.
        if host.software() {
            eprintln!("a software adapter: the draws that check the bounds are skipped");
            return;
        }
        assert_eq!(run_draw(0.6, 1.0), 0, "depth 0.5 outside [0.6, 1.0] must be rejected");
        assert_eq!(run_draw(0.4, 0.6), 255, "depth 0.5 inside [0.4, 0.6] must pass");
    }
}

#[cfg(test)]
mod wave_tests {
    /// A container with one `PSV0` part whose runtime info asks `min..max` lanes.
    fn container(min: u32, max: u32) -> Vec<u8> {
        let mut psv = 48u32.to_le_bytes().to_vec();
        let mut info = [0u8; 48];
        info[16..20].copy_from_slice(&min.to_le_bytes());
        info[20..24].copy_from_slice(&max.to_le_bytes());
        psv.extend(info);
        let header = 0x20 + 4;
        let total = header + 8 + psv.len();
        let mut c = b"DXBC".to_vec();
        c.extend([0u8; 16]);
        c.extend(1u16.to_le_bytes());
        c.extend(0u16.to_le_bytes());
        c.extend((total as u32).to_le_bytes());
        c.extend(1u32.to_le_bytes());
        c.extend((header as u32).to_le_bytes());
        c.extend(b"PSV0");
        c.extend((psv.len() as u32).to_le_bytes());
        c.extend(psv);
        c
    }

    #[test]
    fn wave_size_read_from_psv() {
        assert_eq!(super::wave_lanes_needed(&container(64, 64)), Some(64));
        assert_eq!(super::wave_lanes_needed(&container(32, 32)), Some(32));
        assert_eq!(super::wave_lanes_needed(&container(0, u32::MAX)), None);
        assert_eq!(super::wave_lanes_needed(b"not a container"), None);
    }
}

#[cfg(test)]
mod typed_bits_tests {
    use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_12_0;
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
    use windows::core::Interface;

    /// The translator's typed buffer accesses: float loads and stores.
    const CS: &str = r#"Buffer<float> s : register(t0);
        RWBuffer<float> u : register(u0);
        [RootSignature("DescriptorTable(SRV(t0), UAV(u0))")]
        [numthreads(1, 1, 1)]
        void main() {
            for (uint k = 0; k < 8; k++) {
                u[k] = s[k];
                u[8 + k] = asfloat(asuint(s[k]) + 1);
                u[16 + k] = asfloat(k * 3 + 1);
            }
        }"#;

    /// Manual: whether float loads and stores through typed buffer views in
    /// R32_UINT move the bits on this card, against R32_FLOAT views.
    /// `cargo test -p victory-gpu --lib typed_bits -- --ignored --nocapture`
    #[test]
    #[ignore = "manual: measures the real card's driver"]
    fn float_accesses_through_integer_buffer_views() {
        let dxc = victory_shader::emit::validator::Validator::default_path()
            .unwrap()
            .with_file_name("dxc.exe");
        let dir = std::env::temp_dir().join(format!("victory-typed-bits-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (hlsl, out) = (dir.join("cs.hlsl"), dir.join("cs.dxil"));
        std::fs::write(&hlsl, CS).unwrap();
        let ok = std::process::Command::new(&dxc)
            .args(["-nologo", "-T", "cs_6_5", "-E", "main", "-Fo"])
            .arg(&out)
            .arg(&hlsl)
            .status()
            .unwrap()
            .success();
        assert!(ok, "dxc failed");
        let cs = std::fs::read(&out).unwrap();
        let input: [u32; 8] = [1, 2, 7, 0x8000_0001, 0x7FC0_0001, 0xFFFF_FFFF, 0x0080_0000, 12345];
        let warp = std::env::var("VICTORY_WARP").is_ok_and(|v| v == "1");
        // SAFETY: plain objects of our own on the default adapter (or WARP);
        // every submission is waited for.
        unsafe {
            let mut device: Option<ID3D12Device> = None;
            if warp {
                use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, IDXGIAdapter, IDXGIFactory4};
                let f: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).unwrap();
                let a: IDXGIAdapter = f.EnumWarpAdapter().unwrap();
                D3D12CreateDevice(&a, D3D_FEATURE_LEVEL_12_0, &mut device).unwrap();
            } else {
                D3D12CreateDevice(None, D3D_FEATURE_LEVEL_12_0, &mut device).unwrap();
            }
            let device = device.unwrap();
            let root: ID3D12RootSignature = device.CreateRootSignature(0, &cs).unwrap();
            let pso: ID3D12PipelineState = device
                .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                    pRootSignature: std::mem::transmute_copy(&root),
                    CS: D3D12_SHADER_BYTECODE { pShaderBytecode: cs.as_ptr().cast(), BytecodeLength: cs.len() },
                    ..Default::default()
                })
                .unwrap();
            let queue: ID3D12CommandQueue = device
                .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() })
                .unwrap();
            let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).unwrap();
            let event = CreateEventW(None, false, false, None).unwrap();
            let buffer = |heap, flags, state, bytes: u64| {
                let mut r: Option<ID3D12Resource> = None;
                device
                    .CreateCommittedResource(
                        &D3D12_HEAP_PROPERTIES { Type: heap, ..Default::default() },
                        D3D12_HEAP_FLAG_NONE,
                        &D3D12_RESOURCE_DESC {
                            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                            Width: bytes,
                            Height: 1,
                            DepthOrArraySize: 1,
                            MipLevels: 1,
                            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                            Flags: flags,
                            ..Default::default()
                        },
                        state,
                        None,
                        &mut r,
                    )
                    .unwrap();
                r.unwrap()
            };
            let upload = buffer(D3D12_HEAP_TYPE_UPLOAD, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_GENERIC_READ, 32);
            let mut p = std::ptr::null_mut();
            upload.Map(0, None, Some(&mut p)).unwrap();
            std::ptr::copy_nonoverlapping(input.as_ptr(), p.cast::<u32>(), 8);
            upload.Unmap(0, None);
            let heap: ID3D12DescriptorHeap = device
                .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                    NumDescriptors: 2,
                    Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                    NodeMask: 0,
                })
                .unwrap();
            let step = device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV) as usize;
            let mut fenced = 0u64;
            for (name, format) in [
                ("R32_UINT views", DXGI_FORMAT_R32_UINT),
                ("R32_FLOAT views", DXGI_FORMAT_R32_FLOAT),
                ("R32_SINT views", DXGI_FORMAT_R32_SINT),
            ] {
                let target = buffer(D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COMMON, 96);
                let back = buffer(D3D12_HEAP_TYPE_READBACK, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COPY_DEST, 96);
                let cpu = heap.GetCPUDescriptorHandleForHeapStart();
                device.CreateShaderResourceView(
                    &upload,
                    Some(&D3D12_SHADER_RESOURCE_VIEW_DESC {
                        Format: format,
                        ViewDimension: D3D12_SRV_DIMENSION_BUFFER,
                        Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                        Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                            Buffer: D3D12_BUFFER_SRV { FirstElement: 0, NumElements: 8, StructureByteStride: 0, Flags: D3D12_BUFFER_SRV_FLAG_NONE },
                        },
                    }),
                    cpu,
                );
                device.CreateUnorderedAccessView(
                    &target,
                    None::<&ID3D12Resource>,
                    Some(&D3D12_UNORDERED_ACCESS_VIEW_DESC {
                        Format: format,
                        ViewDimension: D3D12_UAV_DIMENSION_BUFFER,
                        Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
                            Buffer: D3D12_BUFFER_UAV { FirstElement: 0, NumElements: 24, ..Default::default() },
                        },
                    }),
                    D3D12_CPU_DESCRIPTOR_HANDLE { ptr: cpu.ptr + step },
                );
                let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT).unwrap();
                let l: ID3D12GraphicsCommandList =
                    device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, &pso).unwrap();
                l.SetDescriptorHeaps(&[Some(heap.clone())]);
                l.SetComputeRootSignature(&root);
                l.SetComputeRootDescriptorTable(0, heap.GetGPUDescriptorHandleForHeapStart());
                l.Dispatch(1, 1, 1);
                l.ResourceBarrier(&[D3D12_RESOURCE_BARRIER {
                    Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
                    Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
                    Anonymous: D3D12_RESOURCE_BARRIER_0 {
                        Transition: std::mem::ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                            pResource: std::mem::transmute_copy(&target),
                            Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                            StateBefore: D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                            StateAfter: D3D12_RESOURCE_STATE_COPY_SOURCE,
                        }),
                    },
                }]);
                l.CopyResource(&back, &target);
                l.Close().unwrap();
                queue.ExecuteCommandLists(&[l.cast().ok()]);
                fenced += 1;
                queue.Signal(&fence, fenced).unwrap();
                fence.SetEventOnCompletion(fenced, event).unwrap();
                WaitForSingleObject(event, INFINITE);
                let mut d = std::ptr::null_mut();
                back.Map(0, Some(&D3D12_RANGE { Begin: 0, End: 96 }), Some(&mut d)).unwrap();
                let got = std::slice::from_raw_parts(d.cast::<u32>(), 24).to_vec();
                back.Unmap(0, None);
                let want: Vec<u32> = input
                    .iter()
                    .copied()
                    .chain(input.iter().map(|v| v.wrapping_add(1)))
                    .chain((0..8u32).map(|k| k * 3 + 1))
                    .collect();
                let wrong: Vec<String> = (0..24)
                    .filter(|&i| got[i] != want[i])
                    .map(|i| format!("[{i}] {:#010x} for {:#010x}", got[i], want[i]))
                    .collect();
                println!("{name}: {} of 24 wrong {wrong:?}", wrong.len());
            }
        }
    }
}

#[cfg(test)]
mod group_size_tests {
    use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_12_0;
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
    use windows::core::Interface;

    /// Dependent loads, as the title's heavy compute programs chain them:
    /// each thread follows `STEPS` links of a random cycle through `chain`,
    /// with `REGS` live values kept across the walk (register pressure).
    fn source(threads: u32, regs: u32) -> String {
        format!(
            r#"StructuredBuffer<uint> chain : register(t0);
            RWStructuredBuffer<uint> sink : register(u0);
            [RootSignature("DescriptorTable(SRV(t0), UAV(u0))")]
            [numthreads({threads}, 1, 1)]
            void main(uint id : SV_DispatchThreadID) {{
                uint at = (id * 2654435761u) & ((1u << 18) - 1u);
                float acc[{regs}];
                [unroll] for (uint r = 0; r < {regs}; r++) acc[r] = float(r + id);
                for (uint s = 0; s < 64; s++) {{
                    at = chain[at];
                    [unroll] for (uint r = 0; r < {regs}; r++) acc[r] = acc[r] * 1.0001f + float(at & 7u);
                }}
                float t = 0;
                [unroll] for (uint r = 0; r < {regs}; r++) t += acc[r];
                if (t == 12345.0f) sink[0] = at;
            }}"#
        )
    }

    /// Manual: how the card forms its 32-lane waves from a 2-D thread group
    /// (the translator's `console_thread` assumes wave-aligned runs of flat
    /// ids; fxceless PR #1261 found 16x2 tiles on RDNA in wave32). Prints,
    /// for groups of 32x2, 16x4 and 8x8, each thread's wave and lane by its
    /// flat id. `cargo test -p victory-gpu --lib wave_layout -- --ignored --nocapture`
    #[test]
    #[ignore = "manual: reads the real card's wave layout"]
    fn wave_layout_of_2d_groups() {
        let dxc = victory_shader::emit::validator::Validator::default_path().unwrap().with_file_name("dxc.exe");
        let dir = std::env::temp_dir().join(format!("victory-wave-layout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (x, y) in [(32u32, 2u32), (16, 4), (8, 8)] {
            let src = format!(
                r#"RWStructuredBuffer<uint> dst : register(u0);
                [RootSignature("DescriptorTable(UAV(u0))")]
                [numthreads({x}, {y}, 1)]
                void main(uint flat : SV_GroupIndex) {{
                    uint first = WaveReadLaneFirst(flat);
                    dst[flat] = (first << 8) | WaveGetLaneIndex();
                }}"#
            );
            let (hlsl, outp) = (dir.join(format!("w{x}x{y}.hlsl")), dir.join(format!("w{x}x{y}.dxil")));
            std::fs::write(&hlsl, src).unwrap();
            let ok = std::process::Command::new(&dxc)
                .args(["-nologo", "-T", "cs_6_5", "-E", "main", "-Fo"])
                .arg(&outp)
                .arg(&hlsl)
                .status()
                .unwrap()
                .success();
            assert!(ok, "dxc failed");
            let cs = std::fs::read(&outp).unwrap();
            // SAFETY: plain objects of our own on the default adapter; the
            // submission is waited for.
            let got = unsafe { run_one_group(&cs, 64) };
            let rows: Vec<String> = got.iter().map(|v| format!("{}:{}", v >> 8, v & 0xFF)).collect();
            let linear = got.iter().enumerate().all(|(i, v)| (v >> 8) == (i as u32 & !31) && (v & 0xFF) == i as u32 % 32);
            println!("group {x}x{y}: waves follow flat ids (linear): {linear}");
            println!("  flat -> first:lane  {}", rows.join(" "));
        }
    }

    /// Runs `cs` (one UAV of `n` dwords at u0, root table 0) for one group
    /// and returns the dwords.
    unsafe fn run_one_group(cs: &[u8], n: u32) -> Vec<u32> {
        // SAFETY: as the caller's.
        unsafe {
            let mut device: Option<ID3D12Device> = None;
            D3D12CreateDevice(None, D3D_FEATURE_LEVEL_12_0, &mut device).unwrap();
            let device = device.unwrap();
            let root: ID3D12RootSignature = device.CreateRootSignature(0, cs).unwrap();
            let pso: ID3D12PipelineState = device
                .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                    pRootSignature: std::mem::transmute_copy(&root),
                    CS: D3D12_SHADER_BYTECODE { pShaderBytecode: cs.as_ptr().cast(), BytecodeLength: cs.len() },
                    ..Default::default()
                })
                .unwrap();
            let queue: ID3D12CommandQueue = device
                .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() })
                .unwrap();
            let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).unwrap();
            let event = CreateEventW(None, false, false, None).unwrap();
            let make = |heap, flags, state, bytes: u64| {
                let mut r: Option<ID3D12Resource> = None;
                device
                    .CreateCommittedResource(
                        &D3D12_HEAP_PROPERTIES { Type: heap, ..Default::default() },
                        D3D12_HEAP_FLAG_NONE,
                        &D3D12_RESOURCE_DESC {
                            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                            Width: bytes,
                            Height: 1,
                            DepthOrArraySize: 1,
                            MipLevels: 1,
                            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                            Flags: flags,
                            ..Default::default()
                        },
                        state,
                        None,
                        &mut r,
                    )
                    .unwrap();
                r.unwrap()
            };
            let bytes = u64::from(n) * 4;
            let target = make(D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COMMON, bytes);
            let back = make(D3D12_HEAP_TYPE_READBACK, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COPY_DEST, bytes);
            let heap: ID3D12DescriptorHeap = device
                .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                    NumDescriptors: 1,
                    Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                    NodeMask: 0,
                })
                .unwrap();
            device.CreateUnorderedAccessView(
                &target,
                None::<&ID3D12Resource>,
                Some(&D3D12_UNORDERED_ACCESS_VIEW_DESC {
                    Format: DXGI_FORMAT_UNKNOWN,
                    ViewDimension: D3D12_UAV_DIMENSION_BUFFER,
                    Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
                        Buffer: D3D12_BUFFER_UAV { FirstElement: 0, NumElements: n, StructureByteStride: 4, ..Default::default() },
                    },
                }),
                heap.GetCPUDescriptorHandleForHeapStart(),
            );
            let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT).unwrap();
            let l: ID3D12GraphicsCommandList = device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, &pso).unwrap();
            l.SetDescriptorHeaps(&[Some(heap.clone())]);
            l.SetComputeRootSignature(&root);
            l.SetComputeRootDescriptorTable(0, heap.GetGPUDescriptorHandleForHeapStart());
            l.Dispatch(1, 1, 1);
            l.ResourceBarrier(&[D3D12_RESOURCE_BARRIER {
                Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
                Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
                Anonymous: D3D12_RESOURCE_BARRIER_0 {
                    Transition: std::mem::ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                        pResource: std::mem::transmute_copy(&target),
                        Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                        StateBefore: D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                        StateAfter: D3D12_RESOURCE_STATE_COPY_SOURCE,
                    }),
                },
            }]);
            l.CopyResource(&back, &target);
            l.Close().unwrap();
            queue.ExecuteCommandLists(&[l.cast().ok()]);
            queue.Signal(&fence, 1).unwrap();
            fence.SetEventOnCompletion(1, event).unwrap();
            WaitForSingleObject(event, INFINITE);
            let mut d = std::ptr::null_mut();
            back.Map(0, Some(&D3D12_RANGE { Begin: 0, End: bytes as usize }), Some(&mut d)).unwrap();
            let got = std::slice::from_raw_parts(d.cast::<u32>(), n as usize).to_vec();
            back.Unmap(0, None);
            got
        }
    }

    /// Manual: the card's time for the same dependent-load work in thread
    /// groups of 32 (one warp: at most 24 resident groups, 24 of 48 warps,
    /// per Ada SM) and of 64 threads, at a few register loads.
    /// `cargo test -p victory-gpu --lib group_size -- --ignored --nocapture`
    #[test]
    #[ignore = "manual: measures the real card"]
    fn one_warp_groups_against_two_warp_groups() {
        let dxc = victory_shader::emit::validator::Validator::default_path().unwrap().with_file_name("dxc.exe");
        let dir = std::env::temp_dir().join(format!("victory-group-size-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let compile = |threads: u32, regs: u32| {
            let (hlsl, out) = (dir.join(format!("g{threads}r{regs}.hlsl")), dir.join(format!("g{threads}r{regs}.dxil")));
            std::fs::write(&hlsl, source(threads, regs)).unwrap();
            let ok = std::process::Command::new(&dxc)
                .args(["-nologo", "-T", "cs_6_5", "-E", "main", "-O3", "-Fo"])
                .arg(&out)
                .arg(&hlsl)
                .status()
                .unwrap()
                .success();
            assert!(ok, "dxc failed");
            std::fs::read(&out).unwrap()
        };
        const LINKS: u64 = 1 << 18;
        const THREADS: u32 = 1 << 20;
        // SAFETY: plain objects of our own on the default adapter; every
        // submission is waited for; the walk stays inside the chain.
        unsafe {
            let mut device: Option<ID3D12Device> = None;
            D3D12CreateDevice(None, D3D_FEATURE_LEVEL_12_0, &mut device).unwrap();
            let device = device.unwrap();
            let queue: ID3D12CommandQueue = device
                .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() })
                .unwrap();
            let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).unwrap();
            let event = CreateEventW(None, false, false, None).unwrap();
            let buffer = |heap, flags, state, bytes: u64| {
                let mut r: Option<ID3D12Resource> = None;
                device
                    .CreateCommittedResource(
                        &D3D12_HEAP_PROPERTIES { Type: heap, ..Default::default() },
                        D3D12_HEAP_FLAG_NONE,
                        &D3D12_RESOURCE_DESC {
                            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                            Width: bytes,
                            Height: 1,
                            DepthOrArraySize: 1,
                            MipLevels: 1,
                            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                            Flags: flags,
                            ..Default::default()
                        },
                        state,
                        None,
                        &mut r,
                    )
                    .unwrap();
                r.unwrap()
            };
            // A random cycle over 2^18 links (1 MB: inside the L2, as the title's
            // chained reads are: it uses 4 % of the DRAM's bandwidth).
            let mut order: Vec<u32> = (0..LINKS as u32).collect();
            let mut seed = 0x9E37_79B9u64;
            for i in (1..order.len()).rev() {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                order.swap(i, (seed % (i as u64 + 1)) as usize);
            }
            let mut chain = vec![0u32; LINKS as usize];
            for w in 0..order.len() {
                chain[order[w] as usize] = order[(w + 1) % order.len()];
            }
            let upload = buffer(D3D12_HEAP_TYPE_UPLOAD, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_GENERIC_READ, LINKS * 4);
            let mut p = std::ptr::null_mut();
            upload.Map(0, None, Some(&mut p)).unwrap();
            std::ptr::copy_nonoverlapping(chain.as_ptr(), p.cast::<u32>(), chain.len());
            upload.Unmap(0, None);
            let links = buffer(D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COMMON, LINKS * 4);
            let sink = buffer(D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COMMON, 256);
            let heap: ID3D12DescriptorHeap = device
                .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                    NumDescriptors: 2,
                    Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                    NodeMask: 0,
                })
                .unwrap();
            let step = device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV) as usize;
            let cpu = heap.GetCPUDescriptorHandleForHeapStart();
            device.CreateShaderResourceView(
                &links,
                Some(&D3D12_SHADER_RESOURCE_VIEW_DESC {
                    Format: DXGI_FORMAT_UNKNOWN,
                    ViewDimension: D3D12_SRV_DIMENSION_BUFFER,
                    Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                    Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                        Buffer: D3D12_BUFFER_SRV { FirstElement: 0, NumElements: LINKS as u32, StructureByteStride: 4, Flags: D3D12_BUFFER_SRV_FLAG_NONE },
                    },
                }),
                cpu,
            );
            device.CreateUnorderedAccessView(
                &sink,
                None::<&ID3D12Resource>,
                Some(&D3D12_UNORDERED_ACCESS_VIEW_DESC {
                    Format: DXGI_FORMAT_UNKNOWN,
                    ViewDimension: D3D12_UAV_DIMENSION_BUFFER,
                    Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
                        Buffer: D3D12_BUFFER_UAV { FirstElement: 0, NumElements: 64, StructureByteStride: 4, ..Default::default() },
                    },
                }),
                D3D12_CPU_DESCRIPTOR_HANDLE { ptr: cpu.ptr + step },
            );
            let stamps: ID3D12QueryHeap = {
                let mut q: Option<ID3D12QueryHeap> = None;
                device.CreateQueryHeap(&D3D12_QUERY_HEAP_DESC { Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP, Count: 2, NodeMask: 0 }, &mut q).unwrap();
                q.unwrap()
            };
            let back = buffer(D3D12_HEAP_TYPE_READBACK, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COPY_DEST, 16);
            let frequency = queue.GetTimestampFrequency().unwrap() as f64;
            let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT).unwrap();
            let mut fenced = 0u64;
            let mut run = |list: &ID3D12GraphicsCommandList| {
                list.Close().unwrap();
                queue.ExecuteCommandLists(&[list.cast().ok()]);
                fenced += 1;
                queue.Signal(&fence, fenced).unwrap();
                fence.SetEventOnCompletion(fenced, event).unwrap();
                WaitForSingleObject(event, INFINITE);
            };
            {
                allocator.Reset().unwrap();
                let l: ID3D12GraphicsCommandList = device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None).unwrap();
                l.CopyResource(&links, &upload);
                run(&l);
            }
            for regs in [8u32, 32, 64] {
                for threads in [32u32, 64] {
                    let cs = compile(threads, regs);
                    let root: ID3D12RootSignature = device.CreateRootSignature(0, &cs).unwrap();
                    let pso: ID3D12PipelineState = device
                        .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                            pRootSignature: std::mem::transmute_copy(&root),
                            CS: D3D12_SHADER_BYTECODE { pShaderBytecode: cs.as_ptr().cast(), BytecodeLength: cs.len() },
                            ..Default::default()
                        })
                        .unwrap();
                    let mut best = f64::MAX;
                    for _ in 0..3 {
                        allocator.Reset().unwrap();
                        let l: ID3D12GraphicsCommandList =
                            device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, &pso).unwrap();
                        l.SetDescriptorHeaps(&[Some(heap.clone())]);
                        l.SetComputeRootSignature(&root);
                        l.SetComputeRootDescriptorTable(0, heap.GetGPUDescriptorHandleForHeapStart());
                        l.EndQuery(&stamps, D3D12_QUERY_TYPE_TIMESTAMP, 0);
                        l.Dispatch(THREADS / threads, 1, 1);
                        l.EndQuery(&stamps, D3D12_QUERY_TYPE_TIMESTAMP, 1);
                        l.ResolveQueryData(&stamps, D3D12_QUERY_TYPE_TIMESTAMP, 0, 2, &back, 0);
                        run(&l);
                        let mut d = std::ptr::null_mut();
                        back.Map(0, Some(&D3D12_RANGE { Begin: 0, End: 16 }), Some(&mut d)).unwrap();
                        let t = std::slice::from_raw_parts(d.cast::<u64>(), 2);
                        best = best.min(t[1].saturating_sub(t[0]) as f64 / frequency * 1e3);
                        back.Unmap(0, None);
                    }
                    println!("{regs:3} live floats, groups of {threads:2} threads: {best:7.2} ms for {THREADS} threads x 64 dependent loads");
                }
            }
        }
    }
}

#[cfg(test)]
mod lane_mask_tests {
    use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_12_0;
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
    use windows::core::Interface;

    /// How a program keeps `conds` lane conditions across a loop: as the
    /// translator keeps the console's lane masks (one boolean per lane), as
    /// wave ballots (one uniform word per mask, as the console's SGPRs), or
    /// packed as bits of one word per 32 conditions in each lane.
    #[derive(Clone, Copy, Debug)]
    enum Keep {
        Bools,
        Ballots,
        Packed,
    }

    /// A wave-uniform walk of dependent loads (one sector a step: latency,
    /// not bandwidth, as the title's shadow masks run), with `floats` live
    /// values and `conds` lane conditions kept across it. A never-taken,
    /// wave-uniform branch in the loop rewrites them all, so the compiler
    /// keeps every one live through the loop instead of redoing it after.
    fn source(floats: u32, conds: u32, keep: Keep) -> String {
        // One scalar per condition (an array of `bool` is kept as 32-bit
        // words; the translator's lane conditions are `i1` values).
        // Each condition comes from a lane's own walk, one link after the
        // other, so the reads cannot all be in flight (and in registers)
        // at once: they arrive one by one, as a program's compares do.
        let (mut def, mut flip, mut read) = (String::from("uint q = at;\n"), String::new(), String::new());
        let load = "((q & 1u) != 0)";
        for i in 0..conds {
            def += &format!("q = chain[(q + lane * 3u + {}u) & mask];\n", i * 13);
            match keep {
                Keep::Bools => {
                    def += &format!("bool c{i} = {};\n", load);
                    flip += &format!("c{i} = !c{i};\n");
                    read += &format!("if (c{i}) t += {i}.0f;\n");
                }
                Keep::Ballots => {
                    def += &format!("uint c{i} = WaveActiveBallot({}).x;\n", load);
                    flip += &format!("c{i} = ~c{i};\n");
                    read += &format!("if (((c{i} >> lane) & 1u) != 0) t += {i}.0f;\n");
                }
                Keep::Packed => {
                    if i % 32 == 0 {
                        def += &format!("uint p{} = 0u;\n", i / 32);
                        flip += &format!("p{0} = ~p{0};\n", i / 32);
                    }
                    def += &format!("p{} |= ({} ? 1u : 0u) << {}u;\n", i / 32, load, i % 32);
                    read += &format!("if (((p{} >> {}u) & 1u) != 0) t += {i}.0f;\n", i / 32, i % 32);
                }
            }
        }
        format!(
            r#"StructuredBuffer<uint> chain : register(t0);
            RWStructuredBuffer<uint> sink : register(u0);
            [RootSignature("DescriptorTable(SRV(t0), UAV(u0))")]
            [WaveSize(32)]
            [numthreads(32, 1, 1)]
            void main(uint3 group : SV_GroupID, uint lane : SV_GroupIndex) {{
                const uint mask = (1u << 18) - 1u;
                uint at = (group.x * 40503u) & mask;
                float acc[{floats}];
                [unroll] for (uint r = 0; r < {floats}; r++) acc[r] = asfloat((chain[(at * 7u + r * 97u + lane) & mask] & 0x007fffffu) | 0x3f800000u);
                {def}
                [loop] for (uint s = 0; s < 2048u; s++) {{
                    at = chain[at];
                    if (WaveReadLaneFirst(at) == 0xffffffffu) {{
                        [unroll] for (uint r = 0; r < {floats}; r++) acc[r] += 1.0f;
                        {flip}
                    }}
                }}
                float t = 0;
                [unroll] for (uint r = 0; r < {floats}; r++) t += acc[r];
                {read}
                if (t == 12345.0f) sink[lane] = at;
            }}"#
        )
    }

    /// Manual: the card's time for the same latency-bound walk with 48 lane
    /// conditions kept as booleans, ballots or packed bits, next to the walk
    /// with only its floats (the registers' cost, read as occupancy).
    /// `cargo test -p victory-gpu --lib lane_mask -- --ignored --nocapture`
    #[test]
    #[ignore = "manual: measures the real card"]
    fn lane_conditions_kept_as_booleans_ballots_or_bits() {
        let dxc = victory_shader::emit::validator::Validator::default_path().unwrap().with_file_name("dxc.exe");
        let dir = std::env::temp_dir().join(format!("victory-lane-mask-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let compile = |name: &str, text: String| {
            let (hlsl, out) = (dir.join(format!("{name}.hlsl")), dir.join(format!("{name}.dxil")));
            std::fs::write(&hlsl, text).unwrap();
            let ok = std::process::Command::new(&dxc)
                .args(["-nologo", "-T", "cs_6_6", "-E", "main", "-O3", "-Fo"])
                .arg(&out)
                .arg(&hlsl)
                .status()
                .unwrap()
                .success();
            assert!(ok, "dxc failed on {name}");
            std::fs::read(&out).unwrap()
        };
        const LINKS: u64 = 1 << 18;
        const GROUPS: u32 = 1 << 15;
        // SAFETY: plain objects of our own on the default adapter; every
        // submission is waited for; every read stays inside the chain.
        unsafe {
            let mut device: Option<ID3D12Device> = None;
            D3D12CreateDevice(None, D3D_FEATURE_LEVEL_12_0, &mut device).unwrap();
            let device = device.unwrap();
            let queue: ID3D12CommandQueue = device
                .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() })
                .unwrap();
            let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).unwrap();
            let event = CreateEventW(None, false, false, None).unwrap();
            let buffer = |heap, flags, state, bytes: u64| {
                let mut r: Option<ID3D12Resource> = None;
                device
                    .CreateCommittedResource(
                        &D3D12_HEAP_PROPERTIES { Type: heap, ..Default::default() },
                        D3D12_HEAP_FLAG_NONE,
                        &D3D12_RESOURCE_DESC {
                            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                            Width: bytes,
                            Height: 1,
                            DepthOrArraySize: 1,
                            MipLevels: 1,
                            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                            Flags: flags,
                            ..Default::default()
                        },
                        state,
                        None,
                        &mut r,
                    )
                    .unwrap();
                r.unwrap()
            };
            // A random cycle over 2^18 links (1 MB: in the L2, as most of the
            // shadow masks' reads are).
            let mut order: Vec<u32> = (0..LINKS as u32).collect();
            let mut seed = 0x9E37_79B9u64;
            for i in (1..order.len()).rev() {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                order.swap(i, (seed % (i as u64 + 1)) as usize);
            }
            let mut chain = vec![0u32; LINKS as usize];
            for w in 0..order.len() {
                chain[order[w] as usize] = order[(w + 1) % order.len()];
            }
            let upload = buffer(D3D12_HEAP_TYPE_UPLOAD, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_GENERIC_READ, LINKS * 4);
            let mut p = std::ptr::null_mut();
            upload.Map(0, None, Some(&mut p)).unwrap();
            std::ptr::copy_nonoverlapping(chain.as_ptr(), p.cast::<u32>(), chain.len());
            upload.Unmap(0, None);
            let links = buffer(D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COMMON, LINKS * 4);
            let sink = buffer(D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COMMON, 256);
            let heap: ID3D12DescriptorHeap = device
                .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                    NumDescriptors: 2,
                    Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                    NodeMask: 0,
                })
                .unwrap();
            let step = device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV) as usize;
            let cpu = heap.GetCPUDescriptorHandleForHeapStart();
            device.CreateShaderResourceView(
                &links,
                Some(&D3D12_SHADER_RESOURCE_VIEW_DESC {
                    Format: DXGI_FORMAT_UNKNOWN,
                    ViewDimension: D3D12_SRV_DIMENSION_BUFFER,
                    Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                    Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                        Buffer: D3D12_BUFFER_SRV { FirstElement: 0, NumElements: LINKS as u32, StructureByteStride: 4, Flags: D3D12_BUFFER_SRV_FLAG_NONE },
                    },
                }),
                cpu,
            );
            device.CreateUnorderedAccessView(
                &sink,
                None::<&ID3D12Resource>,
                Some(&D3D12_UNORDERED_ACCESS_VIEW_DESC {
                    Format: DXGI_FORMAT_UNKNOWN,
                    ViewDimension: D3D12_UAV_DIMENSION_BUFFER,
                    Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
                        Buffer: D3D12_BUFFER_UAV { FirstElement: 0, NumElements: 64, StructureByteStride: 4, ..Default::default() },
                    },
                }),
                D3D12_CPU_DESCRIPTOR_HANDLE { ptr: cpu.ptr + step },
            );
            let stamps: ID3D12QueryHeap = {
                let mut q: Option<ID3D12QueryHeap> = None;
                device.CreateQueryHeap(&D3D12_QUERY_HEAP_DESC { Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP, Count: 2, NodeMask: 0 }, &mut q).unwrap();
                q.unwrap()
            };
            let back = buffer(D3D12_HEAP_TYPE_READBACK, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COPY_DEST, 16);
            let frequency = queue.GetTimestampFrequency().unwrap() as f64;
            let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT).unwrap();
            let mut fenced = 0u64;
            let mut run = |list: &ID3D12GraphicsCommandList| {
                list.Close().unwrap();
                queue.ExecuteCommandLists(&[list.cast().ok()]);
                fenced += 1;
                queue.Signal(&fence, fenced).unwrap();
                fence.SetEventOnCompletion(fenced, event).unwrap();
                WaitForSingleObject(event, INFINITE);
            };
            {
                allocator.Reset().unwrap();
                let l: ID3D12GraphicsCommandList = device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None).unwrap();
                l.CopyResource(&links, &upload);
                run(&l);
            }
            let mut cases: Vec<(String, String)> = Vec::new();
            for floats in [16u32, 48, 64, 80, 96] {
                cases.push((format!("{floats:3} floats, no conditions          "), source(floats, 0, Keep::Bools)));
            }
            for keep in [Keep::Bools, Keep::Ballots, Keep::Packed] {
                cases.push((format!(" 64 floats + 48 conditions as {keep:8?}"), source(64, 48, keep)));
            }
            // All built first, so the timed submissions follow one another
            // (a GPU trace of the test, `--start-after-submits 1`, holds them
            // all in a short window).
            let built: Vec<(String, ID3D12RootSignature, ID3D12PipelineState)> = cases
                .into_iter()
                .enumerate()
                .map(|(n, (name, text))| {
                    let cs = compile(&format!("case{n}"), text);
                    let root: ID3D12RootSignature = device.CreateRootSignature(0, &cs).unwrap();
                    let pso: ID3D12PipelineState = device
                        .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                            pRootSignature: std::mem::transmute_copy(&root),
                            CS: D3D12_SHADER_BYTECODE { pShaderBytecode: cs.as_ptr().cast(), BytecodeLength: cs.len() },
                            ..Default::default()
                        })
                        .unwrap();
                    (name, root, pso)
                })
                .collect();
            for (name, root, pso) in built {
                let mut best = f64::MAX;
                for _ in 0..3 {
                    allocator.Reset().unwrap();
                    let l: ID3D12GraphicsCommandList = device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, &pso).unwrap();
                    l.SetDescriptorHeaps(&[Some(heap.clone())]);
                    l.SetComputeRootSignature(&root);
                    l.SetComputeRootDescriptorTable(0, heap.GetGPUDescriptorHandleForHeapStart());
                    l.EndQuery(&stamps, D3D12_QUERY_TYPE_TIMESTAMP, 0);
                    l.Dispatch(GROUPS, 1, 1);
                    l.EndQuery(&stamps, D3D12_QUERY_TYPE_TIMESTAMP, 1);
                    l.ResolveQueryData(&stamps, D3D12_QUERY_TYPE_TIMESTAMP, 0, 2, &back, 0);
                    run(&l);
                    let mut d = std::ptr::null_mut();
                    back.Map(0, Some(&D3D12_RANGE { Begin: 0, End: 16 }), Some(&mut d)).unwrap();
                    let t = std::slice::from_raw_parts(d.cast::<u64>(), 2);
                    best = best.min(t[1].saturating_sub(t[0]) as f64 / frequency * 1e3);
                    back.Unmap(0, None);
                }
                println!("{name}: {best:7.2} ms ({GROUPS} one-warp groups x 2048 dependent loads)");
            }
        }
    }
}
