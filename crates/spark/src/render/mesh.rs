//! 3D mesh pass: instanced PBR meshes with directional shadows.
//!
//! Two pipelines share the vertex/instance layouts:
//! * `mesh.wgsl` — Cook-Torrance-flavored PBR (one directional light with a
//!   PCF shadow map, up to 16 point lights, ambient + emissive, `unlit` flag).
//! * `shadow.wgsl` — depth-only variant used by the shadow pass.

use super::MeshInstance;
use crate::math::Vertex;

pub struct MeshPass {
    pipeline: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,
    mat_bgl: wgpu::BindGroupLayout,
    instance_buf: Option<wgpu::Buffer>,
    instance_cap: u64,
    shadow_instance_buf: Option<wgpu::Buffer>,
    shadow_instance_cap: u64,
}

const INSTANCE_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: std::mem::size_of::<MeshInstance>() as u64,
    step_mode: wgpu::VertexStepMode::Instance,
    attributes: &wgpu::vertex_attr_array![
        3 => Float32x4, 4 => Float32x4, 5 => Float32x4, 6 => Float32x4, // model
        7 => Float32x4,                                                // color
        8 => Float32x4,                                                // params
        9 => Float32x4,                                                // emissive
    ],
};

impl MeshPass {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        shadow_bgl: &wgpu::BindGroupLayout,
    ) -> Self {
        // Material bind group: albedo texture + regular sampler.
        let mat_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("spark.mesh.mat"),
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

        let mesh_shader = device.create_shader_module(wgpu::include_wgsl!("shaders/mesh.wgsl"));
        let shadow_shader = device.create_shader_module(wgpu::include_wgsl!("shaders/shadow.wgsl"));

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("spark.mesh.pl"),
            bind_group_layouts: &[shadow_bgl, &mat_bgl],
            push_constant_ranges: &[],
        });
        let shadow_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("spark.mesh.shadow.pl"),
            bind_group_layouts: &[shadow_bgl],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("spark.mesh.pipe"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &mesh_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Vertex::LAYOUT, INSTANCE_LAYOUT],
            },
            fragment: Some(wgpu::FragmentState {
                module: &mesh_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("spark.mesh.shadow.pipe"),
            layout: Some(&shadow_layout),
            vertex: wgpu::VertexState {
                module: &shadow_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Vertex::LAYOUT, INSTANCE_LAYOUT],
            },
            fragment: None,
            primitive: wgpu::PrimitiveState {
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: wgpu::DepthBiasState {
                    constant: 2,
                    slope_scale: 2.0,
                    clamp: 0.0,
                },
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        Self {
            pipeline,
            shadow_pipeline,
            mat_bgl,
            instance_buf: None,
            instance_cap: 0,
            shadow_instance_buf: None,
            shadow_instance_cap: 0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rpass: &mut wgpu::RenderPass<'_>,
        mesh: &super::GpuMesh,
        shadow_bind: &wgpu::BindGroup,
        sampler: &wgpu::Sampler,
        tex_view: &wgpu::TextureView,
        instances: &[u8],
    ) {
        // Bind group first (immutable borrow of self ends immediately).
        let material = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("spark.mesh.mat"),
            layout: &self.mat_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(tex_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        // wgpu handles are cheaply cloneable; clone ends the &mut self borrow.
        let buf = self
            .ensure_instance_buf(device, instances.len() as u64, false)
            .clone();
        queue.write_buffer(&buf, 0, instances);

        rpass.set_pipeline(&self.pipeline);
        rpass.set_bind_group(0, shadow_bind, &[]);
        rpass.set_bind_group(1, &material, &[]);
        rpass.set_vertex_buffer(0, mesh.verts.slice(..));
        rpass.set_vertex_buffer(1, buf.slice(..instances.len() as u64));
        rpass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
        let count = (instances.len() / std::mem::size_of::<MeshInstance>()) as u32;
        rpass.draw_indexed(0..mesh.count, 0, 0..count);
    }

    /// Draw the depth-only shadow variant. The caller passes the *pre-built*
    /// `shadow_bind` (created once in `Renderer::build` with all 3 entries:
    /// globals buffer + shadow texture + comparison sampler). Recreating the
    /// bind group here used to provide only 1 entry, which mismatches the
    /// 3-entry `shadow_bgl` layout and panics inside `create_bind_group` on
    /// the first frame a directional light and a mesh coexist — i.e. the
    /// very first frame after Scene → Add Cube (3D).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_shadow(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rpass: &mut wgpu::RenderPass<'_>,
        mesh: &super::GpuMesh,
        shadow_bind: &wgpu::BindGroup,
        instances: &[u8],
    ) {
        let buf = self
            .ensure_instance_buf(device, instances.len() as u64, true)
            .clone();
        queue.write_buffer(&buf, 0, instances);

        rpass.set_pipeline(&self.shadow_pipeline);
        rpass.set_bind_group(0, shadow_bind, &[]);
        rpass.set_vertex_buffer(0, mesh.verts.slice(..));
        rpass.set_vertex_buffer(1, buf.slice(..instances.len() as u64));
        rpass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
        let count = (instances.len() / std::mem::size_of::<MeshInstance>()) as u32;
        rpass.draw_indexed(0..mesh.count, 0, 0..count);
    }

    fn ensure_instance_buf(
        &mut self,
        device: &wgpu::Device,
        needed: u64,
        shadow: bool,
    ) -> &wgpu::Buffer {
        let (buf, cap) = if shadow {
            (&mut self.shadow_instance_buf, &mut self.shadow_instance_cap)
        } else {
            (&mut self.instance_buf, &mut self.instance_cap)
        };
        if buf.as_ref().is_none_or(|_b| *cap < needed) {
            let size = needed.max(8192);
            *buf = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(if shadow {
                    "spark.mesh.shadow_instances"
                } else {
                    "spark.mesh.instances"
                }),
                size,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            *cap = size;
        }
        buf.as_ref().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::MeshInstance;

    #[test]
    fn instance_size_matches_wgsl() {
        // model(64) + color(16) + params(16) + emissive(16) = 112 bytes.
        assert_eq!(std::mem::size_of::<MeshInstance>(), 112);
    }
}
