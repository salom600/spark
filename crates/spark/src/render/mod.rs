//! The unified 2D/3D renderer.
//!
//! One wgpu device, one frame graph: shadow pass (3D, directional light) →
//! main pass (PBR meshes, then 2D sprites on top) → egui pass (editor/HUD).
//! Sprites and meshes share the camera, material and asset systems; 2D is
//! just an orthographic camera drawing textured quads with a Z layer.
//!
//! GPU objects are cached per asset path and invalidated by version (see
//! `assets::Assets`), giving hot-reload for free.

pub mod gltf;
mod mesh;
mod sprite;

pub use gltf::{import, RawNode};
use mesh::MeshPass;
use sprite::SpritePass;
use wgpu::util::DeviceExt;

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};

use crate::assets::{AssetKind, Assets};
use crate::components::{Camera, CameraKind, Light, LightKind, MeshRenderer, Sprite, Transform, Visible};
use crate::math::Color;
use crate::scene::Scene;

pub const MAX_POINT_LIGHTS: usize = 16;

// ---------------------------------------------------------------------------
// GPU data types
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Default)]
pub struct Globals {
    pub view_proj: [[f32; 4]; 4],
    pub camera_pos: [f32; 4],
    pub light_view_proj: [[f32; 4]; 4],
    pub dir_light: [f32; 4],       // xyz direction, w = light present flag
    pub dir_light_color: [f32; 4], // rgb * intensity, a = ambient
    pub light_meta: [f32; 4],      // point count, shadow bias, unused, unused
    pub point_lights: [[f32; 8]; MAX_POINT_LIGHTS], // pos.xyz, range, color.rgb * intensity, pad
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct SpriteInstance {
    pub pos: [f32; 3],
    pub rot: f32,
    pub scale: [f32; 2],
    pub color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct MeshInstance {
    pub model: [[f32; 4]; 4],
    pub color: [f32; 4],
    pub params: [f32; 4],    // metallic, roughness, unlit, unused
    pub emissive: [f32; 4],
}

// ---------------------------------------------------------------------------
// Frame draw data (built CPU-side, then submitted)
// ---------------------------------------------------------------------------

/// Everything the renderer needs for one frame, independent of the target.
#[derive(Default)]
pub struct FrameDraw {
    pub clear: Color,
    pub globals: Globals,
    /// (texture path, instances) — one instanced draw per group.
    pub sprites: Vec<(String, Vec<SpriteInstance>)>,
    /// (mesh path, texture path or "", instances) — also used by shadows.
    pub meshes: Vec<(String, String, Vec<MeshInstance>)>,
    pub has_directional: bool,
}

/// Extract visible sprite/mesh instances + lights from the scene.
/// `camera_override` replaces the scene's first camera (editor camera).
pub fn build_frame_draw(
    scene: &Scene,
    assets: &mut Assets,
    aspect: f32,
    camera_override: Option<(Transform, Camera)>,
) -> FrameDraw {
    let world = &scene.world;

    // Camera: explicit override or first entity with a Camera component.
    let (cam, cam_tr) = if let Some((tr, c)) = camera_override {
        (c, tr)
    } else {
        match world.query::<(&Camera, &Transform)>().iter().next() {
            Some((_, (c, t))) => (c.clone(), *t),
            None => return default_draw(scene),
        }
    };

    let view = Mat4::from_translation(cam_tr.position) * Mat4::from_quat(cam_tr.quat());
    let proj = match &cam.kind {
        CameraKind::Ortho2D { height } => {
            let h = height.max(0.001);
            let w = h * aspect.max(0.001);
            Mat4::orthographic_rh(-w / 2.0, w / 2.0, -h / 2.0, h / 2.0, -1000.0, 1000.0)
        }
        CameraKind::Perspective { fov_deg } => Mat4::perspective_rh(
            fov_deg.to_radians().max(0.01),
            aspect.max(0.01),
            cam.near.max(0.001),
            cam.far.max(cam.near + 0.01),
        ),
    };
    let view_proj = proj * view.inverse();

    // Lights.
    let mut dir: Option<([f32; 4], [f32; 4])> = None;
    let mut points: Vec<[f32; 8]> = Vec::new();
    for (_e, (l, t)) in world.query::<(&Light, &Transform)>().iter() {
        match &l.kind {
            LightKind::Directional { direction } => {
                if dir.is_none() {
                    let d = direction.normalize_or_zero();
                    dir = Some((
                        [d.x, d.y, d.z, 1.0],
                        [l.color.r * l.intensity, l.color.g * l.intensity, l.color.b * l.intensity, scene.ambient],
                    ));
                }
            }
            LightKind::Point { range } => {
                if points.len() < MAX_POINT_LIGHTS {
                    points.push([
                        t.position.x,
                        t.position.y,
                        t.position.z,
                        *range,
                        l.color.r * l.intensity,
                        l.color.g * l.intensity,
                        l.color.b * l.intensity,
                        0.0,
                    ]);
                }
            }
        }
    }
    while points.len() < MAX_POINT_LIGHTS {
        points.push([0.0; 8]);
    }
    let (dir_vec, dir_color) = dir.unwrap_or(([0.0, -1.0, 0.0, 0.0], [0.0, 0.0, 0.0, scene.ambient]));

    // Shadow view-projection: ortho box around the camera target (fixed
    // radius in v1 — documented in DECISIONS.md §6).
    let shadow_vp = {
        let target = cam_tr.position + cam_forward(&cam_tr);
        let light_dir = Vec3::new(dir_vec[0], dir_vec[1], dir_vec[2]).normalize_or_zero();
        let eye = target - light_dir * 40.0;
        let view = Mat4::look_at_rh(eye, target, Vec3::Y);
        let proj = Mat4::orthographic_rh(-40.0, 40.0, -40.0, 40.0, 0.1, 120.0);
        proj * view
    };

    let mut globals = Globals {
        view_proj: view_proj.to_cols_array_2d(),
        camera_pos: [cam_tr.position.x, cam_tr.position.y, cam_tr.position.z, 0.0],
        light_view_proj: shadow_vp.to_cols_array_2d(),
        dir_light: dir_vec,
        dir_light_color: dir_color,
        light_meta: [points.len() as f32, 0.0015, 0.0, 0.0],
        point_lights: points.try_into().unwrap(),
    };
    globals.dir_light[3] = if dir.is_some() { 1.0 } else { 0.0 };

    let mut draw = FrameDraw {
        clear: cam.clear,
        globals,
        has_directional: dir.is_some(),
        ..Default::default()
    };

    // Sprites (visible, with Sprite component).
    for (e, (sp, t)) in world.query::<(&Sprite, &Transform)>().iter() {
        if !visible(world, e) {
            continue;
        }
        let instance = SpriteInstance {
            pos: [t.position.x, t.position.y, t.position.z],
            rot: t.rotation.z.to_radians(),
            scale: [sp.size.x * t.scale.x, sp.size.y * t.scale.y],
            color: [sp.color.r, sp.color.g, sp.color.b, sp.color.a],
        };
        match draw.sprites.iter_mut().find(|(p, _)| *p == sp.image) {
            Some(g) => g.1.push(instance),
            None => draw.sprites.push((sp.image.clone(), vec![instance])),
        }
    }

    // Meshes (visible, with MeshRenderer component).
    for (e, (mr, t)) in world.query::<(&MeshRenderer, &Transform)>().iter() {
        if !visible(world, e) {
            continue;
        }
        let model = Mat4::from_scale_rotation_translation(t.scale, t.quat(), t.position);
        let instance = MeshInstance {
            model: model.to_cols_array_2d(),
            color: [mr.material.albedo.r, mr.material.albedo.g, mr.material.albedo.b, mr.material.albedo.a],
            params: [mr.material.metallic, mr.material.roughness, if mr.material.unlit { 1.0 } else { 0.0 }, 0.0],
            emissive: [mr.material.emissive.r, mr.material.emissive.g, mr.material.emissive.b, 0.0],
        };
        let tex = mr.material.texture.clone().unwrap_or_default();
        match draw.meshes.iter_mut().find(|(m, t, _)| *m == mr.mesh && *t == tex) {
            Some(g) => g.2.push(instance),
            None => draw.meshes.push((mr.mesh.clone(), tex, vec![instance])),
        }
    }

    // Warm the import cache so the renderer never hits the filesystem later.
    for (path, _) in &draw.sprites {
        warm_texture(assets, path);
    }
    for (mesh, tex, _) in &draw.meshes {
        assets.mesh(mesh);
        if !tex.is_empty() {
            warm_texture(assets, tex);
        }
    }

    draw
}

fn warm_texture(assets: &mut Assets, path: &str) {
    if assets.meta(path).map(|m| m.kind) == Some(AssetKind::Texture) {
        assets.texture(path);
    } else {
        assets.gltf_texture(path);
    }
}

fn default_draw(scene: &Scene) -> FrameDraw {
    FrameDraw {
        clear: scene.sky,
        ..Default::default()
    }
}

fn visible(world: &hecs::World, e: hecs::Entity) -> bool {
    world.get::<&Visible>(e).map(|v| v.0).unwrap_or(true)
}

fn cam_forward(t: &Transform) -> Vec3 {
    t.quat() * Vec3::Z
}

// ---------------------------------------------------------------------------
// Renderer
// ---------------------------------------------------------------------------

/// Owns the wgpu device, surface and all pipelines. The surface borrows the
/// window it was created from (winit 0.30 ownership model).
pub struct Renderer<'window> {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    surface: wgpu::Surface<'window>,
    config: wgpu::SurfaceConfiguration,
    depth: wgpu::TextureView,
    shadow_view: wgpu::TextureView,
    shadow_bind: wgpu::BindGroup,
    shadow_bgl: wgpu::BindGroupLayout,
    globals_bgl: wgpu::BindGroupLayout,
    globals_buf: wgpu::Buffer,
    sprite_pass: SpritePass,
    mesh_pass: MeshPass,
    white_view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    tex_cache: HashMap<String, (u32, wgpu::TextureView)>,
    mesh_cache: HashMap<String, (u32, GpuMesh)>,
    pub egui_renderer: egui_wgpu::Renderer,
}

pub struct GpuMesh {
    pub verts: wgpu::Buffer,
    pub indices: wgpu::Buffer,
    pub count: u32,
}

pub const SHADOW_SIZE: u32 = 2048;

impl<'window> Renderer<'window> {
    /// Create a renderer bound to a window surface. Tries Vulkan/DX12/Metal
    /// first, GL as compatibility fallback (via wgpu backend selection).
    pub fn new(window: &'window winit::window::Window) -> anyhow::Result<Self> {
        let backends = wgpu::Backends::PRIMARY | wgpu::Backends::GL;
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends,
            ..Default::default()
        });
        let surface = instance.create_surface(window)?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;

        let size = window.inner_size();
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let mut renderer = Self::build(device, queue, surface, config)?;
        renderer.upload_white();
        Ok(renderer)
    }

    fn build(
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface: wgpu::Surface<'window>,
        config: wgpu::SurfaceConfiguration,
    ) -> anyhow::Result<Self> {
        let depth = create_depth(&device, config.width, config.height);

        let globals_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("spark.globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("spark.globals_buf"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Shadow resources: globals + depth map + comparison sampler.
        let shadow_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("spark.shadow_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ],
        });

        let shadow_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("spark.shadow"),
            size: wgpu::Extent3d { width: SHADOW_SIZE, height: SHADOW_SIZE, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let shadow_view = shadow_tex.create_view(&Default::default());
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("spark.shadow_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let shadow_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("spark.shadow_bind"),
            layout: &shadow_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: globals_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&shadow_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&shadow_sampler) },
            ],
        });

        let white = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("spark.white"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let white_view = white.create_view(&Default::default());

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("spark.sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let sprite_pass = SpritePass::new(&device, config.format, &globals_bgl);
        let mesh_pass = MeshPass::new(&device, config.format, &shadow_bgl);
        let egui_renderer =
            egui_wgpu::Renderer::new(&device, config.format, egui_wgpu::RendererOptions::default());

        Ok(Self {
            device,
            queue,
            surface,
            config,
            depth,
            shadow_view,
            shadow_bind,
            shadow_bgl,
            globals_bgl,
            globals_buf,
            sprite_pass,
            mesh_pass,
            white_view,
            sampler,
            tex_cache: HashMap::new(),
            mesh_cache: HashMap::new(),
            egui_renderer,
        })
    }

    fn upload_white(&mut self) {
        let white = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("spark.white"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &white,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[255, 255, 255, 255],
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4), rows_per_image: Some(1) },
            wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        );
        self.white_view = white.create_view(&Default::default());
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.depth = create_depth(&self.device, width, height);
    }

    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    /// Ensure a texture is on the GPU; white when missing (artists see
    /// something instead of a crash).
    fn ensure_texture(&mut self, assets: &mut Assets, path: &str) -> wgpu::TextureView {
        if path.is_empty() {
            return self.white_view.clone();
        }
        let entry_version = assets.meta(path).map(|m| m.version).unwrap_or(0);
        if let Some((v, view)) = self.tex_cache.get(path) {
            if *v == entry_version {
                return view.clone();
            }
        }
        let data = if assets.meta(path).map(|m| m.kind) == Some(AssetKind::Texture) {
            assets.texture(path).cloned()
        } else {
            assets.gltf_texture(path).cloned()
        };
        let view = match data {
            Some(tex) => self.upload_texture(path, &tex),
            None => {
                log::warn!("renderer: texture \"{path}\" missing, using white");
                self.white_view.clone()
            }
        };
        self.tex_cache.insert(path.to_string(), (entry_version, view.clone()));
        view
    }

    fn upload_texture(&mut self, label: &str, tex: &crate::assets::TextureData) -> wgpu::TextureView {
        let t = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: tex.width, height: tex.height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &t, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &tex.rgba,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(tex.width * 4), rows_per_image: Some(tex.height) },
            wgpu::Extent3d { width: tex.width, height: tex.height, depth_or_array_layers: 1 },
        );
        t.create_view(&Default::default())
    }

    fn ensure_mesh(&mut self, assets: &mut Assets, path: &str) -> Option<GpuMesh> {
        let entry_version = assets
            .meta(path.split('#').next().unwrap_or(path))
            .map(|m| m.version)
            .unwrap_or(0);
        if let Some((v, m)) = self.mesh_cache.get(path) {
            if *v == entry_version {
                return Some(GpuMesh { verts: m.verts.clone(), indices: m.indices.clone(), count: m.count });
            }
        }
        let mesh = assets.mesh(path)?.clone();
        let verts = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(path),
            contents: bytemuck::cast_slice(&mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let indices = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(path),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let gpu = GpuMesh { verts, indices, count: mesh.indices.len() as u32 };
        self.mesh_cache.insert(
            path.to_string(),
            (entry_version, GpuMesh { verts: gpu.verts.clone(), indices: gpu.indices.clone(), count: gpu.count }),
        );
        Some(gpu)
    }

    /// Render one frame.
    ///
    /// * `viewport_px` — (x, y, w, h) scissor for scene content in physical
    ///   pixels (None = full window). The editor passes its central-panel rect.
    /// * `egui` — tessellated egui output drawn on top (editor panels / HUD).
    pub fn render(
        &mut self,
        assets: &mut Assets,
        frame: &FrameDraw,
        egui: Option<(&[egui::ClippedPrimitive], &egui_wgpu::ScreenDescriptor)>,
        viewport_px: Option<(u32, u32, u32, u32)>,
        pre_submit: Vec<wgpu::CommandBuffer>,
    ) -> anyhow::Result<()> {
        if !pre_submit.is_empty() {
            self.queue.submit(pre_submit);
        }
        let surface_tex = self.surface.get_current_texture()?;
        let view = surface_tex.texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());

        self.queue.write_buffer(&self.globals_buf, 0, bytemuck::bytes_of(&frame.globals));

        let (sx, sy, sw, sh) = viewport_px
            .map(|(x, y, w, h)| (x, y, w.max(1), h.max(1)))
            .unwrap_or((0, 0, self.config.width, self.config.height));

        // ---- Shadow pass (directional light, 3D meshes) ---------------------
        if frame.has_directional && !frame.meshes.is_empty() {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("spark.shadow"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.shadow_view,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            for (mesh_path, _tex, instances) in &frame.meshes {
                let Some(gpu) = self.ensure_mesh(assets, mesh_path) else { continue };
                self.mesh_pass.draw_shadow(
                    &self.device,
                    &self.queue,
                    &mut rpass,
                    &gpu,
                    &self.shadow_bgl,
                    &self.globals_buf,
                    bytemuck::cast_slice(instances),
                );
            }
        }

        // ---- Main pass --------------------------------------------------------
        {
            let clear = frame.clear.to_rgba8();
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("spark.main"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: clear[0] as f64 / 255.0,
                            g: clear[1] as f64 / 255.0,
                            b: clear[2] as f64 / 255.0,
                            a: clear[3] as f64 / 255.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            rpass.set_scissor_rect(
                sx.min(self.config.width),
                sy.min(self.config.height),
                sw.min(self.config.width.saturating_sub(sx)).max(1),
                sh.min(self.config.height.saturating_sub(sy)).max(1),
            );
            let _ = (sx, sy, sw, sh);

            // Meshes first (depth-tested, shadowed, lit).
            for (mesh_path, tex_path, instances) in &frame.meshes {
                let Some(gpu) = self.ensure_mesh(assets, mesh_path) else { continue };
                let tex_view = if tex_path.is_empty() {
                    self.white_view.clone()
                } else {
                    self.ensure_texture(assets, tex_path)
                };
                self.mesh_pass.draw(
                    &self.device,
                    &self.queue,
                    &mut rpass,
                    &gpu,
                    &self.shadow_bind,
                    &self.sampler,
                    &tex_view,
                    bytemuck::cast_slice(instances),
                );
            }

            // Sprites on top (no depth write, depth test against meshes).
            for (tex_path, instances) in &frame.sprites {
                let tex_view = self.ensure_texture(assets, tex_path);
                self.sprite_pass.draw(
                    &self.device,
                    &self.queue,
                    &mut rpass,
                    &self.globals_bgl,
                    &self.globals_buf,
                    &self.sampler,
                    &tex_view,
                    bytemuck::cast_slice(instances),
                );
            }
        }

        // ---- egui pass ---------------------------------------------------------
        if let Some((jobs, screen)) = egui {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("spark.egui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            // egui-wgpu 0.33 requires a 'static pass; forget_lifetime is safe
            // because the parent encoder is not touched until the pass ends.
            let mut rpass = rpass.forget_lifetime();
            self.egui_renderer.render(&mut rpass, jobs, screen);
        }

        self.queue.submit(Some(encoder.finish()));
        surface_tex.present();
        Ok(())
    }
}

fn create_depth(device: &wgpu::Device, w: u32, h: u32) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("spark.depth"),
            size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&Default::default())
}
