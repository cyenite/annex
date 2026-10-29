use std::fs;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GpuVendor {
    Intel,
    Amd,
    Nvidia,
    Other,
}

impl GpuVendor {
    fn from_pci_id(id: &str) -> Self {
        match id {
            "0x8086" => Self::Intel,
            "0x1002" => Self::Amd,
            "0x10de" => Self::Nvidia,
            _ => Self::Other,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Intel => "Intel",
            Self::Amd => "AMD",
            Self::Nvidia => "NVIDIA",
            Self::Other => "unknown vendor",
        }
    }

    /// Android's graphics stack needs a Mesa driver; NVIDIA's proprietary driver isn't one.
    pub fn has_mesa_driver(self) -> bool {
        matches!(self, Self::Intel | Self::Amd)
    }
}

pub struct RenderNode {
    pub name: String,
    pub vendor: GpuVendor,
}

pub fn render_nodes() -> Vec<RenderNode> {
    let Ok(entries) = fs::read_dir("/sys/class/drm") else {
        return Vec::new();
    };
    let mut nodes: Vec<RenderNode> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let vendor = fs::read_to_string(entry.path().join("device/vendor")).ok()?;
            name.starts_with("renderD").then(|| RenderNode {
                name,
                vendor: GpuVendor::from_pci_id(vendor.trim()),
            })
        })
        .collect();
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    nodes
}

pub fn has_hardware_rendering() -> bool {
    render_nodes()
        .iter()
        .any(|node| node.vendor.has_mesa_driver())
}
