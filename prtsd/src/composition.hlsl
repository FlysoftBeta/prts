cbuffer Params : register(b0) {
  float4 sizeOpacityPad;
  float4 corners;
};
Texture2D image : register(t0);
SamplerState nearest : register(s0);
struct Vertex { float4 position : SV_POSITION; float2 uv : TEXCOORD0; };
Vertex vs(uint id : SV_VertexID) {
  Vertex v;
  v.uv = float2((id << 1) & 2, id & 2);
  v.position = float4(v.uv.x * 2 - 1, 1 - v.uv.y * 2, 0, 1);
  return v;
}
float distanceToShape(float2 p) {
  float2 halfSize = sizeOpacityPad.xy / 2;
  float r = p.y < halfSize.y ? (p.x < halfSize.x ? corners.x : corners.y)
                            : (p.x < halfSize.x ? corners.w : corners.z);
  r = min(r, min(halfSize.x, halfSize.y));
  float2 q = abs(p - halfSize) - halfSize + r;
  return length(max(q, 0)) + min(max(q.x, q.y), 0) - r;
}
float4 ps(Vertex v) : SV_TARGET {
  float pad = ceil(sizeOpacityPad.w);
  float2 p = v.uv * (sizeOpacityPad.xy + 2 * pad) - pad;
  float mask = saturate(0.5 - distanceToShape(p)) * sizeOpacityPad.z;
  float4 src = 0;
  if (all(p >= 0) && all(p < sizeOpacityPad.xy))
    src = image.SampleLevel(nearest, p / sizeOpacityPad.xy, 0) * mask;
  if (sizeOpacityPad.w > 0) {
    float d = max(distanceToShape(p - float2(0, sizeOpacityPad.w / 4)), 0);
    float sigma = sizeOpacityPad.w / 3;
    float alpha = 0.35 * exp(-d * d / (2 * sigma * sigma)) * sizeOpacityPad.z;
    src.a += alpha * (1 - src.a);
  }
  return src;
}
