/// Minimal FFI bindings for libfdk-aac decoder.
///
/// Links to the system `libfdk-aac` (install `libfdk-aac-dev` on Debian/Ubuntu).
/// Only the decoder API is exposed; the encoder is not needed.
use std::os::raw::c_int;

// Opaque handle type.
pub(crate) enum AacDecoderInstance {}
pub(crate) type Handle = *mut AacDecoderInstance;

// Transport types (from aacdecoder_lib.h).
pub(crate) const TT_MP4_RAW: c_int = 0;
#[allow(dead_code)]
pub(crate) const TT_MP4_ADTS: c_int = 2;

// Error codes.
pub(crate) const AAC_DEC_OK: c_int = 0x0000;

// Stream info structure (from aacdecoder_lib.h).
// Only the fields we need are included; the rest are padding.
#[repr(C)]
pub(crate) struct CStreamInfo {
    pub sample_rate: c_int,
    pub frame_size: c_int,
    pub num_channels: c_int,
    // There are more fields but we only read these three.
    _padding: [u8; 256],
}

extern "C" {
    pub(crate) fn aacDecoder_Open(transport_fmt: c_int, nr_of_layers: u32) -> Handle;
    pub(crate) fn aacDecoder_Close(handle: Handle);

    pub(crate) fn aacDecoder_Fill(
        handle: Handle,
        p_buffer: *mut *const u8,
        buffer_size: *const u32,
        bytes_valid: *mut u32,
    ) -> c_int;

    pub(crate) fn aacDecoder_DecodeFrame(
        handle: Handle,
        p_time_data: *mut i16,
        time_data_size: c_int,
        flags: u32,
    ) -> c_int;

    pub(crate) fn aacDecoder_GetStreamInfo(handle: Handle) -> *const CStreamInfo;

    pub(crate) fn aacDecoder_ConfigRaw(
        handle: Handle,
        conf: *mut *const u8,
        length: *const u32,
    ) -> c_int;
}

/// Safe wrapper around the fdk-aac decoder.
pub(crate) struct Decoder {
    handle: Handle,
    pub pcm_buf: Vec<i16>,
}

unsafe impl Send for Decoder {}

impl Decoder {
    /// Create a new ADTS decoder.
    #[allow(dead_code)]
    pub fn new_adts() -> Self {
        let handle = unsafe { aacDecoder_Open(TT_MP4_ADTS, 1) };
        assert!(!handle.is_null(), "aacDecoder_Open returned null");
        Decoder {
            handle,
            pcm_buf: vec![0i16; 8 * 2048],
        }
    }

    /// Create a new raw AAC decoder (requires config_raw before use).
    #[allow(dead_code)]
    pub fn new_raw() -> Self {
        let handle = unsafe { aacDecoder_Open(TT_MP4_RAW, 1) };
        assert!(!handle.is_null(), "aacDecoder_Open returned null");
        Decoder {
            handle,
            pcm_buf: vec![0i16; 8 * 2048],
        }
    }

    /// Configure a RAW decoder with an AudioSpecificConfig.
    pub fn config_raw(&mut self, asc: &[u8]) -> Result<(), c_int> {
        let mut ptr = asc.as_ptr();
        let len = asc.len() as u32;
        let err = unsafe {
            aacDecoder_ConfigRaw(self.handle, &mut ptr as *mut *const u8, &len as *const u32)
        };
        if err != AAC_DEC_OK {
            return Err(err);
        }
        Ok(())
    }

    /// Feed data into the decoder's internal buffer.
    pub fn fill(&mut self, data: &[u8]) -> Result<usize, c_int> {
        let mut ptr = data.as_ptr();
        let buf_size = data.len() as u32;
        let mut bytes_valid = buf_size;
        let err = unsafe {
            aacDecoder_Fill(
                self.handle,
                &mut ptr as *mut *const u8,
                &buf_size as *const u32,
                &mut bytes_valid as *mut u32,
            )
        };
        if err != AAC_DEC_OK {
            return Err(err);
        }
        Ok((buf_size - bytes_valid) as usize)
    }

    /// Decode one frame from the internal buffer into `self.pcm_buf`.
    ///
    /// Returns the number of output samples (channels × frame_size) on success.
    pub fn decode_frame(&mut self) -> Result<usize, c_int> {
        let err = unsafe {
            aacDecoder_DecodeFrame(
                self.handle,
                self.pcm_buf.as_mut_ptr(),
                self.pcm_buf.len() as c_int,
                0,
            )
        };
        if err != AAC_DEC_OK {
            return Err(err);
        }
        let info = self.stream_info();
        Ok((info.num_channels * info.frame_size) as usize)
    }

    /// Get stream info after a successful decode.
    pub fn stream_info(&self) -> &CStreamInfo {
        unsafe { &*aacDecoder_GetStreamInfo(self.handle) }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { aacDecoder_Close(self.handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_constructs_adts() {
        let _dec = Decoder::new_adts();
    }

    #[test]
    fn decoder_constructs_raw() {
        let _dec = Decoder::new_raw();
    }
}
