//! Regression tests for the render frame graph's resource ordering.
//!
//! Bug guarded: the shadow pass bound the 3-entry `spark.shadow_bind` —
//! which samples the shadow map — while also using that same map as its
//! depth-stencil attachment. wgpu usage scopes make DEPTH_STENCIL_WRITE
//! exclusive, so the first frame with a directional light AND a mesh (i.e.
//! right after Scene → Add Cube (3D)) aborted with:
//!
//! ```text
//! Attempted to use Texture with 'spark.shadow' label ... with conflicting
//! usages. Current usage TextureUsages(RESOURCE) and new usage
//! TextureUsages(DEPTH_STENCIL_WRITE).
//! ```
//!
//! The real encoder cannot run in headless CI, so the frame graph is plain
//! data (`PassPlan`) and its usage-scope rules are validated on CPU — the
//! same `validate_pass_plan` call `Renderer::render` runs before encoding.

use spark::render::{PassPlan, plan_frame, validate_pass_plan};

/// Sun + mesh (the Add-Cube state): the shadow pass must run, write the
/// map, and sample *nothing*; the main pass samples it strictly afterwards.
#[test]
fn shadow_pass_writes_map_and_samples_nothing() {
    let plan = plan_frame(true, true);
    assert_eq!(plan[0].label, "spark.shadow");
    assert_eq!(plan[0].writes_depth, Some("spark.shadow"));
    assert!(
        plan[0].samples.is_empty(),
        "shadow pass must not sample the shadow map"
    );
    validate_pass_plan(&plan).expect("sun+mesh frame plan is valid");
}

/// Scenes without a directional light or without meshes skip the shadow
/// pass; the remaining plan must still be valid (the main pass then samples
/// the zero-initialized map, which no pass writes — allowed).
#[test]
fn plan_is_valid_without_shadow_pass() {
    for (dir, meshes) in [(false, true), (true, false), (false, false)] {
        let plan = plan_frame(dir, meshes);
        assert!(
            plan.iter().all(|p| p.label != "spark.shadow"),
            "shadow pass must be skipped for (dir={dir}, meshes={meshes})"
        );
        validate_pass_plan(&plan).expect("plan without shadow pass is valid");
    }
}

/// The original crash as data: a pass that writes a texture as depth AND
/// samples it in the same usage scope must be rejected.
#[test]
fn same_pass_write_and_sample_is_rejected() {
    let plan = [PassPlan {
        label: "spark.shadow",
        writes_depth: Some("spark.shadow"),
        samples: &["spark.shadow"],
    }];
    let err = validate_pass_plan(&plan).expect_err("same-pass write+sample must fail");
    assert!(
        err.contains("spark.shadow"),
        "error should name the texture: {err}"
    );
}

/// Read-before-write: sampling the shadow map in a pass ordered *before*
/// the shadow pass must be rejected.
#[test]
fn sample_before_write_is_rejected() {
    let plan = [
        PassPlan {
            label: "spark.main",
            writes_depth: Some("spark.depth"),
            samples: &["spark.shadow"],
        },
        PassPlan {
            label: "spark.shadow",
            writes_depth: Some("spark.shadow"),
            samples: &[],
        },
    ];
    assert!(
        validate_pass_plan(&plan).is_err(),
        "sampling the shadow map before the shadow pass must fail"
    );
}
