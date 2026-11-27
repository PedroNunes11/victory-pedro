use super::*;

fn compile_fxc(src: &str, profile: &CStr) -> Vec<u8> {
    use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: static source and null-terminated profile string.
    let compiled = unsafe {
        D3DCompile(
            src.as_ptr().cast(),
            src.len(),
            PCSTR(c"pso_test".as_ptr().cast()),
            None,
            None,
            PCSTR(c"main".as_ptr().cast()),
            PCSTR(profile.as_ptr().cast()),
            0,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = compiled {
        let msg = errors
            .map(|b| {
                // SAFETY: error blob bytes.
                let bytes = unsafe {
                    std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize())
                };
                String::from_utf8_lossy(bytes).into_owned()
            })
            .unwrap_or_default();
        panic!("D3DCompile failed ({e}): {msg}");
    }
    let blob = code.expect("compiled shader");
    // SAFETY: compiled DXBC blob bytes.
    unsafe {
        std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize())
    }
    .to_vec()
}

fn make_rt_tex(host: &crate::host::Host, format: DXGI_FORMAT) -> ID3D12Resource {
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
        Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
        ..Default::default()
    };
    let mut out: Option<ID3D12Resource> = None;
    // SAFETY: valid heap and texture description.
    unsafe {
        host.device
            .CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_NONE,
                &rd,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                None,
                &mut out,
            )
            .expect("render target texture");
    }
    out.unwrap()
}

fn readback_pixel(
    host: &crate::host::Host,
    list: &crate::host::HostList,
    tex: &ID3D12Resource,
    format: DXGI_FORMAT,
) -> ID3D12Resource {
    let readback = host.read_back_buffer(1024).expect("readback buffer");
    // SAFETY: open list, live render target and readback buffer.
    unsafe {
        list.list.ResourceBarrier(&[crate::host::transition(
            tex,
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
                        Format: format,
                        Width: 4,
                        Height: 4,
                        Depth: 1,
                        RowPitch: 256,
                    },
                },
            },
        };
        let src = D3D12_TEXTURE_COPY_LOCATION {
            pResource: std::mem::transmute_copy(tex),
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                SubresourceIndex: 0,
            },
        };
        list.list.CopyTextureRegion(&dst, 0, 0, 0, &src, None);
        list.list.ResourceBarrier(&[crate::host::transition(
            tex,
            D3D12_RESOURCE_STATE_COPY_SOURCE,
            D3D12_RESOURCE_STATE_RENDER_TARGET,
        )]);
    }
    readback
}

fn map_first_byte(readback: &ID3D12Resource) -> u8 {
    let mut ptr = std::ptr::null_mut();
    // SAFETY: submission finished; buffer holds at least 1024 bytes.
    unsafe {
        readback.Map(0, None, Some(&mut ptr)).expect("map readback");
        let v = *ptr.cast::<u8>();
        readback.Unmap(0, None);
        v
    }
}

fn make_graphics_pso(
    device: &ID3D12Device,
    root: &ID3D12RootSignature,
    vs: &[u8],
    ps: &[u8],
    g: &GraphicsState,
) -> windows::core::Result<ID3D12PipelineState> {
    let desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
        // SAFETY: borrowed pointer, never released by the descriptor.
        pRootSignature: unsafe { std::mem::transmute_copy(root) },
        VS: code(vs),
        PS: code(ps),
        BlendState: host_blend(&g.blend, &g.targets),
        SampleMask: g.sample_mask,
        RasterizerState: g.rasterizer,
        DepthStencilState: g.depth,
        IBStripCutValue: g.strip_cut,
        PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
        NumRenderTargets: g.targets.NumRenderTargets,
        RTVFormats: g.targets.RTFormats,
        DSVFormat: g.depth_format,
        SampleDesc: g.samples,
        ..Default::default()
    };
    // SAFETY: live root signature and bytecode slices.
    unsafe { device.CreateGraphicsPipelineState(&desc) }
}

// Each draw covers a cleared 4x4 target. Read the first texel of every
// attachment only after the host has completed the command list.
fn draw(host: &crate::host::Host, g: &GraphicsState, ps: &str, draws: usize) -> Vec<u8> {
    let vs = compile_fxc(VS, c"vs_5_1");
    let ps = compile_fxc(ps, c"ps_5_1");
    let (root, _) = root_signature(&host.device, &[], &[], false).expect("root signature");
    let pso = make_graphics_pso(&host.device, &root, &vs, &ps, g).expect("graphics PSO");
    let formats = &g.targets.RTFormats[..g.targets.NumRenderTargets as usize];
    let textures: Vec<_> = formats.iter().map(|&f| make_rt_tex(host, f)).collect();
    let rtvs: Vec<_> = textures
        .iter()
        .map(|tex| host.rtv(Some(tex), None))
        .collect();
    let list = host.new_list().expect("command list");
    let viewport = D3D12_VIEWPORT {
        Width: 4.0,
        Height: 4.0,
        MaxDepth: 1.0,
        ..Default::default()
    };
    let rect = windows::Win32::Foundation::RECT {
        left: 0,
        top: 0,
        right: 4,
        bottom: 4,
    };
    // SAFETY: open list, live resources, and one RTV for each PSO target.
    unsafe {
        for &rtv in &rtvs {
            list.list.ClearRenderTargetView(rtv, &[0.0; 4], None);
        }
        list.list
            .OMSetRenderTargets(rtvs.len() as u32, Some(rtvs.as_ptr()), false, None);
        list.list.RSSetViewports(&[viewport]);
        list.list.RSSetScissorRects(&[rect]);
        list.list.SetGraphicsRootSignature(&root);
        list.list.SetPipelineState(&pso);
        list.list.IASetPrimitiveTopology(
            windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
        );
        for _ in 0..draws {
            list.list.DrawInstanced(3, 1, 0, 0);
        }
    }
    let readbacks: Vec<_> = textures
        .iter()
        .zip(formats)
        .map(|(tex, &format)| readback_pixel(host, &list, tex, format))
        .collect();
    list.close();
    host.execute(&[&list]);
    readbacks.iter().map(map_first_byte).collect()
}

#[test]
fn logic_op_draws_to_single_and_mixed_integer_targets() {
    let Some(host) = host() else {
        eprintln!("no D3D12 device; skipping pixel readback");
        return;
    };
    let mut g = single_color_target(DXGI_FORMAT_R8_UINT);
    g.rasterizer.CullMode = D3D12_CULL_MODE_NONE;
    g.blend = logic_op(false);
    // CLEAR makes any logic operation accidentally left on visible in the pixel.
    g.blend.RenderTarget[0].LogicOp = D3D12_LOGIC_OP_CLEAR;
    assert_eq!(
        draw(host, &g, "uint main() : SV_Target { return 42u; }", 1),
        [42]
    );

    g.targets = targets(&[DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R8_UINT]);
    let ps = "struct Out { float4 c : SV_Target0; uint m : SV_Target1; };
        Out main() { Out o; o.c = float4(1, 0, 0, 1); o.m = 77u; return o; }";
    assert_eq!(draw(host, &g, ps, 1), [255, 77]);
}

#[test]
fn stale_rt_formats_still_draw_to_the_active_target() {
    let Some(host) = host() else {
        eprintln!("no D3D12 device; skipping pixel readback");
        return;
    };
    #[repr(C, align(8))]
    struct Sub15(u32, D3D12_RT_FORMAT_ARRAY);
    let mut g = single_color_target(DXGI_FORMAT_R8G8B8A8_UNORM);
    g.rasterizer.CullMode = D3D12_CULL_MODE_NONE;
    let mut stream = Sub15(15, g.targets);
    stream.1.RTFormats[1] = DXGI_FORMAT_R8_UINT;
    stream.1.RTFormats[2] = DXGI_FORMAT_R16G16B16A16_FLOAT;
    // SAFETY: stream is a complete subobject 15.
    unsafe { g.read(15, (&raw const stream).cast()) };
    assert_eq!(g.targets, targets(&[DXGI_FORMAT_R8G8B8A8_UNORM]));
    assert_eq!(draw(host, &g, PS, 1), [255]);
}

#[test]
fn two_draws_add_only_on_the_unorm_target() {
    let Some(host) = host() else {
        eprintln!("no D3D12 device; skipping pixel readback");
        return;
    };
    let mut g = single_color_target(DXGI_FORMAT_R8G8B8A8_UNORM);
    g.rasterizer.CullMode = D3D12_CULL_MODE_NONE;
    g.targets = targets(&[DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R8_UINT]);
    let first = &mut g.blend.RenderTarget[0];
    first.BlendEnable = true.into();
    first.DestBlend = D3D12_BLEND_ONE;
    first.LogicOpEnable = true.into();
    first.LogicOp = D3D12_LOGIC_OP_CLEAR;
    let ps = "struct Out { float4 c : SV_Target0; uint m : SV_Target1; };
        Out main() { Out o; o.c = float4(0.25, 0, 0, 1); o.m = 9u; return o; }";
    let pixels = draw(host, &g, ps, 2);
    assert!(
        (126..=129).contains(&pixels[0]),
        "two additive draws produced {}",
        pixels[0]
    );
    assert_eq!(pixels[1], 9, "integer target must keep its unblended value");
}
