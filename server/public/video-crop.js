// H.264 4:2:0 crops in pairs of pixels. The target metadata retains the
// unpadded pixel size, including the final row/column of an odd viewport.
export function visibleSize(videoWidth, videoHeight, size) {
  const w = Math.ceil(size.width * (size.dpr ?? 1));
  const h = Math.ceil(size.height * (size.dpr ?? 1));
  return videoWidth >= w && videoWidth <= w + 3 && videoHeight >= h && videoHeight <= h + 3
    ? [w, h] : [videoWidth, videoHeight];
}

export function cropLayout(width, height, videoWidth, videoHeight, size) {
  const [w, h] = visibleSize(videoWidth, videoHeight, size);
  const scale = Math.min(width / w, height / h);
  return { width: `${videoWidth * scale}px`, height: `${videoHeight * scale}px`,
    left: `${(width - w * scale) / 2}px`, top: `${(height - h * scale) / 2}px`,
    clipPath: `inset(0 ${(videoWidth - w) / videoWidth * 100}% ${(videoHeight - h) / videoHeight * 100}% 0)` };
}

export function installVideoCrop(video, getSize) {
  const update = () => {
    const size = getSize(), stage = video.parentElement;
    if (!size || !video.videoWidth || !stage.clientWidth || !stage.clientHeight) return;
    Object.assign(video.style, cropLayout(stage.clientWidth, stage.clientHeight, video.videoWidth, video.videoHeight, size));
  };
  video.addEventListener('resize', update);
  new ResizeObserver(update).observe(video.parentElement);
  return update;
}
