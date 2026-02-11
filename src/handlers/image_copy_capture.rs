use smithay::delegate_image_copy_capture;
use smithay::wayland::image_capture_source::ImageCaptureSource;
use smithay::wayland::image_copy_capture::{
    BufferConstraints, Frame, ImageCopyCaptureHandler, ImageCopyCaptureState, Session, SessionRef,
};

use crate::niri::State;

impl ImageCopyCaptureHandler for State {
    fn image_copy_capture_state(&mut self) -> &mut ImageCopyCaptureState {
        &mut self.niri.image_copy_capture_state
    }

    fn capture_constraints(&mut self, source: &ImageCaptureSource) -> Option<BufferConstraints> {
        todo!()
    }

    fn cursor_capture_constraints(
        &mut self,
        source: &ImageCaptureSource,
    ) -> Option<BufferConstraints> {
        todo!()
    }

    fn new_session(&mut self, session: Session) {
        todo!()
    }

    fn frame(&mut self, session: &SessionRef, frame: Frame) {
        todo!()
    }
}

delegate_image_copy_capture!(State);
