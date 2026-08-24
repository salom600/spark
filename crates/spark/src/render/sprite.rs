//! 2D sprite pass: instanced textured quads through one pipeline.
//!
//! The unit quad lives in a static vertex buffer; per-sprite transforms,
//! tint and size ride the instance buffer. Sprites are depth-*tested*
//! against 3D meshes but never write depth, so 2D layers compose via z.

use wgpu::util::DeviceExt;

use super::SpriteInstance;

pub struct SpritePass {
    pipeline: wgpu::RenderPipeline,
    mat_bgl: wgpu::BindGroupLayout,
    quad: wgpu::Buffer,
    instance_buf: Option<wgpu::Buffer>,
    instance_cap: u64,
}

/// Quad vertex: position (unit square centered at origin) + uv.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct QuadVert {
    pos: [f32; 2],
    uv: [f32; 2],
}

const QUAD: [QuadVert; 6] = [
    QuadVert { pos: [-0.5, -0.5], uv: [0.0, 1.0] },
    QuadVert { pos: [0.5, -0.5], uv: [1.0, 1.0] },
    QuadVert { pos: [0.5, 0.5], uv: [1.0, 0.0] },
    QuadVert { pos: [-0.5, -0.5], uv: [0.0, 1.0] },
    QuadVert { pos: [0.5, 0.5], uv: [1.0, 0.0] },
    QuadVert { pos: [-0.5, 0.5], uv: [0.0, 0.0] },
];

impl SpritePass {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, globals_bgl: &wgpu::BindGroupLayout) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("shaders/sprite.wgsl"));
        let mat_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("spark.sprite.mat"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("spark.sprite.pl"),
            bind_group_layouts: &[globals_bgl, &mat_bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("spark.sprite.pipe"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<QuadVert>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
                    },
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<SpriteInstance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &wgpu::vertex_attr_array![
                            2 => Float32x3, 3 => Float32, 4 => Float32x2, 5 => Float32x4
                        ],
                    },
                ],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        let quad = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("spark.sprite.quad"),
            contents: bytemuck::cast_slice(&QUAD),
            usage: wgpu::BufferUsages::VERTEX,
        });
        Self { pipeline, mat_bgl, quad, instance_buf: None, instance_cap: 0 }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rpass: &mut wgpu::RenderPass<'_>,
        globals_bgl: &wgpu::BindGroupLayout,
        globals_buf: &wgpu::Buffer,
        sampler: &wgpu::Sampler,
        tex_view: &wgpu::TextureView,
        instances: &[u8],
    ) {
        // wgpu handles are cheaply cloneable; clone ends the &mut self borrow.
        let needed = instances.len() as u64;
        if self.instance_buf.as_ref().is_none_or(|_| self.instance_cap < needed) {
            let cap = needed.max(4096);
            self.instance_buf = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("spark.sprite.instances"),
                size: cap,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.instance_cap = cap;
        }
        let buf = self.instance_buf.as_ref().unwrap().clone();
        queue.write_buffer(&buf, 0, instances);

        let globals = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("spark.sprite.globals"),
            layout: globals_bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: globals_buf.as_entire_binding() }],
        });
        let material = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("spark.sprite.mat"),
            layout: &self.mat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(tex_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
            ],
        });

        rpass.set_pipeline(&self.pipeline);
        rpass.set_bind_group(0, &globals, &[]);
        rpass.set_bind_group(1, &material, &[]);
        rpass.set_vertex_buffer(0, self.quad.slice(..));
        rpass.set_vertex_buffer(1, buf.slice(..instances.len() as u64));
        rpass.draw(0..6, 0..(instances.len() / std::mem::size_of::<SpriteInstance>()) as u32);
    }
}
