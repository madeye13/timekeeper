extern crate libxdo_sys as libxdo;

use anyhow::bail;
use std::ffi::c_ulong;

pub struct XDoHandle {
    xdo: *mut libxdo::xdo_t,
    window: u64,
}

impl XDoHandle {
    pub fn new() -> anyhow::Result<Self> {
        let handle = unsafe { libxdo::xdo_new(std::ptr::null()) };
        if handle.is_null() {
            bail!("could not create handle");
        }
        let mut active_id: c_ulong = 0;
        unsafe {
            match libxdo::xdo_get_active_window(handle, &mut active_id) {
                0 => (),
                code => bail!("xdo failed with code: {code}"),
            }
        }
        Ok(Self { xdo: handle, window: active_id })
    }

    pub fn require_user_attention(&self, attention: i32) -> anyhow::Result<()> {
        unsafe {
            match libxdo::xdo_set_window_urgency(self.xdo, self.window, attention) {
                0 => (),
                code => bail!("xdo failed with code: {code}"),
            }
        }
        Ok(())
    }
}

impl Drop for XDoHandle {
    fn drop(&mut self) {
        unsafe {
            libxdo::xdo_free(self.xdo);
        }
    }
}
