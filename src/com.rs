//! COM apartment initialisation for Windows COM and WinRT operations.
//!
//! The apartment is deliberately never uninitialised: `windows` caches
//! activation factories per process, and tearing the apartment down would
//! invalidate them for later calls on the same thread.

#[cfg(windows)]
mod platform {
    use anyhow::{Context, Result};
    use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

    /// Proof that the multithreaded COM/WinRT apartment is initialised on the
    /// current thread. Hold it for as long as COM objects are in use.
    pub struct ComApartment;

    impl ComApartment {
        pub fn initialize() -> Result<Self> {
            // SAFETY: Initializing COM apartment with standard multithreaded flag
            let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            if result.is_ok() || result == RPC_E_CHANGED_MODE {
                return Ok(Self);
            }
            result
                .ok()
                .context("failed to initialize COM apartment on current thread")?;
            unreachable!()
        }
    }
}

#[cfg(windows)]
pub use platform::ComApartment;
