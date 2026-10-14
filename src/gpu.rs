//! The host's GPUs, so a bottle can ask for one to render with.

use std::path::Path;

use serde::{Deserialize, Serialize};

const NVIDIA: u16 = 0x10de;
const AMD: u16 = 0x1002;
const INTEL: u16 = 0x8086;

/// A PCI graphics card, as the kernel's DRM subsystem lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gpu {
    /// The PCI address, `0000:01:00.0`, which tells two identical cards apart.
    pub slot: String,
    pub vendor: u16,
    pub device: u16,
    /// The kernel driver: `amdgpu`, `i915`, `xe`, `nouveau`, `nvidia`...
    #[serde(default)]
    pub driver: String,
    pub name: String,
}

impl Gpu {
    /// `vvvv:dddd`, as Mesa's device selection spells a card.
    pub fn ids(&self) -> String {
        format!("{:04x}:{:04x}", self.vendor, self.device)
    }

    /// The PCI address as `DRI_PRIME` takes it: `pci-0000_01_00_0`.
    fn pci_tag(&self) -> String {
        format!("pci-{}", self.slot.replace([':', '.'], "_"))
    }

    /// Whether NVIDIA's own driver runs it, which ignores Mesa's variables.
    fn proprietary_nvidia(&self) -> bool {
        self.driver == "nvidia"
    }

    /// The variables that make Vulkan, and so Rosemary's Metal, use this
    /// card alone. Mesa's device selection layer hides every other card, so
    /// Rosemary's own ranking has nothing to choose between.
    pub fn vulkan_env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("MESA_VK_DEVICE_SELECT", self.ids()),
            ("MESA_VK_DEVICE_SELECT_FORCE_DEFAULT_DEVICE", "1".into()),
            // NVIDIA's Optimus layer, on a laptop that has one, otherwise
            // puts its own card first whatever Mesa says.
            (
                "__VK_LAYER_NV_optimus",
                match self.proprietary_nvidia() {
                    true => "NVIDIA_only",
                    false => "non_NVIDIA_only",
                }
                .into(),
            ),
        ]
    }

    /// The variables that make OpenGL render on this card: PRIME render
    /// offload, through Mesa or NVIDIA's driver.
    pub fn opengl_env(&self) -> Vec<(&'static str, String)> {
        match self.proprietary_nvidia() {
            true => vec![
                ("__NV_PRIME_RENDER_OFFLOAD", "1".into()),
                ("__GLX_VENDOR_LIBRARY_NAME", "nvidia".into()),
            ],
            false => vec![("DRI_PRIME", self.pci_tag())],
        }
    }
}

/// The GPUs in this computer, the one the firmware booted with first.
pub fn list() -> Vec<Gpu> {
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return Vec::new();
    };
    let names = PciNames::load();
    let mut cards: Vec<(bool, Gpu)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // `card1`, not its connectors (`card1-DP-2`) or render nodes.
        let is_card = name
            .strip_prefix("card")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if !is_card {
            continue;
        }
        let device = entry.path().join("device");
        let Some(gpu) = read_card(&device, &names) else {
            continue;
        };
        if cards.iter().any(|(_, g)| g.slot == gpu.slot) {
            continue;
        }
        let boot = read(&device.join("boot_vga")).is_some_and(|v| v == "1");
        cards.push((boot, gpu));
    }
    cards.sort_by(|(a, x), (b, y)| b.cmp(a).then_with(|| x.slot.cmp(&y.slot)));
    cards.into_iter().map(|(_, gpu)| gpu).collect()
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

fn hex(text: &str) -> Option<u16> {
    u16::from_str_radix(text.trim().trim_start_matches("0x"), 16).ok()
}

/// A card from its PCI device directory. Cards that are not on PCI, such as
/// the firmware's framebuffer, cannot be offloaded to and are left out.
fn read_card(device: &Path, names: &PciNames) -> Option<Gpu> {
    let uevent = read(&device.join("uevent"))?;
    let field = |key: &str| {
        uevent
            .lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
            .map(str::to_string)
    };
    let slot = field("PCI_SLOT_NAME")?;
    let vendor = hex(&read(&device.join("vendor"))?)?;
    let id = hex(&read(&device.join("device"))?)?;
    Some(Gpu {
        name: names.name(vendor, id),
        slot,
        vendor,
        device: id,
        driver: field("DRIVER").unwrap_or_default(),
    })
}

/// Names from the PCI ID database, where the system has one.
#[derive(Default)]
struct PciNames(String);

impl PciNames {
    fn load() -> PciNames {
        [
            "/usr/share/hwdata/pci.ids",
            "/usr/share/misc/pci.ids",
            "/usr/share/pci.ids",
            "/run/host/usr/share/hwdata/pci.ids",
        ]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .map(PciNames)
        .unwrap_or_default()
    }

    /// `AMD Radeon RX 6800`, or `AMD GPU 1002:73bf` with no database.
    fn name(&self, vendor: u16, device: u16) -> String {
        let short = match vendor {
            NVIDIA => Some("NVIDIA"),
            AMD => Some("AMD"),
            INTEL => Some("Intel"),
            _ => None,
        };
        let (vendor_name, model) = self.lookup(vendor, device);
        let vendor_name = short
            .map(str::to_string)
            .or(vendor_name)
            .unwrap_or_else(|| "Unknown".into());
        match model {
            // `Navi 21 [Radeon RX 6800/6800 XT / 6900 XT]`: the bracket is
            // the marketing name, the rest the chip's.
            Some(model) => {
                let model = match (model.find('['), model.rfind(']')) {
                    (Some(open), Some(close)) if open < close => &model[open + 1..close],
                    _ => model.as_str(),
                };
                format!("{vendor_name} {model}")
            }
            None => format!("{vendor_name} GPU {vendor:04x}:{device:04x}"),
        }
    }

    fn lookup(&self, vendor: u16, device: u16) -> (Option<String>, Option<String>) {
        let vendor_key = format!("{vendor:04x}");
        let device_key = format!("{device:04x}");
        let mut lines = self.0.lines();
        let Some(vendor_name) = lines.by_ref().find_map(|l| {
            l.strip_prefix(vendor_key.as_str())
                .and_then(|rest| rest.strip_prefix("  "))
                .map(str::to_string)
        }) else {
            return (None, None);
        };
        let model = lines
            .take_while(|l| l.starts_with('\t') || l.starts_with('#') || l.is_empty())
            .find_map(|l| {
                l.strip_prefix('\t')
                    .and_then(|l| l.strip_prefix(device_key.as_str()))
                    .and_then(|rest| rest.strip_prefix("  "))
                    .map(str::to_string)
            });
        (Some(vendor_name), model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDS: &str = "\
# comment
1002  Advanced Micro Devices, Inc. [AMD/ATI]
\t73bf  Navi 21 [Radeon RX 6800/6800 XT / 6900 XT]
\t\t1002 0e3a  Radeon RX 6900 XT
10de  NVIDIA Corporation
\t1f91  TU117M [GeForce GTX 1650 Mobile / Max-Q]
\t2504  GA106 [GeForce RTX 3060 Lite Hash Rate]
1234  Someone Else
\t0001  Plain Card
";

    fn gpu(vendor: u16, device: u16, driver: &str) -> Gpu {
        Gpu {
            slot: "0000:01:00.0".into(),
            vendor,
            device,
            driver: driver.into(),
            name: String::new(),
        }
    }

    #[test]
    fn names() {
        let names = PciNames(IDS.into());
        assert_eq!(
            names.name(AMD, 0x73bf),
            "AMD Radeon RX 6800/6800 XT / 6900 XT"
        );
        assert_eq!(
            names.name(NVIDIA, 0x2504),
            "NVIDIA GeForce RTX 3060 Lite Hash Rate"
        );
        assert_eq!(names.name(0x1234, 0x0001), "Someone Else Plain Card");
        // A device of the next vendor is not this one's.
        assert_eq!(names.name(AMD, 0x1f91), "AMD GPU 1002:1f91");
        assert_eq!(
            PciNames::default().name(INTEL, 0x46a6),
            "Intel GPU 8086:46a6"
        );
    }

    #[test]
    fn environment() {
        let amd = gpu(AMD, 0x73bf, "amdgpu");
        assert_eq!(amd.opengl_env(), [("DRI_PRIME", "pci-0000_01_00_0".into())]);
        assert!(
            amd.vulkan_env()
                .contains(&("MESA_VK_DEVICE_SELECT", "1002:73bf".into()))
        );
        let nvidia = gpu(NVIDIA, 0x2504, "nvidia");
        assert!(
            nvidia
                .opengl_env()
                .contains(&("__NV_PRIME_RENDER_OFFLOAD", "1".into()))
        );
        assert!(
            nvidia
                .vulkan_env()
                .contains(&("__VK_LAYER_NV_optimus", "NVIDIA_only".into()))
        );
        // Nouveau is Mesa's.
        assert_eq!(
            gpu(NVIDIA, 0x2504, "nouveau").opengl_env()[0].0,
            "DRI_PRIME"
        );
    }
}
