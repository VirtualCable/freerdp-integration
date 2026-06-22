// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

pub mod audio_input;
pub mod audio_output;
pub mod clipboard;
pub mod smartcard;
pub mod webcam;

pub use audio_input::AudioInputIntegration;
pub use audio_output::AudioOutputIntegration;
pub use clipboard::{ClipboardCallback, ClipboardIntegration};
pub use smartcard::SmartcardIntegration;
pub use webcam::{WebcamFrame, WebcamIntegration, WebcamMode};

use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct RdpIntegrations {
    pub audio_output: Option<Arc<dyn AudioOutputIntegration>>,
    pub audio_input: Option<Arc<dyn AudioInputIntegration>>,
    pub webcam: Option<Arc<dyn WebcamIntegration>>,
    pub clipboard: Option<Arc<dyn ClipboardIntegration>>,
    pub smartcard: Option<Arc<dyn SmartcardIntegration>>,
}
