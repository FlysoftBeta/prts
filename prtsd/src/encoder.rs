use openh264::{
    OpenH264API,
    encoder::{
        BitRate, Complexity, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, Profile,
        RateControlMode, UsageType,
    },
    formats::YUVBuffer,
};

pub trait VideoEncoder {
    fn encode(
        &mut self,
        bgra: &[u8],
        width: usize,
        height: usize,
        keyframe: bool,
    ) -> anyhow::Result<Vec<u8>>;
}
pub struct Software {
    encoder: Encoder,
}
impl Software {
    pub fn new(bps: u32, fps: u32) -> anyhow::Result<Self> {
        let config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(bps))
            .max_frame_rate(FrameRate::from_hz(fps as f32))
            .usage_type(UsageType::CameraVideoRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .profile(Profile::Baseline)
            .complexity(Complexity::Low)
            .skip_frames(true)
            .intra_frame_period(IntraFramePeriod::from_num_frames(fps * 2))
            .num_threads(2);
        Ok(Self {
            encoder: Encoder::with_api_config(OpenH264API::from_source(), config)?,
        })
    }
}
impl VideoEncoder for Software {
    fn encode(&mut self, bgra: &[u8], w: usize, h: usize, key: bool) -> anyhow::Result<Vec<u8>> {
        if key {
            self.encoder.force_intra_frame();
        }
        let yuv = bgra_to_i420(bgra, w, h);
        Ok(self.encoder.encode(&yuv)?.to_vec())
    }
}
/// BT.601 limited range. Edge replication pads odd device dimensions to even I420.
pub fn bgra_to_i420(bgra: &[u8], w: usize, h: usize) -> YUVBuffer {
    let ew = (w + 1) & !1;
    let eh = (h + 1) & !1;
    let size = ew * eh;
    let mut data = vec![0; size * 3 / 2];
    let clamp = |v: i32| v.clamp(0, 255) as u8;
    for y in 0..eh {
        for x in 0..ew {
            let i = (y.min(h - 1) * w + x.min(w - 1)) * 4;
            let (b, g, r) = (bgra[i] as i32, bgra[i + 1] as i32, bgra[i + 2] as i32);
            data[y * ew + x] = clamp(((66 * r + 129 * g + 25 * b + 128) >> 8) + 16);
        }
    }
    for y in (0..eh).step_by(2) {
        for x in (0..ew).step_by(2) {
            let (mut b, mut g, mut r) = (0, 0, 0);
            for dy in 0..2 {
                for dx in 0..2 {
                    let i = ((y + dy).min(h - 1) * w + (x + dx).min(w - 1)) * 4;
                    b += bgra[i] as i32;
                    g += bgra[i + 1] as i32;
                    r += bgra[i + 2] as i32;
                }
            }
            b /= 4;
            g /= 4;
            r /= 4;
            let i = y / 2 * (ew / 2) + x / 2;
            data[size + i] = clamp(((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128);
            data[size + size / 4 + i] = clamp(((112 * r - 94 * g - 18 * b + 128) >> 8) + 128);
        }
    }
    YUVBuffer::from_vec(data, ew, eh)
}
