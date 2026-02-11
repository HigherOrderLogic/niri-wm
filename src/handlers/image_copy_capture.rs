use std::sync::Mutex;

use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::reexports::wayland_server::protocol::wl_shm::Format as ShmFormat;
use smithay::utils::{Size, Transform};
use smithay::wayland::image_capture_source::ImageCaptureSource;
use smithay::wayland::image_copy_capture::{
    BufferConstraints, CaptureFailureReason, CursorSession, CursorSessionRef, Frame,
    ImageCopyCaptureHandler, ImageCopyCaptureState, Session, SessionRef,
};

use crate::handlers::ImageCaptureSourceKind;
use crate::niri::State;

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
                let output = weak.upgrade()?;
                let mode = output.current_mode()?;
                let size = mode.size.to_logical(1).to_buffer(1, Transform::Normal);

                // Basic SHM formats supported
                let shm_formats = vec![
                    ShmFormat::Xrgb8888,
                    ShmFormat::Argb8888,
                    ShmFormat::Abgr8888,
                    ShmFormat::Xbgr8888,
                ];

                // TODO: Add DMABUF constraints based on renderer capabilities
                let dma = None;

                Some(BufferConstraints {
                    size,
                    shm: shm_formats,
                    dma,
                })
            }
            ImageCaptureSourceKind::Window(_window) => {
                // Window capture not yet implemented
                None
            }
            ImageCaptureSourceKind::Destroyed => None,
        }
    }

    fn cursor_capture_constraints(
        &mut self,
        _source: &ImageCaptureSource,
    ) -> Option<BufferConstraints> {
        // Standard cursor size
        let size = Size::from((64, 64));

        Some(BufferConstraints {
            size,
            shm: vec![ShmFormat::Argb8888],
            dma: None,
        })
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
            ImageCaptureSourceKind::Window(_window) => {
                // Window capture not yet implemented
                session.stop();
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
            ImageCaptureSourceKind::Window(_window) => {
                // Window capture not yet implemented
                frame.fail(CaptureFailureReason::Unknown);
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
            ImageCaptureSourceKind::Window(_window) => {
                // Window session cleanup not yet implemented
            }
            ImageCaptureSourceKind::Destroyed => {}
        }
    }

    fn cursor_session_destroyed(&mut self, _session: CursorSessionRef) {
        // Cursor session cleanup
    }
}

// Delegate the protocol implementation to smithay
smithay::delegate_image_copy_capture!(State);
