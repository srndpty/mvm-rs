struct VsOut {
    @builtin(position) pos: vec4<f32>,
}

@vertex
fn vs(@builtin(vertex_index) id: u32) -> VsOut {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var out: VsOut;
    out.pos = vec4<f32>(positions[id], 0.0, 1.0);
    return out;
}

struct Params {
    affine: vec4<f32>,
    affine_tail: vec2<f32>,
    opacity: f32,
    kind: f32,
    crop: vec4<f32>,
    src_size: vec2<f32>,
    _pad: vec2<f32>,
    // R の Cr、G の Cb、G の Cr、B の Cb。色空間ごとに CPU が選ぶ。
    yuv_mul: vec4<f32>,
    // E = sample * scale + bias。xy が輝度、zw が色差。レンジと bit 深度込み。
    yuv_range: vec4<f32>,
}

@group(0) @binding(0) var tex0: texture_2d<f32>;
@group(0) @binding(1) var tex1: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var<uniform> params: Params;

fn yuv_to_rgb(y: f32, u: f32, v: f32) -> vec3<f32> {
    let yy = y * params.yuv_range.x + params.yuv_range.y;
    let cb = u * params.yuv_range.z + params.yuv_range.w;
    let cr = v * params.yuv_range.z + params.yuv_range.w;
    let r = yy + params.yuv_mul.x * cr;
    let g = yy - params.yuv_mul.y * cb - params.yuv_mul.z * cr;
    let b = yy + params.yuv_mul.w * cb;
    return clamp(vec3<f32>(r, g, b), vec3<f32>(0.0), vec3<f32>(1.0));
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let sx = params.affine.x * pos.x + params.affine.y * pos.y + params.affine.z;
    let sy = params.affine.w * pos.x + params.affine_tail.x * pos.y + params.affine_tail.y;
    let crop = params.crop;
    if (sx < crop.x || sy < crop.y || sx >= crop.x + crop.z || sy >= crop.y + crop.w) {
        return vec4<f32>(0.0);
    }
    let uv = vec2<f32>(sx, sy) / params.src_size;
    let kind = u32(params.kind + 0.5);
    var straight = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    if (kind == 0u || kind == 1u) {
        let y = textureSampleLevel(tex0, samp, uv, 0.0).r;
        let chroma = textureSampleLevel(tex1, samp, uv, 0.0).rg;
        straight = vec4<f32>(yuv_to_rgb(y, chroma.r, chroma.g), 1.0);
    } else {
        straight = textureSampleLevel(tex0, samp, uv, 0.0);
    }
    let alpha = straight.a * params.opacity;
    return vec4<f32>(straight.rgb * alpha, alpha);
}
