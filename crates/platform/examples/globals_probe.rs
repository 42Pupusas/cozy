//! Lists every global the compositor advertises and every `wl_shm` format,
//! marking the ones section composition needs.
//!
//!   `cargo run -p platform --example globals_probe`

use platform::conn::Sender;
use platform::error::Result;
use platform::event::RawEvent;
use platform::id::{RegistryId, ShmId};
use platform::protocol::{self as proto, CallbackEvent, DisplayRequest, Interface, ShmEvent};
use platform::surface::{GlobalCollector, RegistryDiscovery, SurfaceBootstrap};
use platform::wire::MessageBuilder;
use quetzalcoatl::spsc::Consumer;

struct Advertised {
    interface: String,
    version: u32,
}

struct GlobalsProbe {
    advertised: Vec<Advertised>,
    shm: Option<ShmId>,
    shm_formats: Vec<u32>,
}

impl GlobalsProbe {
    const REQUIRED: [Interface; 3] = [
        proto::WL_SUBCOMPOSITOR,
        proto::WP_VIEWPORTER,
        proto::WP_SINGLE_PIXEL_BUFFER_MANAGER_V1,
    ];

    const fn new() -> Self {
        Self {
            advertised: Vec::new(),
            shm: None,
            shm_formats: Vec::new(),
        }
    }

    fn record(&mut self, event: &RawEvent) {
        if let Some(shm) = self.shm
            && event.is(shm)
            && event.event::<ShmEvent>() == Some(ShmEvent::Format)
        {
            self.shm_formats.push(event.arg(0));
        }
    }

    fn drain_until_sync(
        &mut self,
        sender: &mut Sender,
        events: &mut Consumer<RawEvent>,
    ) -> Result<()> {
        let callback: platform::id::CallbackId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(platform::id::DisplayId(1), DisplayRequest::Sync).new_id(callback),
        )?;
        while let Some(event) = events.pop_block() {
            if event.is(callback) && event.event::<CallbackEvent>() == Some(CallbackEvent::Done) {
                return Ok(());
            }
            self.record(&event);
        }
        Ok(())
    }

    fn report(&self) {
        println!("globals:");
        for g in &self.advertised {
            let mark = if Self::REQUIRED.iter().any(|r| r.name == g.interface) {
                "  <-- needed"
            } else {
                ""
            };
            println!("  {:<44} v{}{mark}", g.interface, g.version);
        }
        println!("\nsection composition:");
        for r in Self::REQUIRED {
            let found = self.advertised.iter().find(|g| g.interface == r.name);
            match found {
                Some(g) => println!("  ok      {} v{}", r.name, g.version),
                None => println!("  MISSING {}", r.name),
            }
        }
        println!("\nwl_shm formats:");
        for code in &self.shm_formats {
            println!("  0x{code:08x}  {}", FourCc(*code));
        }
    }
}

impl GlobalCollector for GlobalsProbe {
    fn is_complete(&self) -> bool {
        false
    }

    fn bind_global(
        &mut self,
        sender: &mut Sender,
        registry: RegistryId,
        name: u32,
        interface: &str,
        version: u32,
    ) -> Result<()> {
        if interface == proto::WL_SHM.name {
            Self::bind_if_missing(
                &mut self.shm,
                sender,
                registry,
                name,
                interface,
                version,
                proto::WL_SHM.version,
            )?;
        }
        self.advertised.push(Advertised {
            interface: interface.to_string(),
            version,
        });
        Ok(())
    }
}

struct FourCc(u32);

impl std::fmt::Display for FourCc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            0 => f.write_str("ARGB8888"),
            1 => f.write_str("XRGB8888"),
            code => {
                for b in code.to_le_bytes() {
                    let c = if b.is_ascii_graphic() { b as char } else { '?' };
                    write!(f, "{c}")?;
                }
                Ok(())
            }
        }
    }
}

struct ProbeRun;

impl ProbeRun {
    fn run() -> Result<()> {
        let wake = waker::Waker::new().map_err(platform::error::PlatformError::ReaderSpawn)?;
        let (mut sender, _timer, mut events, _reader) =
            SurfaceBootstrap::connect(256, "globals-probe", &wake)?;
        let mut probe = GlobalsProbe::new();
        let registry = sender.alloc_id::<RegistryId>();
        sender.request(
            MessageBuilder::new(platform::id::DisplayId(1), DisplayRequest::GetRegistry)
                .new_id(registry),
        )?;
        RegistryDiscovery::collect_through_sync(&mut sender, &mut events, registry, &mut probe)?;
        probe.drain_until_sync(&mut sender, &mut events)?;
        probe.report();
        Ok(())
    }
}

fn main() -> Result<()> {
    ProbeRun::run()
}
