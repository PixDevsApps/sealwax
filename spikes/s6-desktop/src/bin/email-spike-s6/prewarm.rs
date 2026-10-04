//! A startup experiment: initialize the Vulkan driver on a side thread while the GTK thread runs
//! GTK and libadwaita startup, so GTK's own renderer setup at `realize` finds the driver warm.
//! The thread creates and destroys its own instance and device; it shares nothing with GTK.

use ash::vk;
use email_spike_s6::clock::mono_ns;
use email_spike_s6::events::emit;
use serde_json::json;

pub fn start() {
    let spawned = std::thread::Builder::new()
        .name("s6-prewarm".into())
        .spawn(|| {
            let started = mono_ns();
            let result = warm();
            let finished = mono_ns();
            match result {
                Ok(stamps) => emit(
                    "prewarm",
                    json!({
                        "started": started,
                        "loaded": stamps[0],
                        "instance": stamps[1],
                        "device": stamps[2],
                        "finished": finished,
                    }),
                ),
                Err(error) => emit("prewarm-failed", json!({ "error": error })),
            }
        });
    if let Err(error) = spawned {
        emit("prewarm-failed", json!({ "error": error.to_string() }));
    }
}

fn warm() -> Result<[i64; 3], String> {
    // SAFETY: loads the system Vulkan loader; the entry outlives every call made through it.
    let entry = unsafe { ash::Entry::load() }.map_err(|error| error.to_string())?;
    let loaded = mono_ns();
    let application = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_3);
    let create = vk::InstanceCreateInfo::default().application_info(&application);
    // SAFETY: valid create info with no extensions or layers; destroyed below.
    let instance = unsafe { entry.create_instance(&create, None) }.map_err(|e| e.to_string())?;
    let created = mono_ns();
    // SAFETY: the instance is live; the device is created and destroyed before the instance.
    let device = unsafe {
        let devices = instance
            .enumerate_physical_devices()
            .map_err(|e| e.to_string())?;
        let result = match devices.first() {
            Some(physical) => {
                let priorities = [1.0f32];
                let queue = vk::DeviceQueueCreateInfo::default()
                    .queue_family_index(0)
                    .queue_priorities(&priorities);
                let info = vk::DeviceCreateInfo::default()
                    .queue_create_infos(std::slice::from_ref(&queue));
                instance
                    .create_device(*physical, &info, None)
                    .map(|device| device.destroy_device(None))
                    .map_err(|e| e.to_string())
            }
            None => Err("no Vulkan device".to_owned()),
        };
        let device = mono_ns();
        instance.destroy_instance(None);
        result.map(|()| device)
    }?;
    Ok([loaded, created, device])
}
