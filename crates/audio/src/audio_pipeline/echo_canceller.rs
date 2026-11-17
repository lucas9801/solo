// SOLO: no echo cancellation. Zed's used libwebrtc's audio processing module
// for calls; Solo has no calls, and leaving it out means Solo doesn't need
// WebRTC at all. Zed already had this no-op for targets without libwebrtc.
#[derive(Clone, Default)]
pub struct EchoCanceller;

impl EchoCanceller {
    pub fn process_reverse_stream(&mut self, _buf: &mut [i16]) {}
    pub fn process_stream(&mut self, _buf: &mut [i16]) -> anyhow::Result<()> {
        Ok(())
    }
}
