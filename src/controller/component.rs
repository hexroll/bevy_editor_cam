//! The primary [`Component`] of the controller, [`EditorCam`].

use std::time::Duration;

use bevy_ecs::prelude::*;
use bevy_log::prelude::*;
use bevy_math::{prelude::*, DVec2, DVec3};
use bevy_platform::time::Instant;
use bevy_reflect::prelude::*;
use bevy_render::prelude::*;
use bevy_time::prelude::*;
use bevy_transform::prelude::*;
use bevy_window::RequestRedraw;

use super::{
    inputs::MotionInputs,
    momentum::{Momentum, Velocity},
    motion::CurrentMotion,
    projections::OrthographicSettings,
    smoothing::{InputQueue, Smoothing},
    zoom::ZoomLimits,
};

/// Tracks all state of a camera's controller, including its inputs, motion, and settings.
///
/// See the documentation on the contained fields and types to learn more about each setting.
///
/// # Moving the Camera
///
/// The [`EditorCamPlugin`](crate::DefaultEditorCamPlugins) will automatically handle sending inputs
/// to the camera controller using [`bevy_picking`] to compute pointer hit locations for mouse,
/// touch, and pen inputs. The picking plugin allows you to specify your own picking backend, or
/// choose from a variety of provided backends. This is important because this camera controller
/// relies on depth information for each pointer, and using the picking plugin means it can do this
/// without forcing you into using a particular hit testing backend, e.g. raycasting, which is used
/// by default.
///
/// To move the camera manually:
///
/// 1. Start a camera motion using one of [`EditorCam::start_orbit`],  [`EditorCam::start_pan`],
///    [`EditorCam::start_zoom`].
/// 2. While the motion should be active, send inputs with [`EditorCam::send_screenspace_input`] and
///    [`EditorCam::send_zoom_input`].
/// 3. When the motion should end, call  [`EditorCam::end_move`].
#[derive(Debug, Clone, Reflect, Component)]
pub struct EditorCam {
    /// What input motions are currently allowed?
    pub enabled_motion: EnabledMotion,
    /// Set near and far zoom limits, as well as the ability to zoom through objects.
    pub zoom_limits: ZoomLimits,
    /// Input smoothing of camera motion.
    pub smoothing: Smoothing,
    /// Input sensitivity of the camera.
    pub sensitivity: Sensitivity,
    /// Amount of camera momentum after inputs have stopped.
    pub momentum: Momentum,
    /// How long should inputs attempting to start a new motion be ignored, after the last input
    /// ends? This is useful to prevent accidentally killing momentum when, for example, releasing a
    /// two finger right click on a trackpad triggers a scroll input.
    pub input_debounce: Duration,
    /// Settings used when the camera has an orthographic [`Projection`].
    pub orthographic: OrthographicSettings,
    /// Managed by the camera controller, though you may want to change this when spawning or
    /// manually moving the camera.
    ///
    /// If the camera starts moving, but there is nothing under the pointer, the controller will
    /// rotate, pan, and zoom about a point in the direction the camera is facing, at this depth.
    /// This will be overwritten with the latest depth if a hit is found, to ensure the anchor point
    /// doesn't change suddenly if the user moves the pointer away from an object.
    pub last_anchor_depth: f64,
    /// Current camera motion. Managed by the camera controller, but exposed publicly to allow for
    /// overriding motion.
    pub current_motion: CurrentMotion,
}

impl Default for EditorCam {
    fn default() -> Self {
        EditorCam {
            zoom_limits: Default::default(),
            smoothing: Default::default(),
            sensitivity: Default::default(),
            momentum: Default::default(),
            input_debounce: Duration::from_millis(80),
            orthographic: Default::default(),
            enabled_motion: Default::default(),
            current_motion: Default::default(),
            last_anchor_depth: -2.0,
        }
    }
}

impl EditorCam {
    /// Create a new editor camera component.
    pub fn new(
        smoothness: Smoothing,
        sensitivity: Sensitivity,
        momentum: Momentum,
        initial_anchor_depth: f64,
    ) -> Self {
        Self {
            smoothing: smoothness,
            sensitivity,
            momentum,
            last_anchor_depth: initial_anchor_depth.abs() * -1.0, // ensure depth is correct sign
            ..Default::default()
        }
    }

    /// Set the initial anchor depth of the camera controller.
    pub fn with_initial_anchor_depth(self, initial_anchor_depth: f64) -> Self {
        Self {
            last_anchor_depth: initial_anchor_depth.abs() * -1.0, // ensure depth is correct sign
            ..self
        }
    }

    /// Gets the [`MotionInputs`], if the camera is being actively moved..
    pub fn motion_inputs(&self) -> Option<&MotionInputs> {
        match &self.current_motion {
            CurrentMotion::Stationary => None,
            CurrentMotion::Momentum { .. } => None,
            CurrentMotion::UserControlled { motion_inputs, .. } => Some(motion_inputs),
        }
    }

    /// Returns the best guess at an anchor point if none is provided.
    ///
    /// Updates the fallback value with the latest hit to ensure that if the camera starts orbiting
    /// again, but has no hit to anchor onto, the anchor doesn't suddenly change distance, which is
    /// what would happen if we used a fixed value.
    fn maybe_update_anchor(&mut self, anchor: Option<DVec3>) -> DVec3 {
        let anchor = anchor.unwrap_or(DVec3::new(0.0, 0.0, self.last_anchor_depth.abs() * -1.0));
        self.last_anchor_depth = anchor.z;
        anchor
    }

    /// Get the position of the anchor in the camera's view space.
    pub fn anchor_view_space(&self) -> Option<DVec3> {
        return None;
    }

    /// Should the camera controller prevent new motions from starting, because the user is actively
    /// operating the camera?
    ///
    /// This does not consider zooming as "actively controlled". This is needed because scroll input
    /// devices often have their own momentum, and can continue to provide inputs even when the user
    /// is not actively providing inputs, like a scroll wheel that keeps spinning, or a trackpad
    /// with smooth scrolling. Without this, the controller will feel unresponsive, as a user will
    /// be unable to initiate a new motion even though they are not technically providing an input.
    pub fn is_actively_controlled(&self) -> bool {
        !self.current_motion.is_zooming_only()
            && (self.current_motion.is_user_controlled()
                || self
                    .current_motion
                    .momentum_duration()
                    .map(|duration| duration < self.input_debounce)
                    .unwrap_or(false))
    }

    /// Call this to start an panning motion with the optionally supplied anchor position in view
    /// space. See [`EditorCam`] for usage.
    pub fn start_pan(&mut self, anchor: Option<DVec3>) {
        if !self.enabled_motion.pan {
            return;
        }
        self.current_motion = CurrentMotion::UserControlled {
            anchor: self.maybe_update_anchor(anchor),
            motion_inputs: MotionInputs::PanZoom {
                screenspace_inputs: InputQueue::default(),
                zoom_inputs: InputQueue::default(),
            },
        }
    }

    /// Call this to start a zooming motion with the optionally supplied anchor position in view
    /// space. See [`EditorCam`] for usage.
    pub fn start_zoom(&mut self, anchor: Option<DVec3>) {
        if !self.enabled_motion.zoom {
            return;
        }
        let anchor = self.maybe_update_anchor(anchor);

        // Inherit current camera velocity
        let zoom_inputs = match self.current_motion {
            CurrentMotion::Stationary | CurrentMotion::Momentum { .. } => InputQueue::default(),
            CurrentMotion::UserControlled {
                ref mut motion_inputs,
                ..
            } => InputQueue(motion_inputs.zoom_inputs_mut().0.drain(..).collect()),
        };
        self.current_motion = CurrentMotion::UserControlled {
            anchor,
            motion_inputs: MotionInputs::Zoom { zoom_inputs },
        }
    }

    /// Send screen space camera inputs. This will be interpreted as panning or orbiting depending
    /// on the current motion. See [`EditorCam`] for usage.
    pub fn send_screenspace_input(&mut self, screenspace_input: Vec2) {
        if let CurrentMotion::UserControlled {
            ref mut motion_inputs,
            ..
        } = self.current_motion
        {
            match motion_inputs {
                MotionInputs::PanZoom {
                    screenspace_inputs: ref mut movement,
                    ..
                } => movement.process_input(screenspace_input, self.smoothing.pan),
                MotionInputs::Zoom { .. } => (), // When in zoom-only, we ignore pan and zoom
            }
        }
    }

    /// Send zoom inputs. See [`EditorCam`] for usage.
    pub fn send_zoom_input(&mut self, zoom_amount: f32) {
        if let CurrentMotion::UserControlled { motion_inputs, .. } = &mut self.current_motion {
            motion_inputs
                .zoom_inputs_mut()
                .process_input(zoom_amount, self.smoothing.zoom)
        }
    }

    /// End the current camera motion, allowing other motions on this camera to begin. See
    /// [`EditorCam`] for usage.
    pub fn end_move(&mut self) {
        let velocity = match self.current_motion {
            CurrentMotion::Stationary => return,
            CurrentMotion::Momentum { .. } => return,
            CurrentMotion::UserControlled {
                anchor,
                ref motion_inputs,
                ..
            } => match motion_inputs {
                MotionInputs::PanZoom { .. } => Velocity::Pan {
                    anchor,
                    velocity: motion_inputs.pan_momentum(self.momentum.init_pan),
                },
                MotionInputs::Zoom { .. } => Velocity::None,
            },
        };
        let momentum_start = Instant::now();
        self.current_motion = CurrentMotion::Momentum {
            velocity,
            momentum_start,
        };
    }

    /// Called once every frame to compute motions and update the transforms of all [`EditorCam`]s
    pub fn update_camera_positions(
        mut cameras: Query<(&mut EditorCam, &Camera, &mut Transform, &mut Projection)>,
        mut event: EventWriter<RequestRedraw>,
        time: Res<Time>,
    ) {
        for (mut camera_controller, camera, ref mut transform, ref mut projection) in
            cameras.iter_mut()
        {
            if matches!(**projection, Projection::Custom(_)) {
                warn_once!("Custom projection is not supported in editor_cam.");
                continue;
            }

            let dt = time.delta();
            camera_controller
                .update_transform_and_projection(camera, transform, projection, &mut event, dt);
        }
    }

    /// Update this [`EditorCam`]'s transform and projection.
    pub fn update_transform_and_projection(
        &mut self,
        camera: &Camera,
        cam_transform: &mut Transform,
        projection: &mut Projection,
        redraw: &mut EventWriter<RequestRedraw>,
        delta_time: Duration,
    ) {
        let (anchor, pan, zoom) = match &mut self.current_motion {
            CurrentMotion::Stationary => return,
            CurrentMotion::Momentum {
                ref mut velocity, ..
            } => {
                velocity.decay(self.momentum, delta_time);
                match velocity {
                    Velocity::None => {
                        self.current_motion = CurrentMotion::Stationary;
                        return;
                    }
                    Velocity::Pan { anchor, velocity } => (anchor, *velocity, 0.0),
                }
            }
            CurrentMotion::UserControlled {
                anchor,
                motion_inputs,
            } => (
                anchor,
                motion_inputs.smooth_pan_velocity(),
                motion_inputs.smooth_zoom_velocity() * self.sensitivity.zoom as f64,
            ),
        };

        // If there is no motion, we will have already early-exited.
        redraw.write(RequestRedraw);

        let view_offset = match projection {
            Projection::Orthographic(ortho) => DVec2::new(-ortho.scale as f64, ortho.scale as f64),
            Projection::Custom(_) => unreachable!(),
            Projection::Perspective(_) => unreachable!(),
        };

        let pan_translation_view_space = (pan * view_offset).extend(0.0);

        let size_at_anchor =
            super::zoom::length_per_pixel_at_view_space_pos(camera, *anchor).unwrap_or(0.0);

        // I'm not sure why I created this mapping - maybe it was to prevent zooming through
        // surfaces if the user really whipped the mouse:
        //
        // let zoom_unscaled = (zoom.abs() / 60.0)
        //     .powf(1.3); // Varies from 0 to 1 over x = [0..inf]
        // let zoom_input = (1.0 - 1.0 / (zoom_unscaled + 1.0)) * zoom.signum();
        //
        // It is roughly equivalent to just using
        // let zoom_input = zoom * 0.01;
        //
        // ...so I've opted to just factor this constant out of the other scaling constants below.
        //
        // I recall spending a lot of time on this mapping function, but for the life of me can't
        // remember why. Leaving this comment behind for a few releases, delete me if nothing
        // breaks.

        // The zoom input, bounded to prevent zooming past the limits.
        let zoom_bounded = if size_at_anchor <= self.zoom_limits.min_size_per_pixel {
            zoom.min(0.0) // Prevent zooming in further
        } else if size_at_anchor >= self.zoom_limits.max_size_per_pixel {
            zoom.max(0.0) // Prevent zooming out further
        } else {
            zoom
        };

        let zoom_translation_view_space = match projection {
            Projection::Perspective(_perspective) => unreachable!(),
            Projection::Orthographic(ref mut ortho) => {
                // Constants are hand tuned to feel equivalent between perspective and ortho. Might
                // be a better way to do this correctly, if it matters.
                ortho.scale *= 1.0 - zoom_bounded as f32 * 0.0015;
                // NOTE: Limit zooming so we won't flip direction
                if ortho.scale < 0.001 {
                    ortho.scale = 0.001;
                }
                // We don't move the camera in z, as this is managed by another ortho system.
                anchor.normalize()
                    * zoom_bounded
                    * anchor.z.abs()
                    * 0.0015
                    * DVec3::new(1.0, 1.0, 0.0)
            }
            Projection::Custom(_) => unreachable!(),
        };

        // If we can zoom through objects, then scoot the anchor point forward when we hit the
        // limit. This prevents the anchor from getting closer to the camera than the minimum
        // distance, or worse, zooming past the anchor.
        if self.zoom_limits.zoom_through_objects
            && size_at_anchor < self.zoom_limits.min_size_per_pixel
            && matches!(projection, Projection::Perspective(_))
            && zoom > 0.0
        {
            *anchor += zoom_translation_view_space;
        }

        cam_transform.translation += (cam_transform.rotation.as_dquat()
            * (pan_translation_view_space + zoom_translation_view_space))
            .as_vec3();

        *anchor -= pan_translation_view_space + zoom_translation_view_space;

        self.last_anchor_depth = anchor.z;
    }

    /// Compute the world space size of a pixel at the anchor.
    ///
    /// This is a robust alternative to using the distance of the camera from the anchor point.
    /// Camera distance is not directly related to how large something is on screen - that depends
    /// on the camera projection.
    ///
    /// This function correctly accounts for camera projection, and is particularly useful when
    /// doing zoom and scale calculations.
    pub fn length_per_pixel_at_anchor(&self, camera: &Camera) -> Option<f64> {
        let anchor_view = self.anchor_view_space()?;
        super::zoom::length_per_pixel_at_view_space_pos(camera, anchor_view)
    }

    /// The last known anchor depth. This value will always be negative.
    pub fn last_anchor_depth(&self) -> f64 {
        self.last_anchor_depth.abs() * -1.0
    }
}

/// The sensitivity of the camera controller to inputs.
#[derive(Debug, Clone, Copy, Reflect)]
pub struct Sensitivity {
    /// Sensitivity of zoom inputs, multiplied.
    pub zoom: f32,
}

impl Default for Sensitivity {
    fn default() -> Self {
        Self { zoom: 1.0 }
    }
}

/// Controls what kinds of motions are allowed to initiate. Does not affect momentum.
#[derive(Debug, Clone, Reflect)]
pub struct EnabledMotion {
    /// Should pan be enabled?
    pub pan: bool,
    /// Should orbit be enabled?
    pub orbit: bool,
    /// Should zoom be enabled?
    pub zoom: bool,
}

impl Default for EnabledMotion {
    fn default() -> Self {
        Self {
            pan: true,
            orbit: true,
            zoom: true,
        }
    }
}
