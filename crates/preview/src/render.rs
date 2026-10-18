//! 計画した layer を D3D12VA と compositor で 1 枚にする。

use std::path::{Path, PathBuf};

use mvm_gpu::{Compositor, Layer, LayerParams, LayerPixels, SourceKind};
use mvm_media::{
    DecodedImage, DecodedTextureFormat, ImportedFrame, StillLimits, decode_still_image,
    open_d3d12va,
};
use mvm_project::Project;
use mvm_text::{GlyphRun, SubtitlePaint, raster, raster_subtitle};

use crate::error::PreviewError;
use crate::geometry::layer_params;
use crate::plan::{FramePlan, PlannedSource, PreviewImage, black_image, output_size};

const VIDEO_POOL: usize = 4;

/// compositor と、開き直さないための decoder。UI thread から 1 frame ずつ呼ぶ。
pub struct StillPreview {
    compositor: Compositor,
    videos: Vec<VideoSlot>,
    images: Vec<(PathBuf, DecodedImage)>,
    texts: Vec<CachedText>,
}

struct VideoSlot {
    path: PathBuf,
    decoder: mvm_media::D3d12Decoder,
}

#[derive(Clone, PartialEq)]
enum TextKey {
    Run(GlyphRun),
    Subtitle(SubtitlePaint),
}

struct CachedText {
    key: TextKey,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

struct HeldVideo {
    path: PathBuf,
    decoded: ImportedFrame,
    decoder: mvm_media::D3d12Decoder,
}

impl StillPreview {
    pub fn new() -> Result<Self, PreviewError> {
        let compositor = Compositor::new().map_err(|error| PreviewError::new(error.to_string()))?;
        Ok(Self {
            compositor,
            videos: Vec::new(),
            images: Vec::new(),
            texts: Vec::new(),
        })
    }

    /// `plan` の layer を下から合成する。返した画像を読むまで、次の `render` を呼ばなくてよい。
    pub fn render(
        &mut self,
        project: &Project,
        frame: i64,
        plan: &FramePlan,
    ) -> Result<PreviewImage, PreviewError> {
        let (width, height) = output_size(project)?;
        for layer in &plan.layers {
            if layer.source == PlannedSource::Image {
                self.ensure_image(&layer.media_path)?;
            }
        }
        let mut videos = Vec::new();
        for layer in &plan.layers {
            let PlannedSource::Video {
                frame: source_frame,
            } = layer.source
            else {
                continue;
            };
            let mut decoder = self.take_video(&layer.media_path)?;
            match decoder.decode_source_frame(source_frame) {
                Ok(decoded) => videos.push(HeldVideo {
                    path: layer.media_path.clone(),
                    decoded,
                    decoder,
                }),
                Err(error) => {
                    self.store_video(layer.media_path.clone(), decoder);
                    self.store_held(videos);
                    return Err(PreviewError::new(error.to_string()));
                }
            }
        }
        let rendered = self.composite(project, frame, plan, width, height, &videos);
        self.store_held(videos);
        if rendered.is_ok() {
            self.images.retain(|(path, _)| {
                plan.layers
                    .iter()
                    .any(|layer| layer.source == PlannedSource::Image && layer.media_path == *path)
            });
        }
        rendered
    }

    fn composite(
        &mut self,
        project: &Project,
        frame: i64,
        plan: &FramePlan,
        width: u32,
        height: u32,
        videos: &[HeldVideo],
    ) -> Result<PreviewImage, PreviewError> {
        let mut gpu_layers: Vec<Layer<'_>> = Vec::new();
        let black = [0u8, 0, 0, 255];
        let backdrop = backdrop_params();
        gpu_layers.push(Layer {
            params: backdrop,
            pixels: LayerPixels::Rgba { data: &black },
        });
        let mut rasters = Vec::new();
        for layer in &plan.layers {
            match &layer.source {
                PlannedSource::Text(run) => {
                    rasters.push(self.text_rgba(&TextKey::Run(run.clone()), width, height)?)
                }
                PlannedSource::Subtitle(paint) => rasters.push(self.text_rgba(
                    &TextKey::Subtitle(paint.clone()),
                    width,
                    height,
                )?),
                PlannedSource::Video { .. } | PlannedSource::Image => {}
            }
        }
        let mut held_videos = videos.iter();
        let mut raster_index = 0usize;
        for layer in &plan.layers {
            let (params, pixels) = match &layer.source {
                // text crate の raster は straight alpha。静止画と同じ RGBA の経路で重ねる。
                PlannedSource::Text(_) | PlannedSource::Subtitle(_) => {
                    let rgba = rasters
                        .get(raster_index)
                        .ok_or_else(|| PreviewError::new("文字画像の数が layer と一致しません"))?;
                    raster_index += 1;
                    let params = layer_params(
                        width,
                        height,
                        width,
                        height,
                        &layer.effects,
                        layer.opacity,
                        SourceKind::Rgba,
                        mvm_gpu::YuvMatrix::Bt709,
                        mvm_gpu::YuvRange::Limited,
                    )?;
                    (params, LayerPixels::Rgba { data: rgba })
                }
                PlannedSource::Video { .. } => {
                    let held = held_videos.next().ok_or_else(|| {
                        PreviewError::new("映像 frame の数が layer と一致しません")
                    })?;
                    let kind = match held.decoded.format {
                        DecodedTextureFormat::Nv12 => SourceKind::Nv12Texture,
                        DecodedTextureFormat::P010 => SourceKind::P010Texture,
                    };
                    let params = layer_params(
                        held.decoded.width,
                        held.decoded.height,
                        width,
                        height,
                        &layer.effects,
                        layer.opacity,
                        kind,
                        held.decoded.color.matrix,
                        held.decoded.color.range,
                    )?;
                    (params, video_pixels(&held.decoded))
                }
                PlannedSource::Image => {
                    let image = self.image(&layer.media_path)?;
                    let image_width = u32::try_from(image.width)
                        .map_err(|_| PreviewError::new("画像の幅が不正です"))?;
                    let image_height = u32::try_from(image.height)
                        .map_err(|_| PreviewError::new("画像の高さが不正です"))?;
                    let params = layer_params(
                        image_width,
                        image_height,
                        width,
                        height,
                        &layer.effects,
                        layer.opacity,
                        SourceKind::Rgba,
                        mvm_gpu::YuvMatrix::Bt709,
                        mvm_gpu::YuvRange::Limited,
                    )?;
                    (params, LayerPixels::Rgba { data: &image.rgba })
                }
            };
            if let Some(params) = params {
                gpu_layers.push(Layer { params, pixels });
            }
        }
        if gpu_layers.len() == 1 {
            return black_image(frame, project);
        }
        let rgba = self
            .compositor
            .composite(width, height, &gpu_layers)
            .map_err(|error| PreviewError::new(error.to_string()))?;
        Ok(PreviewImage {
            frame,
            width,
            height,
            rgba,
        })
    }

    fn text_rgba(
        &mut self,
        key: &TextKey,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, PreviewError> {
        if let Some(cached) = self
            .texts
            .iter()
            .find(|cached| cached.key == *key && cached.width == width && cached.height == height)
        {
            return Ok(cached.rgba.clone());
        }
        let rgba = match key {
            TextKey::Run(run) => raster(run, width, height),
            TextKey::Subtitle(paint) => raster_subtitle(paint, width, height),
        }
        .map_err(|error| PreviewError::new(error.to_string()))?;
        if self.texts.len() >= 8 {
            self.texts.remove(0);
        }
        self.texts.push(CachedText {
            key: key.clone(),
            width,
            height,
            rgba: rgba.clone(),
        });
        Ok(rgba)
    }

    fn ensure_image(&mut self, path: &Path) -> Result<(), PreviewError> {
        if self.images.iter().any(|(cached, _)| cached == path) {
            return Ok(());
        }
        let decoded = decode_still_image(path, &StillLimits::default())
            .map_err(|error| PreviewError::new(error.to_string()))?;
        self.images.push((path.to_path_buf(), decoded));
        Ok(())
    }

    fn image(&self, path: &Path) -> Result<&DecodedImage, PreviewError> {
        self.images
            .iter()
            .find(|(cached, _)| cached == path)
            .map(|(_, image)| image)
            .ok_or_else(|| PreviewError::new(format!("画像を読めません: {}", path.display())))
    }

    fn take_video(&mut self, path: &Path) -> Result<mvm_media::D3d12Decoder, PreviewError> {
        if let Some(index) = self.videos.iter().position(|slot| slot.path == path) {
            return Ok(self.videos.swap_remove(index).decoder);
        }
        open_d3d12va(self.compositor.device(), path)
            .map_err(|error| PreviewError::new(error.to_string()))
    }

    fn store_video(&mut self, path: PathBuf, decoder: mvm_media::D3d12Decoder) {
        if self.videos.len() >= VIDEO_POOL {
            self.videos.remove(0);
        }
        self.videos.push(VideoSlot { path, decoder });
    }

    fn store_held(&mut self, videos: Vec<HeldVideo>) {
        for held in videos {
            let HeldVideo {
                path,
                decoded,
                decoder,
            } = held;
            drop(decoded);
            self.store_video(path, decoder);
        }
    }
}

fn video_pixels(frame: &ImportedFrame) -> LayerPixels<'_> {
    match frame.format {
        DecodedTextureFormat::Nv12 => LayerPixels::Nv12Texture {
            texture: &frame.texture,
            array_layer: frame.array_layer,
        },
        DecodedTextureFormat::P010 => LayerPixels::P010Texture {
            texture: &frame.texture,
            array_layer: frame.array_layer,
        },
    }
}

fn backdrop_params() -> LayerParams {
    let mut params = LayerParams::full(SourceKind::Rgba, 1, 1);
    // どの出力 pixel も、1x1 の黒の中心を読む。
    params.affine = [0.0, 0.0, 0.5, 0.0, 0.0, 0.5];
    params.crop = [0, 0, 1, 1];
    params.opacity = 1.0;
    params
}
