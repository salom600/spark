//! glTF import: turns `.glb`/`.gltf` files into engine meshes, materials,
//! textures and a node tree.
//!
//! v1 scope (documented in DECISIONS.md): static meshes, PBR factors and
//! base-color textures (embedded or external files), node hierarchy and
//! transforms. Skinning and animation clips are parsed by the gltf crate but
//! not evaluated — they are roadmap items. Missing normals are computed flat
//! from triangle winding; missing tex coords default to (0, 0).

use std::path::Path;

use glam::{Quat, Vec3};

use crate::components::Material;
use crate::math::{Color, Vertex};

pub struct PrimData {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub material: Material,
}

/// One glTF node, decomposed for spawning as entities.
#[derive(Clone)]
pub struct RawNode {
    pub name: String,
    pub position: Vec3,
    pub rotation: Vec3,
    pub scale: Vec3,
    /// Index into `GltfImport::primitives`.
    pub primitive: Option<usize>,
    pub children: Vec<RawNode>,
}

pub struct GltfImport {
    pub primitives: Vec<PrimData>,
    /// (width, height, rgba bytes) — referenced by `Material::texture` as
    /// `"path#texN"` through the asset system.
    pub textures: Vec<(u32, u32, Vec<u8>)>,
    pub root_nodes: Vec<RawNode>,
}

/// Import a glTF file from disk.
pub fn import(path: &Path) -> Option<GltfImport> {
    let (doc, buffers, images) = gltf::import(path)
        .map_err(|e| {
            log::warn!("gltf: failed to import {}: {e}", path.display());
        })
        .ok()?;

    // ---- Images → RGBA8 -------------------------------------------------
    let mut texture_of_image: Vec<Option<usize>> = vec![None; images.len()];
    let mut textures: Vec<(u32, u32, Vec<u8>)> = Vec::new();

    // ---- Primitives -------------------------------------------------------
    let mut primitives: Vec<PrimData> = Vec::new();
    let mut first_prim_of_mesh: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new();

    for mesh in doc.meshes() {
        first_prim_of_mesh.insert(mesh.index(), primitives.len());
        for prim in mesh.primitives() {
            let reader = prim.reader(|buffer| buffers.get(buffer.index()).map(|b| b.0.as_slice()));

            let positions: Vec<[f32; 3]> = reader
                .read_positions()
                .map(|p| p.collect())
                .unwrap_or_default();
            if positions.is_empty() {
                continue;
            }
            let indices: Vec<u32> = reader
                .read_indices()
                .map(|i| i.into_u32().collect())
                .unwrap_or_else(|| (0..positions.len() as u32).collect());
            let normals: Option<Vec<[f32; 3]>> = reader.read_normals().map(|n| n.collect());
            let uvs: Vec<[f32; 2]> = reader
                .read_tex_coords(0)
                .map(|t| t.into_f32().collect())
                .unwrap_or_else(|| vec![[0.0, 0.0]; positions.len()]);

            let normals = normals.unwrap_or_else(|| flat_normals(&positions, &indices));

            let vertices: Vec<Vertex> = positions
                .iter()
                .zip(normals.iter())
                .zip(uvs.iter())
                .map(|((p, n), uv)| Vertex {
                    position: *p,
                    normal: *n,
                    uv: *uv,
                })
                .collect();

            // Material: PBR factors + base color texture.
            let mat = prim.material();
            let pbr = mat.pbr_metallic_roughness();
            let base = pbr.base_color_factor();
            let emissive = mat.emissive_factor();
            let mut material = Material {
                albedo: Color::rgba(base[0], base[1], base[2], base[3]),
                texture: None,
                emissive: Color::rgb(emissive[0], emissive[1], emissive[2]),
                metallic: pbr.metallic_factor(),
                roughness: pbr.roughness_factor(),
                unlit: false,
            };
            if let Some(info) = pbr.base_color_texture() {
                let image_idx = info.texture().source().index();
                if let Some(img) = images.get(image_idx) {
                    let local = texture_of_image[image_idx].unwrap_or_else(|| {
                        let rgba = image_to_rgba(img);
                        textures.push((img.width, img.height, rgba));
                        texture_of_image[image_idx] = Some(textures.len() - 1);
                        textures.len() - 1
                    });
                    material.texture = Some(format!("#tex{local}"));
                }
            }
            primitives.push(PrimData {
                vertices,
                indices,
                material,
            });
        }
    }

    // ---- Node tree --------------------------------------------------------
    let scene = doc.default_scene().or_else(|| doc.scenes().next());
    let mut root_nodes = Vec::new();
    if let Some(scene) = scene {
        for node in scene.nodes() {
            root_nodes.push(convert_node(&node, &first_prim_of_mesh));
        }
    }

    Some(GltfImport {
        primitives,
        textures,
        root_nodes,
    })
}

fn convert_node(
    node: &gltf::Node,
    first_prim_of_mesh: &std::collections::HashMap<usize, usize>,
) -> RawNode {
    let (translation, rotation, scale) = match node.transform() {
        gltf::scene::Transform::Decomposed {
            translation,
            rotation,
            scale,
        } => (
            Vec3::from(translation),
            Quat::from_xyzw(rotation[0], rotation[1], rotation[2], rotation[3]),
            Vec3::from(scale),
        ),
        gltf::scene::Transform::Matrix { matrix } => {
            let m = glam::Mat4::from_cols_array_2d(&matrix);
            let (t, r, s) = m.to_scale_rotation_translation();
            (t, r, s)
        }
    };
    let euler = rotation.to_euler(glam::EulerRot::XYZ);
    RawNode {
        name: node.name().unwrap_or("glTF node").to_string(),
        position: translation,
        rotation: Vec3::new(
            euler.0.to_degrees(),
            euler.1.to_degrees(),
            euler.2.to_degrees(),
        ),
        scale,
        primitive: node
            .mesh()
            .and_then(|m| first_prim_of_mesh.get(&m.index()).copied()),
        children: node
            .children()
            .map(|c| convert_node(&c, first_prim_of_mesh))
            .collect(),
    }
}

fn flat_normals(positions: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut normals = vec![[0.0f32; 3]; positions.len()];
    for tri in indices.chunks(3) {
        if tri.len() < 3 {
            break;
        }
        let p0 = Vec3::from(positions[tri[0] as usize]);
        let p1 = Vec3::from(positions[tri[1] as usize]);
        let p2 = Vec3::from(positions[tri[2] as usize]);
        let n = (p1 - p0).cross(p2 - p0).normalize_or_zero();
        for &i in tri {
            normals[i as usize] = n.to_array();
        }
    }
    normals
}

fn image_to_rgba(img: &gltf::image::Data) -> Vec<u8> {
    use gltf::image::Format;
    match img.format {
        Format::R8G8B8A8 => img.pixels.clone(),
        Format::R8G8B8 => img
            .pixels
            .chunks(3)
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect(),
        Format::R16G16 => img
            .pixels
            .chunks(4)
            .flat_map(|c| [c[0], c[2], 0, 255])
            .collect(),
        Format::R16G16B16 => img
            .pixels
            .chunks(6)
            .flat_map(|c| [c[1], c[3], c[5], 255])
            .collect(),
        Format::R16G16B16A16 => img
            .pixels
            .chunks(8)
            .flat_map(|c| {
                let r = u16::from_le_bytes([c[0], c[1]]);
                let g = u16::from_le_bytes([c[2], c[3]]);
                let b = u16::from_le_bytes([c[4], c[5]]);
                let a = u16::from_le_bytes([c[6], c[7]]);
                [
                    (r >> 8) as u8,
                    (g >> 8) as u8,
                    (b >> 8) as u8,
                    (a >> 8) as u8,
                ]
            })
            .collect(),
        // R8 (luminance) and anything unexpected: replicate to RGB.
        _ => img.pixels.iter().flat_map(|&c| [c, c, c, 255]).collect(),
    }
}
