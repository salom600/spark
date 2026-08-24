//! Project manifests: the `project.ron` file that defines a game (name,
//! dimension, main scene, input bindings) plus template scaffolding and
//! export packaging.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::input::Binding;
use crate::math::Color;
use crate::scene::Dimension;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    #[serde(default)]
    pub dimension: Dimension,
    /// Project-relative path to the scene opened by default / on play.
    pub main_scene: String,
    #[serde(default)]
    pub clear: Color,
    /// Named action bindings: `"jump": [Key("Space"), Pad("South")]`.
    #[serde(default)]
    pub input: HashMap<String, Vec<Binding>>,
}

impl Project {
    /// Load `project.ron` from a project directory.
    pub fn load_dir(dir: &Path) -> anyhow::Result<Project> {
        let path = dir.join("project.ron");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
        let project: Project = ron::from_str(&text)?;
        Ok(project)
    }

    pub fn save_dir(&self, dir: &Path) -> anyhow::Result<()> {
        let text = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default().struct_names(true))?;
        std::fs::write(dir.join("project.ron"), text)?;
        Ok(())
    }

    /// Create a fresh project from the bundled blank template.
    pub fn create_from_template(dir: &Path, name: &str, dimension: Dimension) -> anyhow::Result<PathBuf> {
        let dir = dir.to_path_buf();
        std::fs::create_dir_all(dir.join("assets"))?;
        std::fs::create_dir_all(dir.join("scenes"))?;
        let project = Project {
            name: name.to_string(),
            dimension,
            main_scene: "scenes/main.scene".into(),
            clear: Color::ENGINE_BG,
            input: HashMap::new(),
        };
        project.save_dir(&dir)?;
        // Blank scene: a camera + a light, ready to edit.
        let scene = crate::scene::Scene {
            dimension,
            name: "Main".into(),
            ..Default::default()
        };
        let mut scene = scene;
        let registry = crate::scene::default_registry();
        let _ = &registry;
        scene.world.spawn((
            crate::ecs::Name("Camera".into()),
            crate::components::Transform::default(),
            crate::components::Camera::default(),
        ));
        scene.world.spawn((
            crate::ecs::Name("Sun".into()),
            crate::components::Transform::default(),
            crate::components::Light::default(),
        ));
        let text = scene.save(&registry);
        std::fs::write(dir.join("scenes/main.scene"), text)?;
        Ok(dir)
    }

    /// Package a project for distribution: copy the runtime binary next to
    /// `project.ron` + `assets/` (the "export" output shape).
    pub fn export(source_dir: &Path, binary: &Path, out_dir: &Path) -> anyhow::Result<PathBuf> {
        std::fs::create_dir_all(out_dir)?;
        let bin_name = binary
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("spark");
        std::fs::copy(binary, out_dir.join(bin_name))?;
        for entry in ["project.ron", "scenes", "assets"] {
            let src = source_dir.join(entry);
            let dst = out_dir.join(entry);
            if src.is_dir() {
                copy_dir(&src, &dst)?;
            } else if src.is_file() {
                std::fs::copy(&src, &dst)?;
            }
        }
        Ok(out_dir.to_path_buf())
    }
}

fn copy_dir(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_roundtrip() {
        let dir = std::env::temp_dir().join(format!("spark_proj_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Project::create_from_template(&dir, "Test Game", Dimension::D2).unwrap();
        let project = Project::load_dir(&dir).unwrap();
        assert_eq!(project.name, "Test Game");
        assert_eq!(project.main_scene, "scenes/main.scene");
        let scene_path = dir.join(&project.main_scene);
        let registry = crate::scene::default_registry();
        let scene = crate::scene::load_scene_file(&scene_path, &registry).unwrap();
        assert!(crate::ecs::find_by_name(&scene.world, "Camera").is_some());
        std::fs::remove_dir_all(&dir).ok();
    }
}
