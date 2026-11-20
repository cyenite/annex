//! Cameras for container-mode instances. Redroid has no camera HAL, so annex mounts its own
//! (`android/camera/`): a back and a front camera that film a drawn picture, chosen per instance
//! with `annex camera`, for previews, photos and frame analysis. Where that HAL can't run,
//! instances report no camera instead, as a phone without one would, so apps don't crash looking
//! for one.

use anyhow::{Context, Result, bail};
use clap::ValueEnum;

use crate::docker;
use crate::instance::{AndroidVersion, Backend, Instance};
use crate::paths::{data_dir, write_if_changed};
use crate::redroid::container_name;

const HAL: &[u8] =
    include_bytes!("../android/camera/android.hardware.camera.provider-service.annex");
const INIT: &str = include_str!("../android/camera/annex-camera.rc");
const MANIFEST: &str = include_str!("../android/camera/annex-camera.xml");
const FEATURES: &str = include_str!("../android/camera/annex-camera-features.xml");
const NO_CAMERA: &str = include_str!("../android/permissions/annex-no-camera.xml");
const DRIVER: &str = "/vendor/bin/hw/android.hardware.camera.provider-service.annex";
// The camera driver reads this each time an app opens a camera; it runs as cameraserver, which
// can open a world-readable file in /data/local/tmp but not list the folder.
const PICTURE_SETTING: &str = "/data/local/tmp/annex-camera";
// The image's default allocator in GPU mode (gbm) can only make RGB buffers, while camera
// streams need YUV and JPEG ones; minigbm, also in the image, makes all three and displays them.
const MINIGBM: &str = "ro.hardware.gralloc=cros";
// The OMX encoders run in a 32-bit process, where minigbm can't map buffers, so the screen
// stream would fail to encode; redroid's own switch moves encoding to the 64-bit Codec2 service.
const CODEC2: &str = "androidboot.use_redroid_c2=1";

/// What the cameras film. The camera driver draws each at any size, in the order listed here
/// (`Picture` in `camera.cpp`), and films the landscape for any name it doesn't know.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Picture {
    /// A landscape at dusk: soft colours, no hard edges (the default)
    Landscape,
    /// A city at night: lit windows and lane markings, sharp and high-contrast
    City,
    /// A printed page on a desk: a title, lines of text and a table, for document and text apps
    Desk,
    /// A test chart: colour bars, a focus star, a checkerboard and a grey ramp
    Chart,
}

impl Picture {
    pub fn label(self) -> &'static str {
        match self {
            Self::Landscape => "landscape",
            Self::City => "city",
            Self::Desk => "desk",
            Self::Chart => "chart",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        Self::value_variants()
            .iter()
            .copied()
            .find(|picture| picture.label() == label)
    }

    /// The one-line description `--help` shows, from the variant's doc comment.
    pub fn describe(self) -> String {
        self.to_possible_value()
            .and_then(|value| value.get_help().map(|help| help.to_string()))
            .unwrap_or_default()
    }
}

/// The built-in Camera app, hidden on instances without cameras.
fn camera_app(android: AndroidVersion) -> &'static str {
    if android.has_product_partition() {
        "/system/product/app/Camera2"
    } else {
        "/system/app/Camera2"
    }
}

/// Whether the instance has annex's cameras: Android 15 with hardware rendering, created by a
/// version of annex that has them.
pub fn has_cameras(instance: &Instance) -> bool {
    docker::succeeds(&[
        "exec",
        &container_name(&instance.name),
        "test",
        "-f",
        DRIVER,
    ])
}

pub fn picture(instance: &Instance) -> Result<Picture> {
    require_cameras(instance)?;
    let script = format!("cat {PICTURE_SETTING} 2>/dev/null; true");
    let setting = docker::run(&["exec", &container_name(&instance.name), "sh", "-c", &script])?;
    Ok(Picture::from_label(setting.trim()).unwrap_or(Picture::Landscape))
}

pub fn set_picture(instance: &Instance, picture: Picture) -> Result<()> {
    require_cameras(instance)?;
    let script = match picture {
        Picture::Landscape => format!("rm -f {PICTURE_SETTING}"),
        _ => format!("echo \"$1\" > {PICTURE_SETTING} && chmod 644 {PICTURE_SETTING}"),
    };
    let container = container_name(&instance.name);
    docker::run(&[
        "exec",
        &container,
        "sh",
        "-c",
        &script,
        "sh",
        picture.label(),
    ])
    .map(drop)
}

fn require_cameras(instance: &Instance) -> Result<()> {
    if let Backend::Waydroid { .. } = instance.backend {
        bail!(
            "{} runs in window mode, where annex doesn't simulate cameras",
            instance.name
        );
    }
    if !has_cameras(instance) {
        bail!(
            "{} has no camera: cameras need Android 15 and hardware rendering, and an instance from before they existed gets them at its next `annex up`",
            instance.name
        );
    }
    Ok(())
}

/// The `docker create` additions that give an instance its cameras, or that hide them.
pub struct Setup {
    pub mounts: Vec<String>,
    pub boot_arguments: Vec<&'static str>,
}

/// Cameras need minigbm, which needs a GPU render node (software rendering has no YUV or JPEG
/// allocator at all), and the HAL is only verified against Android 15's image.
pub fn is_supported(android: AndroidVersion, has_hardware_rendering: bool) -> bool {
    has_hardware_rendering && android == AndroidVersion::Android15
}

/// Writes the camera files under annex's data folder and returns what mounts them.
pub fn setup(android: AndroidVersion, has_hardware_rendering: bool) -> Result<Setup> {
    let is_supported = is_supported(android, has_hardware_rendering);
    let folder = data_dir().join("camera").join(env!("CARGO_PKG_VERSION"));
    let files: Vec<(&str, &[u8], u32, &str)> = if is_supported {
        vec![
            (
                "android.hardware.camera.provider-service.annex",
                HAL,
                0o755,
                "/vendor/bin/hw/android.hardware.camera.provider-service.annex",
            ),
            (
                "annex-camera.rc",
                INIT.as_bytes(),
                0o644,
                "/vendor/etc/init/annex-camera.rc",
            ),
            (
                "annex-camera.xml",
                MANIFEST.as_bytes(),
                0o644,
                "/vendor/etc/vintf/manifest/annex-camera.xml",
            ),
            (
                "annex-camera-features.xml",
                FEATURES.as_bytes(),
                0o644,
                "/vendor/etc/permissions/annex-camera-features.xml",
            ),
        ]
    } else {
        vec![(
            "annex-no-camera.xml",
            NO_CAMERA.as_bytes(),
            0o644,
            "/vendor/etc/permissions/annex-no-camera.xml",
        )]
    };
    let mut mounts = files
        .into_iter()
        .map(|(name, content, mode, target)| {
            let path = folder.join(name);
            write_if_changed(&path, content, mode)
                .with_context(|| format!("could not write {}", path.display()))?;
            Ok(format!(
                "type=bind,src={},dst={target},readonly",
                path.display()
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    if !is_supported {
        // The built-in Camera app crashes on an empty camera list, and apps hand photo
        // requests to it; hiding it lets them see there is nothing to take a photo with.
        mounts.push(format!("type=tmpfs,dst={},readonly", camera_app(android)));
    }
    let boot_arguments = if is_supported {
        vec![MINIGBM, CODEC2]
    } else {
        vec![]
    };
    Ok(Setup {
        mounts,
        boot_arguments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_driver_knows_every_picture_by_its_label() {
        let driver = include_str!("../android/camera/camera.cpp");
        for picture in Picture::value_variants() {
            assert_eq!(Picture::from_label(picture.label()), Some(*picture));
            assert!(!picture.describe().is_empty(), "{picture:?}");
            if *picture != Picture::Landscape {
                let check = format!("std::strcmp(name, \"{}\") == 0", picture.label());
                assert!(driver.contains(&check), "{check}");
            }
        }
    }

    #[test]
    fn only_android_15_with_a_gpu_has_cameras() {
        assert!(is_supported(AndroidVersion::Android15, true));
        assert!(!is_supported(AndroidVersion::Android15, false));
        assert!(!is_supported(AndroidVersion::Android14, true));
        assert!(!is_supported(AndroidVersion::Android13, true));
    }
}
