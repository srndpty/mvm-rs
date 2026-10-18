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
}

@group(0) @binding(0) var tex0: texture_2d<f32>;
@group(0) @binding(1) var tex1: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var<uniform> params: Params;

fn yuv_matrix(yy: f32, cb: f32, cr: f32) -> vec3<f32> {
    let r = yy + 1.5748 * cr;
    let g = yy - 0.187324 * cb - 0.468124 * cr;
    let b = yy + 1.8556 * cb;
    return clamp(vec3<f32>(r, g, b), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn nv12_to_rgb(y: f32, u: f32, v: f32) -> vec3<f32> {
    let yy = (y - 16.0 / 255.0) * (255.0 / 219.0);
    let cb = (u - 128.0 / 255.0) * (255.0 / 224.0);
    let cr = (v - 128.0 / 255.0) * (255.0 / 224.0);
    return yuv_matrix(yy, cb, cr);
}

fn p010_to_rgb(y: f32, u: f32, v: f32) -> vec3<f32> {
    let y10 = y * 65535.0 / 64.0;
    let u10 = u * 65535.0 / 64.0;
    let v10 = v * 65535.0 / 64.0;
    let yy = (y10 - 64.0) / 876.0;
    let cb = (u10 - 512.0) / 896.0;
    let cr = (v10 - 512.0) / 896.0;
    return yuv_matrix(yy, cb, cr);
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
        let rgb = select(nv12_to_rgb(y, chroma.r, chroma.g), p010_to_rgb(y, chroma.r, chroma.g), kind == 1u);
        straight = vec4<f32>(rgb, 1.0);
    } else {
        straight = textureSampleLevel(tex0, samp, uv, 0.0);
    }
    let alpha = straight.a * params.opacity;
    return vec4<f32>(straight.rgb * alpha, alpha);
}
