//! Asset pipeline: discovery, lazy typed imports, caching and hot reload.
//!
//! * The project `assets/` tree is walked once at startup and watched with
//!   `notify` for the engine's lifetime.
//! * Imports are lazy: a texture is decoded the first time something
//!   references it, then cached with a `version` counter.
//! * On file change the affected asset is re-imported and its version is
//!   bumped; the renderer notices (path, version) changes and re-uploads to
//!   the GPU. Handles stay valid because everything is path-addressed.
//! * glTF models import as derived entries: `model.glb#0`, `model.glb#1`
//!   (primitives) and `model.glb#tex0` (textures), all invalidated together.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use notify::{Event, EventKind, RecursiveMode, Watcher};

use crate::components::Material;
use crate::math::{Vec3, Vertex};

/// Reference to an asset: project-root-relative path with `/` separators,
/// e.g. `"assets/player.png"`, `"assets/robot.glb#0"`.
pub type AssetRef = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetKind {
    Texture,
    Model,
    Sound,
    Scene,
    Prefab,
    Other,
}

impl AssetKind {
    pub fn from_extension(ext: &str) -> Self {
        match ext {
            "png" | "jpg" | "jpeg" | "hdr" | "bmp" | "webp" | "tga" => AssetKind::Texture,
            "glb" | "gltf" => AssetKind::Model,
            "wav" | "ogg" | "mp3" | "flac" => AssetKind::Sound,
            "scene" => AssetKind::Scene,
            "ron" => AssetKind::Prefab,
            _ => AssetKind::Other,
        }
    }
}

#[derive(Clone)]
pub struct AssetMeta {
    pub kind: AssetKind,
    pub version: u32,
}

/// A decoded texture (8-bit RGBA, CPU side until the renderer uploads it).
#[derive(Clone)]
pub struct TextureData {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub version: u32,
}

/// A CPU-side mesh; the renderer owns the GPU mirror.
#[derive(Clone)]
pub struct MeshData {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    /// Default material extracted from glTF (or builtin gray).
    pub material: Material,
}

/// Node tree produced by glTF import (used by the editor to spawn instances).
#[derive(Clone)]
pub struct GltfNode {
    pub name: String,
    pub position: crate::math::Vec3,
    pub rotation: crate::math::Vec3,
    pub scale: crate::math::Vec3,
    pub mesh: Option<String>,
    pub children: Vec<GltfNode>,
}

/// The asset system: index + caches + watcher.
pub struct Assets {
    root: PathBuf,
    index: HashMap<String, AssetMeta>,
    textures: HashMap<String, TextureData>,
    meshes: HashMap<String, MeshData>,
    sounds: HashMap<String, Arc<Vec<u8>>>,
    models: HashMap<String, Arc<Vec<GltfNode>>>,
    /// Held alive: dropping the watcher stops file notifications.
    #[allow(dead_code)]
    watcher: Option<notify::RecommendedWatcher>,
    events: Option<Receiver<Event>>,
    /// Paths re-imported since the last `take_reloaded` (for live updates).
    reloaded: Vec<String>,
}

fn normalize(p: &Path, root: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

impl Assets {
    /// Index the tree under `root` and start watching for changes.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let mut index = HashMap::new();
        if root.exists() {
            walk_tree(&root, &root, &mut index);
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |res: Result<Event, _>| {
            if let Ok(ev) = res {
                let _ = tx.send(ev);
            }
        })
        .ok();
        if let Some(w) = watcher.as_mut() {
            let _ = w.watch(&root, RecursiveMode::Recursive);
        }
        Self {
            root,
            index,
            textures: HashMap::new(),
            meshes: HashMap::new(),
            sounds: HashMap::new(),
            models: HashMap::new(),
            watcher,
            events: Some(rx),
            reloaded: Vec::new(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve an asset path to a filesystem location (`#sub` refs strip).
    pub fn fs_path(&self, asset: &str) -> PathBuf {
        let base = asset.split('#').next().unwrap_or(asset);
        self.root.join(base)
    }

    pub fn meta(&self, asset: &str) -> Option<&AssetMeta> {
        self.index.get(asset.split('#').next().unwrap_or(asset))
    }

    /// All discovered asset paths of a kind, sorted (asset browser listing).
    pub fn list(&self, kind: AssetKind) -> Vec<String> {
        let mut v: Vec<String> = self
            .index
            .iter()
            .filter(|(_, m)| m.kind == kind)
            .map(|(p, _)| p.clone())
            .collect();
        v.sort();
        v
    }

    // -----------------------------------------------------------------------
    // Typed access (lazy import on first use)
    // -----------------------------------------------------------------------

    /// Decoded texture, importing on demand. `#texN` refs address glTF
    /// embedded textures.
    pub fn texture(&mut self, asset: &str) -> Option<&TextureData> {
        if !self.textures.contains_key(asset) {
            let bytes = std::fs::read(self.fs_path(asset)).ok()?;
            let img = decode_image(&bytes)?;
            let data = TextureData {
                width: img.0,
                height: img.1,
                rgba: img.2,
                version: 0,
            };
            self.textures.insert(asset.to_string(), data);
        }
        self.textures.get(asset)
    }

    /// glTF-derived texture (loads the model once, extracts `#texN`).
    pub fn gltf_texture(&mut self, asset: &str) -> Option<&TextureData> {
        if !self.textures.contains_key(asset) {
            let model_path = asset.split('#').next()?.to_string();
            self.import_model(&model_path)?;
            self.import_model_textures(&model_path)?;
            return self.textures.get(asset);
        }
        self.textures.get(asset)
    }

    /// Mesh by asset path: builtin names or `model.glb#N` primitives.
    pub fn mesh(&mut self, asset: &str) -> Option<&MeshData> {
        if builtin_mesh(asset).is_some() && !self.meshes.contains_key(asset) {
            let (vertices, indices) = builtin_mesh(asset)?;
            self.meshes.insert(
                asset.to_string(),
                MeshData {
                    vertices,
                    indices,
                    material: Material::default(),
                },
            );
        }
        if !self.meshes.contains_key(asset) {
            let model_path = asset.split('#').next()?.to_string();
            self.import_model(&model_path)?;
        }
        self.meshes.get(asset)
    }

    /// Sound bytes (wav/ogg/mp3/flac), importing on demand.
    pub fn sound(&mut self, asset: &str) -> Option<Arc<Vec<u8>>> {
        if !self.sounds.contains_key(asset) {
            let bytes = std::fs::read(self.fs_path(asset)).ok()?;
            self.sounds.insert(asset.to_string(), Arc::new(bytes));
        }
        self.sounds.get(asset).cloned()
    }

    /// glTF node tree (scene instantiation), importing on demand.
    pub fn gltf_nodes(&mut self, model_path: &str) -> Option<Arc<Vec<GltfNode>>> {
        if !self.models.contains_key(model_path) {
            self.import_model(model_path)?;
        }
        self.models.get(model_path).cloned()
    }

    // -----------------------------------------------------------------------
    // Hot reload
    // -----------------------------------------------------------------------

    /// Drain watcher events; re-import anything currently cached. Call once
    /// per frame (cheap when nothing changed).
    pub fn update(&mut self) {
        let Some(rx) = self.events.take() else { return };
        let mut changed: Vec<PathBuf> = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            match ev.kind {
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {
                    changed.extend(ev.paths);
                }
                _ => {}
            }
        }
        self.events = Some(rx);
        for p in &changed {
            self.on_file_changed(p);
        }
        // New files may have appeared (watch create events update the index).
        for p in &changed {
            if let Some(meta) = self.index_mut_for(p) {
                let _ = meta;
            }
        }
    }

    fn index_mut_for(&mut self, path: &Path) -> Option<&mut AssetMeta> {
        let key = normalize(path, &self.root);
        if !self.index.contains_key(&key) && path.is_file() {
            let kind =
                AssetKind::from_extension(path.extension().and_then(|e| e.to_str()).unwrap_or(""));
            if kind != AssetKind::Other {
                self.index
                    .insert(key.clone(), AssetMeta { kind, version: 0 });
            }
        }
        self.index.get_mut(&key)
    }

    fn on_file_changed(&mut self, path: &Path) {
        let key = normalize(path, &self.root);
        let Some(kind) = self.index.get(&key).map(|m| m.kind) else {
            return;
        };
        // Bump version on the index entry.
        if let Some(meta) = self.index.get_mut(&key) {
            meta.version += 1;
        }
        match kind {
            AssetKind::Texture => {
                if let Some(tex) = self.textures.get_mut(&key)
                    && let Ok(bytes) = std::fs::read(path)
                    && let Some((w, h, rgba)) = decode_image(&bytes)
                {
                    tex.width = w;
                    tex.height = h;
                    tex.rgba = rgba;
                    tex.version += 1;
                    self.reloaded.push(key.clone());
                }
            }
            AssetKind::Model => {
                // Drop every derived entry; next access re-imports.
                let prefix = format!("{key}#");
                self.meshes.retain(|k, _| !k.starts_with(&prefix));
                self.textures.retain(|k, _| !k.starts_with(&prefix));
                self.models.remove(&key);
                self.reloaded.push(key);
            }
            AssetKind::Sound => {
                if let Ok(bytes) = std::fs::read(path) {
                    self.sounds.insert(key.clone(), Arc::new(bytes));
                    self.reloaded.push(key);
                }
            }
            _ => {
                self.reloaded.push(key);
            }
        }
    }

    /// Asset paths re-imported since the last call.
    pub fn take_reloaded(&mut self) -> Vec<String> {
        std::mem::take(&mut self.reloaded)
    }

    // -----------------------------------------------------------------------
    // glTF import
    // -----------------------------------------------------------------------

    fn import_model(&mut self, model_path: &str) -> Option<()> {
        let fs_path = self.root.join(model_path);
        let imported = crate::render::gltf::import(&fs_path)?;
        let mut nodes = Vec::new();
        for node in imported.root_nodes {
            nodes.push(self.register_gltf_node(model_path, node));
        }
        for (i, mut prim) in imported.primitives.into_iter().enumerate() {
            // Rewrite "#texN" refs to full "{model_path}#texN" asset paths.
            if let Some(tex) = &prim.material.texture
                && tex.starts_with('#')
            {
                prim.material.texture = Some(format!("{model_path}{tex}"));
            }
            let key = format!("{model_path}#{i}");
            self.meshes.insert(
                key.clone(),
                MeshData {
                    vertices: prim.vertices,
                    indices: prim.indices,
                    material: prim.material,
                },
            );
        }
        for (i, tex) in imported.textures.into_iter().enumerate() {
            let key = format!("{model_path}#tex{i}");
            self.textures.insert(
                key,
                TextureData {
                    width: tex.0,
                    height: tex.1,
                    rgba: tex.2,
                    version: 0,
                },
            );
        }
        self.models.insert(model_path.to_string(), Arc::new(nodes));
        Some(())
    }

    fn import_model_textures(&mut self, model_path: &str) -> Option<()> {
        if self.models.contains_key(model_path) {
            return Some(());
        }
        self.import_model(model_path)
    }

    fn register_gltf_node(&self, model_path: &str, node: crate::render::gltf::RawNode) -> GltfNode {
        GltfNode {
            name: node.name,
            position: node.position,
            rotation: node.rotation,
            scale: node.scale,
            mesh: node.primitive.map(|p| format!("{model_path}#{p}")),
            children: node
                .children
                .into_iter()
                .map(|c| self.register_gltf_node(model_path, c))
                .collect(),
        }
    }
}

fn walk_tree(root: &Path, dir: &Path, out: &mut HashMap<String, AssetMeta>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "target" {
                continue;
            }
            walk_tree(root, &p, out);
        } else if p.is_file() {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            let kind = AssetKind::from_extension(ext);
            if kind != AssetKind::Other {
                out.insert(normalize(&p, root), AssetMeta { kind, version: 0 });
            }
        }
    }
}

fn decode_image(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory(bytes).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some((w, h, rgba.into_raw()))
}

// ---------------------------------------------------------------------------
// Builtin meshes
// ---------------------------------------------------------------------------

fn push_quad(vertices: &mut Vec<Vertex>, indices: &mut Vec<u32>, v: [Vertex; 4]) {
    let base = vertices.len() as u32;
    vertices.extend_from_slice(&v);
    indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// Unit primitives addressable by name: `"cube"`, `"sphere"`, `"plane"`,
/// `"quad"`. Cube spans -0.5..0.5; plane lies on XZ; quad faces +Z.
pub fn builtin_mesh(name: &str) -> Option<(Vec<Vertex>, Vec<u32>)> {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    match name {
        "cube" => {
            let h = 0.5f32;
            let faces: [(Vec3, Vec3, Vec3); 6] = [
                // normal, tangent (u dir), bitangent (v dir)
                (Vec3::Z, Vec3::X, Vec3::Y),   // +Z front
                (-Vec3::Z, -Vec3::X, Vec3::Y), // -Z back
                (Vec3::X, -Vec3::Z, Vec3::Y),  // +X
                (-Vec3::X, Vec3::Z, Vec3::Y),  // -X
                (Vec3::Y, Vec3::X, -Vec3::Z),  // +Y top
                (-Vec3::Y, Vec3::X, Vec3::Z),  // -Y bottom
            ];
            for (n, u, v) in faces {
                let c = n * h;
                push_quad(
                    &mut vertices,
                    &mut indices,
                    [
                        Vertex {
                            position: (c - u * h - v * h).to_array(),
                            normal: n.to_array(),
                            uv: [0.0, 0.0],
                        },
                        Vertex {
                            position: (c + u * h - v * h).to_array(),
                            normal: n.to_array(),
                            uv: [1.0, 0.0],
                        },
                        Vertex {
                            position: (c + u * h + v * h).to_array(),
                            normal: n.to_array(),
                            uv: [1.0, 1.0],
                        },
                        Vertex {
                            position: (c - u * h + v * h).to_array(),
                            normal: n.to_array(),
                            uv: [0.0, 1.0],
                        },
                    ],
                );
            }
        }
        "sphere" => {
            let (stacks, sectors) = (14u32, 20u32);
            for i in 0..=stacks {
                let phi = std::f32::consts::PI * i as f32 / stacks as f32;
                for j in 0..=sectors {
                    let theta = std::f32::consts::TAU * j as f32 / sectors as f32;
                    let n = Vec3::new(phi.sin() * theta.cos(), phi.cos(), phi.sin() * theta.sin());
                    vertices.push(Vertex {
                        position: (n * 0.5).to_array(),
                        normal: n.to_array(),
                        uv: [j as f32 / sectors as f32, i as f32 / stacks as f32],
                    });
                }
            }
            for i in 0..stacks {
                for j in 0..sectors {
                    let a = i * (sectors + 1) + j;
                    let b = a + sectors + 1;
                    indices.extend_from_slice(&[a, b, a + 1, b, b + 1, a + 1]);
                }
            }
        }
        "plane" => push_quad(
            &mut vertices,
            &mut indices,
            [
                Vertex {
                    position: [-0.5, 0.0, -0.5],
                    normal: [0.0, 1.0, 0.0],
                    uv: [0.0, 0.0],
                },
                Vertex {
                    position: [0.5, 0.0, -0.5],
                    normal: [0.0, 1.0, 0.0],
                    uv: [1.0, 0.0],
                },
                Vertex {
                    position: [0.5, 0.0, 0.5],
                    normal: [0.0, 1.0, 0.0],
                    uv: [1.0, 1.0],
                },
                Vertex {
                    position: [-0.5, 0.0, 0.5],
                    normal: [0.0, 1.0, 0.0],
                    uv: [0.0, 1.0],
                },
            ],
        ),
        "quad" => push_quad(
            &mut vertices,
            &mut indices,
            [
                Vertex {
                    position: [-0.5, -0.5, 0.0],
                    normal: [0.0, 0.0, 1.0],
                    uv: [0.0, 0.0],
                },
                Vertex {
                    position: [0.5, -0.5, 0.0],
                    normal: [0.0, 0.0, 1.0],
                    uv: [1.0, 0.0],
                },
                Vertex {
                    position: [0.5, 0.5, 0.0],
                    normal: [0.0, 0.0, 1.0],
                    uv: [1.0, 1.0],
                },
                Vertex {
                    position: [-0.5, 0.5, 0.0],
                    normal: [0.0, 0.0, 1.0],
                    uv: [0.0, 1.0],
                },
            ],
        ),
        _ => return None,
    }
    Some((vertices, indices))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_exist() {
        for name in ["cube", "sphere", "plane", "quad"] {
            let (v, i) = builtin_mesh(name).unwrap();
            assert!(!v.is_empty());
            assert!(!i.is_empty());
        }
        assert!(builtin_mesh("nope").is_none());
    }

    #[test]
    fn index_walk() {
        let dir = std::env::temp_dir().join(format!("spark_assets_test_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("assets/sfx")).unwrap();
        std::fs::write(dir.join("assets/player.png"), b"not-really-png").unwrap();
        std::fs::write(dir.join("assets/sfx/jump.wav"), b"wav").unwrap();
        std::fs::write(dir.join("notes.txt"), b"ignored").unwrap();
        let mut assets = Assets::new(&dir);
        let textures = assets.list(AssetKind::Texture);
        let sounds = assets.list(AssetKind::Sound);
        assert_eq!(textures, vec!["assets/player.png".to_string()]);
        assert_eq!(sounds, vec!["assets/sfx/jump.wav".to_string()]);
        // Lazy import of an invalid image must not panic.
        assert!(assets.texture("assets/player.png").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
