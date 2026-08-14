use std::path::Path;

#[cfg(windows)]
mod platform {
    use std::os::windows::ffi::OsStrExt;

    use anyhow::{Context, Result, anyhow, bail};
    use image::{DynamicImage, RgbaImage};
    use windows::Win32::Foundation::{GENERIC_READ, RPC_E_CHANGED_MODE};
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat32bppRGBA, IWICBitmapCodecInfo,
        IWICBitmapDecoder, IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom,
        WICComponentEnumerateDefault, WICDecodeMetadataCacheOnDemand, WICDecoder,
    };
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows::core::{Interface, PCWSTR};

    use super::Path;

    struct ComApartment {
        uninitialize: bool,
    }

    impl ComApartment {
        fn initialize() -> Result<Self> {
            let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            if result.is_ok() {
                return Ok(Self { uninitialize: true });
            }
            if result == RPC_E_CHANGED_MODE {
                return Ok(Self {
                    uninitialize: false,
                });
            }
            result
                .ok()
                .context("failed to initialize COM for Windows image decoding")?;
            unreachable!()
        }
    }

    impl Drop for ComApartment {
        fn drop(&mut self) {
            if self.uninitialize {
                unsafe { CoUninitialize() };
            }
        }
    }

    pub struct Decoder {
        decoder: IWICBitmapDecoder,
        factory: IWICImagingFactory,
        _apartment: ComApartment,
    }

    impl Decoder {
        pub fn open(path: &Path) -> Result<Self> {
            let apartment = ComApartment::initialize()?;
            let factory: IWICImagingFactory =
                unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
                    .context("failed to create Windows Imaging Component factory")?;
            let wide_path = path
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect::<Vec<_>>();
            let decoder = unsafe {
                factory.CreateDecoderFromFilename(
                    PCWSTR(wide_path.as_ptr()),
                    None,
                    GENERIC_READ,
                    WICDecodeMetadataCacheOnDemand,
                )
            }
            .with_context(|| {
                format!(
                    "Windows cannot decode {}; install or update the corresponding WIC image extension",
                    path.display()
                )
            })?;
            Ok(Self {
                decoder,
                factory,
                _apartment: apartment,
            })
        }

        pub fn frame_count(&self) -> Result<u32> {
            let count = unsafe { self.decoder.GetFrameCount() }
                .context("failed to read WIC image frame count")?;
            if count == 0 {
                bail!("WIC image has no decodable frames");
            }
            Ok(count)
        }

        pub fn decode_frame(&self, index: u32) -> Result<DynamicImage> {
            let frame = unsafe { self.decoder.GetFrame(index) }
                .with_context(|| format!("failed to open WIC image frame {}", index + 1))?;
            let mut width = 0;
            let mut height = 0;
            unsafe { frame.GetSize(&mut width, &mut height) }
                .context("failed to read WIC image dimensions")?;
            if width == 0 || height == 0 {
                bail!("WIC decoder returned empty image dimensions");
            }

            let converter = unsafe { self.factory.CreateFormatConverter() }
                .context("failed to create WIC pixel converter")?;
            unsafe {
                converter.Initialize(
                    &frame,
                    &GUID_WICPixelFormat32bppRGBA,
                    WICBitmapDitherTypeNone,
                    None,
                    0.0,
                    WICBitmapPaletteTypeCustom,
                )
            }
            .context("failed to convert WIC pixels to RGBA")?;

            let stride = width
                .checked_mul(4)
                .ok_or_else(|| anyhow!("WIC image row is too large"))?;
            let length = stride
                .checked_mul(height)
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| anyhow!("WIC image is too large"))?;
            let mut pixels = vec![0; length];
            unsafe { converter.CopyPixels(std::ptr::null(), stride, &mut pixels) }
                .context("failed to copy decoded WIC pixels")?;
            let image = RgbaImage::from_raw(width, height, pixels)
                .ok_or_else(|| anyhow!("WIC decoder returned an invalid pixel buffer"))?;
            Ok(DynamicImage::ImageRgba8(image))
        }

        fn largest_frame_index(&self) -> Result<u32> {
            let mut largest = (0, 0u64);
            for index in 0..self.frame_count()? {
                let frame = unsafe { self.decoder.GetFrame(index) }?;
                let mut width = 0;
                let mut height = 0;
                unsafe { frame.GetSize(&mut width, &mut height) }?;
                let area = u64::from(width) * u64::from(height);
                if area > largest.1 {
                    largest = (index, area);
                }
            }
            Ok(largest.0)
        }
    }

    pub fn decode(path: &Path) -> Result<DynamicImage> {
        let decoder = Decoder::open(path)?;
        decoder.decode_frame(decoder.largest_frame_index()?)
    }

    pub fn availability() -> Result<String> {
        let _apartment = ComApartment::initialize()?;
        let factory: IWICImagingFactory =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
                .context("Windows Imaging Component is unavailable")?;
        let codecs = unsafe {
            factory.CreateComponentEnumerator(
                WICDecoder.0 as u32,
                WICComponentEnumerateDefault.0 as u32,
            )
        }
        .context("failed to enumerate Windows image codecs")?;
        loop {
            let mut item = [None];
            let mut fetched = 0;
            let status = unsafe { codecs.Next(&mut item, Some(&mut fetched)) };
            if fetched == 0 {
                break;
            }
            status
                .ok()
                .context("failed while enumerating Windows image codecs")?;
            let Some(item) = item[0].take() else {
                continue;
            };
            let Ok(codec) = item.cast::<IWICBitmapCodecInfo>() else {
                continue;
            };
            let extensions =
                codec_string(|buffer, actual| unsafe { codec.GetFileExtensions(buffer, actual) })?;
            if extensions
                .split(',')
                .any(|extension| extension.trim().eq_ignore_ascii_case(".dng"))
            {
                let name = codec_string(|buffer, actual| unsafe {
                    codec.GetFriendlyName(buffer, actual)
                })?;
                return Ok(format!("{name} available"));
            }
        }
        bail!(
            "Microsoft Raw Image Extension is not installed; run winget install --id 9NCTDW2W1BH8 -s msstore"
        )
    }

    fn codec_string(
        read: impl FnOnce(&mut [u16], *mut u32) -> windows::core::Result<()>,
    ) -> Result<String> {
        let mut buffer = vec![0u16; 4096];
        let mut actual = 0;
        read(&mut buffer, &mut actual).context("failed to read Windows image codec metadata")?;
        let length = usize::try_from(actual)
            .unwrap_or(buffer.len())
            .min(buffer.len());
        let length = buffer[..length]
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(length);
        Ok(String::from_utf16_lossy(&buffer[..length]))
    }
}

#[cfg(windows)]
pub use platform::{Decoder, availability, decode};

#[cfg(not(windows))]
pub fn decode(_path: &Path) -> anyhow::Result<image::DynamicImage> {
    anyhow::bail!("camera RAW input requires Windows and Microsoft Raw Image Extension")
}

#[cfg(not(windows))]
pub fn availability() -> anyhow::Result<String> {
    anyhow::bail!("camera RAW input requires Windows and Microsoft Raw Image Extension")
}
