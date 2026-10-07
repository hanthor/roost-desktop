//! SDR white-point adjustment after the complete scene, before presentation.
//!
//! Captures never use this stage. No global shader override is installed. Each
//! active output has a bounded physical-size texture; neutral output bypasses
//! the extra render/copy. This does not implement ICC/HDR or hardware gamma.
use smithay::backend::{
    allocator::Fourcc,
    renderer::{
        gles::{
            GlesError, GlesFrame, GlesRenderer, GlesTarget, GlesTexProgram, GlesTexture, Uniform,
            UniformName, UniformType,
        },
        sync::SyncPoint,
        Bind, Color32F, ContextId, Frame, Offscreen, Renderer,
    },
};
use smithay::utils::{Logical, Physical, Point, Rectangle, Size, Transform};

pub(crate) const MAX_OUTPUTS: usize = 8;
const MAX_DIMENSION: i32 = 8192;
const MAX_OUTPUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

// The definitions marker and all variants match Smithay's pinned GLES texture
// shader contract, including external/no-alpha/debug variants. Only the final
// opaque scene texture uses this program and its RGB uniform.
const SHADER: &str = r#"#version 100
//_DEFINES_
#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif
precision mediump float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif
uniform float alpha;
uniform vec3 rgb_scale;
varying vec2 v_coords;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif
void main() {
    vec4 color = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    color.a = 1.0;
#endif
    color = vec4(color.rgb * rgb_scale, color.a) * alpha;
#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif
    gl_FragColor = color;
}
"#;

pub(crate) fn allocation_bytes(size: Size<i32, Physical>) -> Option<u64> {
    if size.w <= 0 || size.h <= 0 || size.w > MAX_DIMENSION || size.h > MAX_DIMENSION {
        return None;
    }
    let bytes = u64::try_from(size.w)
        .ok()?
        .checked_mul(u64::try_from(size.h).ok()?)?
        .checked_mul(4)?;
    (bytes <= MAX_OUTPUT_BYTES).then_some(bytes)
}

pub(crate) fn outputs_fit(sizes: &[Size<i32, Physical>]) -> bool {
    !sizes.is_empty()
        && sizes.len() <= MAX_OUTPUTS
        && sizes
            .iter()
            .try_fold(0_u64, |total, size| {
                total.checked_add(allocation_bytes(*size)?)
            })
            .is_some_and(|bytes| bytes <= MAX_TOTAL_BYTES)
}

pub(crate) fn valid_rgb(rgb: [f32; 3]) -> bool {
    rgb.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v))
}

pub(crate) struct Submitted {
    pub sync: SyncPoint,
    pub warmed: bool,
}

enum AttemptError<E> {
    Display(GlesError),
    Scene(E),
}

fn classify_attempt<T, E>(attempt: Result<T, AttemptError<E>>) -> Result<Option<T>, E> {
    match attempt {
        Ok(value) => Ok(Some(value)),
        Err(AttemptError::Display(_error)) => Ok(None),
        Err(AttemptError::Scene(original)) => Err(original),
    }
}

// Existing native arrow geometry, shared with nested software transformation.
// This preserves Roost's fixed-arrow policy; it does not add client cursor shapes.
pub(crate) fn cursor_rects(
    pos: Point<f64, Logical>,
    output_loc: (i32, i32),
    scale: f64,
) -> (Vec<Rectangle<i32, Physical>>, Vec<Rectangle<i32, Physical>>) {
    let x = ((pos.x - f64::from(output_loc.0)) * scale).round() as i32;
    let y = ((pos.y - f64::from(output_loc.1)) * scale).round() as i32;
    // The arrow grows by whole pixels with the scale, staying crisp.
    let k = (scale.round() as i32).max(1);
    let rect = |dx: i32, dy: i32, w: i32| -> Rectangle<i32, Physical> {
        Rectangle::new((x + dx * k, y + dy * k).into(), (w * k, k).into())
    };
    let mut outline = Vec::new();
    let mut fill = Vec::new();
    // Left-aligned triangle, 12 rows tall, plus a short tail.
    for row in 0..12 {
        outline.push(rect(0, row, row + 2));
        if row > 0 && row < 11 {
            fill.push(rect(1, row, row));
        }
    }
    for row in 12..17 {
        outline.push(rect(4, row, 4));
        fill.push(rect(5, row, 2));
    }
    (outline, fill)
}

#[derive(Default)]
pub(crate) struct Stage {
    context: Option<ContextId<GlesTexture>>,
    size: Option<Size<i32, Physical>>,
    texture: Option<GlesTexture>,
    program: Option<GlesTexProgram>,
    // Also changes on VT/S3 invalidation, even when the temperature is static.
    generation: u64,
    failed: bool,
    allocations: u64,
    failures: u64,
    #[cfg(feature = "night-light-vm-fixture")]
    fixture: Option<std::sync::Arc<crate::night_light_fixture::Fault>>,
}

impl Stage {
    pub(crate) fn reset(&mut self) {
        self.context = None;
        self.size = None;
        self.texture = None;
        self.program = None;
        self.failed = false;
        self.generation = self.generation.wrapping_add(1);
    }

    pub(crate) fn counters(&self) -> (u64, u64) {
        (self.allocations, self.failures)
    }

    #[cfg(feature = "night-light-vm-fixture")]
    pub(crate) fn set_fixture(
        &mut self,
        fixture: std::sync::Arc<crate::night_light_fixture::Fault>,
    ) {
        self.fixture = Some(fixture);
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn has_resources(&self) -> bool {
        self.context.is_some() || self.texture.is_some() || self.program.is_some()
    }

    pub(crate) fn matches(&self, renderer: &GlesRenderer, size: Size<i32, Physical>) -> bool {
        self.context.as_ref() == Some(&renderer.context_id()) && self.size == Some(size)
    }

    pub(crate) fn prepare(
        &mut self,
        renderer: &mut GlesRenderer,
        size: Size<i32, Physical>,
    ) -> Result<(), GlesError> {
        let context = renderer.context_id();
        if self.context.as_ref() != Some(&context) || self.size != Some(size) {
            self.reset();
            self.context = Some(context);
            self.size = Some(size);
        }
        if self.failed || allocation_bytes(size).is_none() {
            return Err(GlesError::FramebufferBindingError);
        }
        if self.texture.is_some() && self.program.is_some() {
            return Ok(());
        }
        self.allocations = self.allocations.wrapping_add(1);
        let initialized = (|| {
            // Texture/program drops use Smithay's deferred destruction queue.
            // Retire old stages before admitting the new topology's textures.
            renderer.cleanup_texture_cache()?;
            let program = renderer.compile_custom_texture_shader(
                SHADER,
                &[UniformName::new("rgb_scale", UniformType::_3f)],
            )?;
            let mut texture: GlesTexture =
                renderer.create_buffer(Fourcc::Abgr8888, (size.w, size.h).into())?;
            // create_buffer can defer GL allocation failure: require a complete
            // real framebuffer binding before claiming renderer capability.
            let target = renderer.bind(&mut texture)?;
            drop(target);
            Ok((program, texture))
        })();
        match initialized {
            Ok((program, texture)) => {
                self.program = Some(program);
                self.texture = Some(texture);
                Ok(())
            }
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }

    fn fail(&mut self) {
        self.failures = self.failures.wrapping_add(1);
        self.failed = true;
        self.texture = None;
        self.program = None;
        self.generation = self.generation.wrapping_add(1);
    }

    /// Optional display-pass failures latch unsupported and repaint the actual
    /// target fully in neutral. Original scene failures and failures rendering
    /// that neutral target keep the existing fatal error contract.
    // Fixed renderer contract carries target geometry plus scene/error/failure
    // callbacks; grouping these would hide their distinct recovery authority.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render<E>(
        &mut self,
        renderer: &mut GlesRenderer,
        target: &mut GlesTarget<'_>,
        size: Size<i32, Physical>,
        transform: Transform,
        rgb: [f32; 3],
        mut draw: impl FnMut(&mut GlesFrame<'_, '_>, bool) -> Result<(), E>,
        error: impl Fn(GlesError) -> E,
        unavailable: impl Fn(),
    ) -> Result<Submitted, E> {
        if rgb != [1.0; 3] {
            let attempt = (|| {
                if !valid_rgb(rgb)
                    || self.failed
                    || self.texture.is_none()
                    || self.program.is_none()
                {
                    return Err(AttemptError::Display(GlesError::FramebufferBindingError));
                }
                let texture = self
                    .texture
                    .as_mut()
                    .ok_or(AttemptError::Display(GlesError::FramebufferBindingError))?;
                {
                    let mut intermediate = renderer.bind(texture).map_err(AttemptError::Display)?;
                    let mut frame = renderer
                        .render(&mut intermediate, size, Transform::Normal)
                        .map_err(AttemptError::Display)?;
                    draw(&mut frame, true).map_err(AttemptError::Scene)?;
                    // Same-context command order carries the intermediate into
                    // final sampling. Only final finish supplies scanout sync.
                    let _ = frame.finish().map_err(AttemptError::Display)?;
                }
                #[cfg(feature = "night-light-vm-fixture")]
                if self.fixture.as_ref().is_some_and(|fault| fault.consume()) {
                    return Err(AttemptError::Display(GlesError::FramebufferBindingError));
                }
                let full = Rectangle::from_size(size);
                let mut frame = renderer
                    .render(target, size, transform)
                    .map_err(AttemptError::Display)?;
                frame
                    .clear(Color32F::BLACK, &[full])
                    .map_err(AttemptError::Display)?;
                frame
                    .render_texture_from_to(
                        texture,
                        Rectangle::from_size((f64::from(size.w), f64::from(size.h)).into()),
                        full,
                        &[full],
                        &[full],
                        Transform::Normal,
                        1.0,
                        self.program.as_ref(),
                        &[Uniform::new("rgb_scale", rgb)],
                    )
                    .map_err(AttemptError::Display)?;
                frame.finish().map_err(AttemptError::Display)
            })();
            match classify_attempt(attempt)? {
                Some(sync) => return Ok(Submitted { sync, warmed: true }),
                None => {
                    self.fail();
                    unavailable();
                    eprintln!("roost-compositor: night light: display pass unavailable; repainting neutral output");
                }
            }
        }
        let mut frame = renderer.render(target, size, transform).map_err(&error)?;
        draw(&mut frame, false)?;
        Ok(Submitted {
            sync: frame.finish().map_err(error)?,
            warmed: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounds_cover_two_4k_outputs_and_reject_unbounded_or_overbudget_topology() {
        assert!(outputs_fit(&[(3840, 2160).into(), (3840, 2160).into()]));
        for sizes in [
            vec![],
            vec![(0, 800).into()],
            vec![(8193, 1).into()],
            vec![(4096, 4096).into(); 5],
            vec![(1280, 800).into(); MAX_OUTPUTS + 1],
        ] {
            assert!(!outputs_fit(&sizes));
        }
    }
    #[test]
    fn only_finite_bounded_whitepoint_scales_are_submitted() {
        assert!(valid_rgb([1.0, 0.6, 0.2]));
        assert!(!valid_rgb([f32::NAN, 1.0, 1.0]));
        assert!(!valid_rgb([1.01, 1.0, 1.0]));
        assert!(!valid_rgb([-0.1, 1.0, 1.0]));
    }
    #[test]
    fn optional_stage_failure_latches_until_explicit_context_or_wake_reset() {
        let mut stage = Stage::default();
        let before = stage.generation();
        stage.fail();
        assert!(stage.failed);
        assert!(stage.texture.is_none() && stage.program.is_none());
        assert_ne!(stage.generation(), before);
        stage.reset();
        assert!(!stage.failed);
    }

    #[test]
    fn stage_only_recovery_preserves_original_scene_failure_and_success() {
        let original = Box::new(42);
        let address = (&*original) as *const i32;
        let failed: Result<(), AttemptError<Box<i32>>> = Err(AttemptError::Scene(original));
        let retained = classify_attempt(failed).unwrap_err();
        assert_eq!((&*retained) as *const i32, address);
        let optional: Result<i32, AttemptError<()>> =
            Err(AttemptError::Display(GlesError::FramebufferBindingError));
        assert_eq!(classify_attempt(optional), Ok(None));
        assert_eq!(classify_attempt::<_, ()>(Ok(7)), Ok(Some(7)));
    }

    #[test]
    fn shared_software_cursor_uses_actual_output_origin_and_scale() {
        let (outline, fill) = cursor_rects((1930.0, 5.0).into(), (1920, 0), 2.0);
        assert_eq!(outline[0].loc, (20, 10).into());
        assert_eq!(outline[0].size, (4, 2).into());
        assert!(!fill.is_empty());
        let (nested, _) = cursor_rects((10.0, 5.0).into(), (0, 0), 1.0);
        assert_eq!(nested[0].loc, (10, 5).into());
    }

    #[test]
    fn context_or_wake_reset_changes_damage_generation_and_drops_old_resources() {
        let mut stage = Stage::default();
        let before = stage.generation();
        stage.failed = true;
        stage.reset();
        assert_ne!(stage.generation(), before);
        assert!(!stage.failed);
        assert!(stage.texture.is_none() && stage.program.is_none() && stage.context.is_none());
    }
}
