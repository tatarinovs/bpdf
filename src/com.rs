//! RAII COM apartment manager for Windows COM and WinRT operations.

#[cfg(windows)]
mod platform {
    use anyhow::{Context, Result};
    use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

    /// An RAII guard that ensures the multithreaded COM/WinRT apartment is initialized on the current thread.
    pub struct ComApartment;

    impl ComApartment {
        pub fn initialize() -> Result<Self> {
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

#[cfg(not(windows))]
pub struct ComApartment;

#[cfg(not(windows))]
impl ComApartment {
    pub fn initialize() -> anyhow::Result<Self> {
        Ok(Self)
    }
}
