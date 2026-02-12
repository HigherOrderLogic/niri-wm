use std::collections::HashMap;
use std::sync::Mutex;

use smithay::backend::allocator::Fourcc;
use smithay::backend::egl::EGLDevice;
use smithay::backend::renderer::buffer_dimensions;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::{Capability, GlesRenderer};
use smithay::delegate_image_copy_capture;
use smithay::reexports::wayland_server::protocol::wl_shm::Format as ShmFormat;
use smithay::utils::{Buffer, Logical, Point, Scale, Size, Transform};
use smithay::wayland::image_capture_source::ImageCaptureSource;
use smithay::wayland::image_copy_capture::{
    BufferConstraints, CaptureFailureReason, CursorSession, CursorSessionRef, DmabufConstraints,
    Frame, ImageCopyCaptureHandler, ImageCopyCaptureState, Session, SessionRef,
};

use crate::cursor::RenderCursor;
use crate::handlers::ImageCaptureSourceKind;
use crate::niri::State;
use crate::render_helpers::surface::push_elements_from_surface_tree;

/// Per-window image copy capture tracking.
#[derive(Debug)]
pub struct WindowCaptureState {
    /// Sessions capturing this window.
    pub sessions: Vec<Session>,
    /// Cursor sessions for this window.
    pub cursor_sessions: Vec<CursorSession>,
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
                        cursor_sessions: Vec::new(),
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
        // Get the cursor size from the cursor manager
        let cursor_size = self.niri.cursor_manager.cursor_size() as i32;
        let size = Size::from((cursor_size, cursor_size));

        // Create damage tracker for cursor
        session.user_data().insert_if_missing_threadsafe(|| {
            Mutex::new(SessionUserData::new(OutputDamageTracker::new(
                size,
                1.0,
                Transform::Normal,
            )))
        });

        // Get the source kind to determine cursor position
        let Some(kind) = session
            .source()
            .user_data()
            .get::<ImageCaptureSourceKind>()
            .cloned()
        else {
            return;
        };

        // Get pointer location
        let pointer = self.niri.seat.get_pointer();
        let pointer_loc = pointer
            .as_ref()
            .map(|p| p.current_location().to_i32_round());

        // Set cursor position based on source kind
        match kind {
            ImageCaptureSourceKind::Output(weak) => {
                let Some(output) = weak.upgrade() else {
                    return;
                };

                // Check if pointer is on this output
                if let Some(pointer_loc) = pointer_loc {
                    if let Some(output_geo) = self.niri.global_space.output_geometry(&output) {
                        if output_geo.contains(pointer_loc) {
                            // Calculate cursor position in output-local coordinates
                            let local_pos = pointer_loc - output_geo.loc;
                            let buffer_pos = local_pos
                                .to_f64()
                                .to_buffer(
                                    output.current_scale().fractional_scale(),
                                    output.current_transform(),
                                    &output
                                        .current_mode()
                                        .map(|mode| {
                                            mode.size.to_f64().to_logical(
                                                output.current_scale().fractional_scale(),
                                            )
                                        })
                                        .unwrap_or_else(|| Size::from((0.0, 0.0))),
                                )
                                .to_i32_round();

                            session.set_cursor_pos(Some(buffer_pos));
                        }
                    }
                }

                // Add cursor session to output
                if let Some(state) = self.niri.output_state.get_mut(&output) {
                    state.cursor_sessions.push(session);
                }
            }
            ImageCaptureSourceKind::Window(window) => {
                // For window capture, check if cursor is over the window
                if let Some(pointer_loc) = pointer_loc {
                    let window_geo = window.geometry();
                    if window_geo.contains(pointer_loc) {
                        // Calculate cursor position relative to window
                        // Convert to buffer coordinates (for window cursor capture, use scale 1.0)
                        let relative_pos = pointer_loc.to_f64() - window_geo.loc.to_f64();
                        let buffer_pos: Point<i32, Buffer> =
                            Point::from((relative_pos.x as i32, relative_pos.y as i32));

                        session.set_cursor_pos(Some(buffer_pos));
                    }
                }

                // Store cursor session in window's user data
                window.user_data().insert_if_missing(|| {
                    Mutex::new(WindowCaptureState {
                        sessions: Vec::new(),
                        cursor_sessions: Vec::new(),
                        pending_frames: Vec::new(),
                    })
                });

                if let Some(state) = window.user_data().get::<WindowCaptureData>() {
                    if let Ok(mut state) = state.lock() {
                        state.cursor_sessions.push(session);
                    }
                }
            }
            ImageCaptureSourceKind::Destroyed => {}
        }
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

    fn cursor_frame(&mut self, session: &CursorSessionRef, frame: Frame) {
        // Get cursor info
        let cursor_scale = 1;
        let render_cursor = self.niri.cursor_manager.get_render_cursor(cursor_scale);

        // Check if cursor is visible
        if matches!(&render_cursor, RenderCursor::Hidden) {
            // Cursor is hidden, return success with empty damage
            frame.success(
                Transform::Normal,
                Vec::new(),
                crate::utils::get_monotonic_time(),
            );
            return;
        }

        // Get the buffer and verify size
        let buffer = frame.buffer();
        let cursor_size = self.niri.cursor_manager.cursor_size() as i32;
        let expected_size = Size::<i32, Buffer>::from((cursor_size, cursor_size));

        // Check buffer size matches expected cursor size
        if let Some(buffer_size) = buffer_dimensions(&buffer) {
            if buffer_size != expected_size {
                // Buffer size mismatch - update constraints and fail
                let constraints = BufferConstraints {
                    size: expected_size,
                    shm: vec![ShmFormat::Argb8888],
                    dma: None,
                };
                session.update_constraints(constraints);
                frame.fail(CaptureFailureReason::BufferConstraints);
                return;
            }
        }

        // Render the cursor
        self.backend.with_primary_renderer(|renderer| {
            let mut elements: Vec<crate::niri::OutputRenderElements<GlesRenderer>> = Vec::new();

            // Render cursor at (0, 0) since this is a cursor-only buffer
            let pos = Point::<f64, Logical>::from((0.0, 0.0));
            let scale = Scale::from(1.0);

            match render_cursor {
                RenderCursor::Hidden => unreachable!(),
                RenderCursor::Surface { surface, hotspot } => {
                    let surface_pos = (pos - hotspot.to_f64()).to_physical_precise_round(scale);
                    push_elements_from_surface_tree(
                        renderer,
                        &surface,
                        surface_pos,
                        scale,
                        1.0,
                        Kind::Cursor,
                        &mut |elem| elements.push(elem.into()),
                    );
                }
                RenderCursor::Named {
                    icon,
                    scale: cursor_scale,
                    cursor,
                } => {
                    use crate::cursor::XCursor;
                    let (_, image) =
                        cursor.frame(self.niri.start_time.elapsed().as_millis() as u32);
                    let hotspot = XCursor::hotspot(image);
                    let surface_pos = (pos.to_physical(scale) - hotspot.to_f64()).to_i32_round();

                    let texture =
                        self.niri
                            .cursor_texture_cache
                            .get(icon, cursor_scale, &cursor, 0);
                    match MemoryRenderBufferRenderElement::from_buffer(
                        renderer,
                        surface_pos,
                        &texture,
                        None,
                        None,
                        None,
                        Kind::Cursor,
                    ) {
                        Ok(element) => {
                            use smithay::backend::renderer::element::utils::{
                                Relocate, RelocateRenderElement,
                            };
                            let relocated = RelocateRenderElement::from_element(
                                element,
                                (0, 0),
                                Relocate::Relative,
                            );
                            elements.push(relocated.into());
                        }
                        Err(err) => {
                            warn!("error importing cursor texture: {err:?}");
                        }
                    }
                }
            }

            // Render to buffer
            // Convert expected_size to Physical for rendering
            let physical_size: smithay::utils::Size<i32, smithay::utils::Physical> =
                smithay::utils::Size::from((expected_size.w, expected_size.h));

            let result = if let Ok(dmabuf) = smithay::wayland::dmabuf::get_dmabuf(&buffer) {
                crate::render_helpers::render_to_dmabuf(
                    renderer,
                    dmabuf.clone(),
                    physical_size,
                    scale,
                    Transform::Normal,
                    elements.iter().rev(),
                )
                .map(|_| ()) // Ignore sync point for cursor
            } else {
                crate::render_helpers::render_to_shm(
                    renderer,
                    &buffer,
                    physical_size,
                    scale,
                    Transform::Normal,
                    elements.iter().rev(),
                )
            };

            match result {
                Ok(_) => {
                    frame.success(
                        Transform::Normal,
                        Vec::new(),
                        crate::utils::get_monotonic_time(),
                    );
                }
                Err(err) => {
                    warn!("error rendering cursor: {err:?}");
                    frame.fail(CaptureFailureReason::Unknown);
                }
            }
        });
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

    fn cursor_session_destroyed(&mut self, session: CursorSessionRef) {
        // Get the source kind
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
                        state.cursor_sessions.retain(|s| s != &session);
                    }
                }
            }
            ImageCaptureSourceKind::Window(window) => {
                // Clean up cursor session from window tracking
                if let Some(state) = window.user_data().get::<WindowCaptureData>() {
                    if let Ok(mut state) = state.lock() {
                        state.cursor_sessions.retain(|s| s != &session);
                    }
                }
            }
            ImageCaptureSourceKind::Destroyed => {}
        }
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
