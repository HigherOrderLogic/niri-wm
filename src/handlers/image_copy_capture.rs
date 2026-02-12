use std::collections::HashMap;
use std::sync::Mutex;

use smithay::backend::allocator::Fourcc;
use smithay::backend::egl::EGLDevice;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::gles::{Capability, GlesRenderer};
use smithay::delegate_image_copy_capture;
use smithay::reexports::wayland_server::protocol::wl_shm::Format as ShmFormat;
use smithay::utils::{Buffer, Size, Transform};
use smithay::wayland::image_capture_source::ImageCaptureSource;
use smithay::wayland::image_copy_capture::{
    BufferConstraints, CaptureFailureReason, CursorSession, CursorSessionRef, DmabufConstraints,
    Frame, ImageCopyCaptureHandler, ImageCopyCaptureState, Session, SessionRef,
};

use crate::handlers::ImageCaptureSourceKind;
use crate::niri::State;

/// Per-window image copy capture tracking.
#[derive(Debug)]
pub struct WindowCaptureState {
    /// Sessions capturing this window.
    pub sessions: Vec<Session>,
    /// Pending frames waiting to be rendered.
    pub pending_frames: Vec<(SessionRef, Frame)>,
}

/// Thread-safe wrapper for WindowCaptureState.
pub type WindowCaptureData = Mutex<WindowCaptureState>;

/// Per-session data for image copy capture.
pub struct SessionUserData {
    pub damage_tracker: OutputDamageTracker,
}

impl SessionUserData {
    pub fn new(damage_tracker: OutputDamageTracker) -> Self {
        Self { damage_tracker }
    }
}

pub type SessionData = Mutex<SessionUserData>;

impl ImageCopyCaptureHandler for State {
    fn image_copy_capture_state(&mut self) -> &mut ImageCopyCaptureState {
        &mut self.niri.image_copy_capture_state
    }

    fn capture_constraints(&mut self, source: &ImageCaptureSource) -> Option<BufferConstraints> {
        let kind = source.user_data().get::<ImageCaptureSourceKind>()?;

        match kind {
            ImageCaptureSourceKind::Output(weak) => {
                let size = weak
                    .upgrade()?
                    .current_mode()?
                    .size
                    .to_logical(1)
                    .to_buffer(1, Transform::Normal);
                self.backend
                    .with_primary_renderer(|renderer| constraints_for_renderer(size, renderer))
            }
            ImageCaptureSourceKind::Window(window) => {
                let size = window.geometry().size.to_buffer(1, Transform::Normal);
                self.backend
                    .with_primary_renderer(|renderer| constraints_for_renderer(size, renderer))
            }
            ImageCaptureSourceKind::Destroyed => None,
        }
    }

    fn cursor_capture_constraints(
        &mut self,
        _source: &ImageCaptureSource,
    ) -> Option<BufferConstraints> {
        let cursor_size = self.niri.cursor_manager.cursor_size() as i32;
        let size = Size::from((cursor_size, cursor_size));
        self.backend
            .with_primary_renderer(|renderer| constraints_for_renderer(size, renderer))
    }

    fn new_session(&mut self, session: Session) {
        let Some(kind) = session
            .source()
            .user_data()
            .get::<ImageCaptureSourceKind>()
            .cloned()
        else {
            session.stop();
            return;
        };

        match kind {
            ImageCaptureSourceKind::Output(weak) => {
                let Some(output) = weak.upgrade() else {
                    session.stop();
                    return;
                };

                // Create damage tracker for this output
                session.user_data().insert_if_missing_threadsafe(|| {
                    Mutex::new(SessionUserData::new(OutputDamageTracker::from_output(
                        &output,
                    )))
                });

                // Add session to output state for tracking
                if let Some(state) = self.niri.output_state.get_mut(&output) {
                    state.image_copy_sessions.push(session);
                }
            }
            ImageCaptureSourceKind::Window(window) => {
                // Create damage tracker for this window
                let geometry = window.geometry();
                let size = geometry.size.to_physical_precise_round(1.0);
                session.user_data().insert_if_missing_threadsafe(|| {
                    Mutex::new(SessionUserData::new(OutputDamageTracker::new(
                        size,
                        1.0,
                        Transform::Normal,
                    )))
                });

                // Store session in the window's user data
                window.user_data().insert_if_missing(|| {
                    Mutex::new(WindowCaptureState {
                        sessions: Vec::new(),
                        pending_frames: Vec::new(),
                    })
                });

                if let Some(state) = window.user_data().get::<WindowCaptureData>() {
                    if let Ok(mut state) = state.lock() {
                        state.sessions.push(session);
                    }
                }
            }
            ImageCaptureSourceKind::Destroyed => {
                session.stop();
            }
        }
    }

    fn new_cursor_session(&mut self, session: CursorSession) {
        // Standard cursor size
        let size = Size::from((64, 64));

        session.user_data().insert_if_missing_threadsafe(|| {
            Mutex::new(SessionUserData::new(OutputDamageTracker::new(
                size,
                1.0,
                Transform::Normal,
            )))
        });
    }

    fn frame(&mut self, session: &SessionRef, frame: Frame) {
        let Some(kind) = session
            .source()
            .user_data()
            .get::<ImageCaptureSourceKind>()
            .cloned()
        else {
            frame.fail(CaptureFailureReason::Unknown);
            return;
        };

        match kind {
            ImageCaptureSourceKind::Output(weak) => {
                let Some(output) = weak.upgrade() else {
                    frame.fail(CaptureFailureReason::Unknown);
                    return;
                };

                // Queue the frame for rendering during the next output redraw
                if let Some(state) = self.niri.output_state.get_mut(&output) {
                    state
                        .pending_image_copy_frames
                        .push((session.clone(), frame));
                    // Schedule a redraw to process the frame
                    self.niri.queue_redraw(&output);
                } else {
                    frame.fail(CaptureFailureReason::Unknown);
                }
            }
            ImageCaptureSourceKind::Window(window) => {
                // Find which output this window is on
                let mut found_output = None;
                self.niri.layout.with_windows(|mapped, output, _, _| {
                    if found_output.is_none() && mapped.window == window {
                        found_output = output.cloned();
                    }
                });

                let Some(output) = found_output else {
                    frame.fail(CaptureFailureReason::Unknown);
                    return;
                };

                // Queue the frame for rendering
                if let Some(state) = window.user_data().get::<WindowCaptureData>() {
                    if let Ok(mut state) = state.lock() {
                        state.pending_frames.push((session.clone(), frame));
                        // Schedule a redraw on the output to process the frame
                        self.niri.queue_redraw(&output);
                    } else {
                        frame.fail(CaptureFailureReason::Unknown);
                    }
                } else {
                    frame.fail(CaptureFailureReason::Unknown);
                }
            }
            ImageCaptureSourceKind::Destroyed => {
                frame.fail(CaptureFailureReason::Unknown);
            }
        }
    }

    fn cursor_frame(&mut self, _session: &CursorSessionRef, frame: Frame) {
        // Cursor capture not yet implemented
        frame.fail(CaptureFailureReason::Unknown);
    }

    fn frame_aborted(&mut self, _frame: smithay::wayland::image_copy_capture::FrameRef) {
        // Frame was aborted, no action needed
    }

    fn session_destroyed(&mut self, session: SessionRef) {
        let Some(kind) = session
            .source()
            .user_data()
            .get::<ImageCaptureSourceKind>()
            .cloned()
        else {
            return;
        };

        match kind {
            ImageCaptureSourceKind::Output(weak) => {
                if let Some(output) = weak.upgrade() {
                    if let Some(state) = self.niri.output_state.get_mut(&output) {
                        state.image_copy_sessions.retain(|s| s != &session);
                        // Also remove any pending frames for this session
                        state
                            .pending_image_copy_frames
                            .retain(|(s, _)| s != &session);
                    }
                }
            }
            ImageCaptureSourceKind::Window(window) => {
                // Clean up the session from window tracking
                if let Some(state) = window.user_data().get::<WindowCaptureData>() {
                    if let Ok(mut state) = state.lock() {
                        state.sessions.retain(|s| s != &session);
                        state.pending_frames.retain(|(s, _)| s != &session);
                    }
                }
            }
            ImageCaptureSourceKind::Destroyed => {}
        }
    }

    fn cursor_session_destroyed(&mut self, _session: CursorSessionRef) {
        // Cursor session cleanup
    }
}

/// Generate buffer constraints based on renderer capabilities.
fn constraints_for_renderer(
    size: Size<i32, Buffer>,
    renderer: &mut GlesRenderer,
) -> BufferConstraints {
    // Start with basic SHM formats
    let mut shm_formats = vec![
        ShmFormat::Abgr8888,
        ShmFormat::Xbgr8888,
        ShmFormat::Argb8888,
        ShmFormat::Xrgb8888,
    ];

    // Check for 10-bit support
    if renderer.capabilities().contains(&Capability::_10Bit) {
        shm_formats.extend([ShmFormat::Abgr2101010, ShmFormat::Xbgr2101010]);
    }

    // Get DMABUF constraints if available
    let dma = EGLDevice::device_for_display(renderer.egl_context().display())
        .ok()
        .and_then(|device| device.try_get_render_node().ok().flatten())
        .map(|node| {
            let formats = renderer
                .egl_context()
                .dmabuf_render_formats()
                .iter()
                .fold(HashMap::<Fourcc, Vec<_>>::new(), |mut map, format| {
                    map.entry(format.code).or_default().push(format.modifier);
                    map
                })
                .into_iter()
                .collect();

            DmabufConstraints { node, formats }
        });

    BufferConstraints {
        size,
        shm: shm_formats,
        dma,
    }
}

// Delegate the protocol implementation to smithay
delegate_image_copy_capture!(State);
